//! Pure rendering: a pending-harvest record into a validated operator
//! payload (`EN.8.D` task 1).
//!
//! [`PendingHarvestRecord`] mirrors the `{artifact_id, url, payload,
//! doc_paths}` shape [`crate::nodes::harvest_gate::pending_harvest_record`]
//! constructs. [`render`] is a pure function — no I/O, no clock read, no
//! network — that turns one of those records into an
//! [`OperatorPayload`](crate::operator::OperatorPayload): an inline
//! rendered summary (the doc paths plus the payload's material content,
//! truncated to fit the declared [`OperatorPayloadLimits`]) and exactly
//! three named response options — approve / skip / open a session.
//!
//! [`render_and_validate`] runs that result through
//! [`crate::operator::validate`], the *only* constructor for
//! [`ValidatedOperatorPayload`](crate::operator::ValidatedOperatorPayload).
//! When validation fails, the caller — not this module — decides to route
//! to [`crate::operator::OperatorChannel::session`] instead (`EN.8.D` task
//! 3); this module never degrades a payload to force it through.

use serde::Deserialize;
use serde_json::Value;

use crate::operator::payload::{OperatorPayload, OperatorResponseOption};
use crate::operator::ValidatedOperatorPayload;
use crate::operator::{validate, OperatorPayloadLimits, OperatorValidationError};

/// The stable machine key for the "approve" response option.
pub const OPTION_APPROVE: &str = "approve";
/// The stable machine key for the "skip" response option.
pub const OPTION_SKIP: &str = "skip";
/// The stable machine key for the "open a session" response option.
pub const OPTION_OPEN_SESSION: &str = "open_session";

/// A pending-harvest record, typed to mirror
/// [`crate::nodes::harvest_gate::pending_harvest_record`]'s
/// `{artifact_id, url, payload, doc_paths}` JSON shape. `payload` is kept as
/// a raw [`Value`] — this module renders a summary from it but never
/// re-derives or mutates it; the stored bytes are what task 5 eventually
/// POSTs verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PendingHarvestRecord {
    /// Identity of the artifact this record defers a harvest for. The
    /// rendered [`OperatorPayload::gate_id`] derives deterministically from
    /// this field.
    pub artifact_id: String,
    /// The Synapse ingest URL the stored `payload` will eventually be
    /// POSTed to, byte-identical, by `HarvestApproveNode`.
    pub url: String,
    /// The exact JSON body an in-process push would have POSTed. Rendered
    /// into the summary but never mutated.
    pub payload: Value,
    /// Doc paths the deferred harvest touched, surfaced in the rendered
    /// summary so the operator can see what is being asked about.
    pub doc_paths: Vec<String>,
}

impl PendingHarvestRecord {
    /// Parse a [`PendingHarvestRecord`] out of the raw JSON shape
    /// [`crate::nodes::harvest_gate::pending_harvest_record`] produces.
    pub fn from_value(value: &Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

/// The gate identity [`render`] stamps into [`OperatorPayload::gate_id`] for
/// a given `artifact_id` — deterministic for the same `artifact_id`,
/// distinct across `artifact_id`s (`EN.8.D` task 1).
#[must_use]
pub fn gate_id_for(artifact_id: &str) -> String {
    format!("approve-and-run:{artifact_id}")
}

/// Truncate `s` to at most `max_chars` **characters** (not bytes), appending
/// a single-character ellipsis when truncation actually occurs so the
/// operator can see the summary was cut. Character-counted, not
/// byte-counted, so a multi-byte character is never split mid-codepoint.
fn truncate_to_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let keep = max_chars - 1;
    let mut truncated: String = s.chars().take(keep).collect();
    truncated.push('…');
    truncated
}

/// Render `record`'s doc paths plus its payload's material content into the
/// inline summary text the operator reads, truncated to fit
/// `limits.max_summary_chars`. Pure — no I/O, no clock, no network.
fn render_summary(record: &PendingHarvestRecord, limits: &OperatorPayloadLimits) -> String {
    let doc_paths_line = if record.doc_paths.is_empty() {
        String::new()
    } else {
        format!("docs: {}\n", record.doc_paths.join(", "))
    };
    let payload_line = serde_json::to_string(&record.payload).unwrap_or_default();
    let full = format!("{doc_paths_line}{payload_line}");
    truncate_to_chars(&full, limits.max_summary_chars)
}

/// The exactly-three named response options every rendered payload offers:
/// approve / skip / open a session. Stable machine keys, short
/// operator-visible labels (`EN.8.D` task 1's Invariant 1).
fn response_options() -> Vec<OperatorResponseOption> {
    vec![
        OperatorResponseOption::new(OPTION_APPROVE, "Approve"),
        OperatorResponseOption::new(OPTION_SKIP, "Skip"),
        OperatorResponseOption::new(OPTION_OPEN_SESSION, "Open Session"),
    ]
}

/// Render `record` into an (unvalidated) [`OperatorPayload`] — the inline
/// summary plus the fixed approve/skip/open-a-session option set. Pure: no
/// I/O, no clock read, no network. Callers that need the validated,
/// notification-eligible type use [`render_and_validate`].
#[must_use]
pub fn render(record: &PendingHarvestRecord, limits: &OperatorPayloadLimits) -> OperatorPayload {
    let summary = render_summary(record, limits);
    OperatorPayload::new(
        gate_id_for(&record.artifact_id),
        summary,
        response_options(),
    )
}

/// Render `record` and run the result through
/// [`crate::operator::validate`], the only constructor for
/// [`ValidatedOperatorPayload`]. `Err` means this record cannot be reduced
/// to a conforming payload under `limits` — the caller routes it to
/// [`crate::operator::OperatorChannel::session`] instead (`EN.8.D` task 3);
/// this function never degrades the payload to force it through.
pub fn render_and_validate(
    record: &PendingHarvestRecord,
    limits: &OperatorPayloadLimits,
) -> Result<ValidatedOperatorPayload, OperatorValidationError> {
    let payload = render(record, limits);
    validate(payload, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::harvest_gate::pending_harvest_record;
    use serde_json::json;

    fn record(artifact_id: &str) -> PendingHarvestRecord {
        let value = pending_harvest_record(
            artifact_id,
            "https://synapse.example/ingest/learning-artifact",
            json!({"title": "some artifact", "body": "content"}),
            vec!["docs/foo.md".to_string()],
        );
        PendingHarvestRecord::from_value(&value).expect("record parses")
    }

    #[test]
    fn conforming_record_validates() {
        let limits = OperatorPayloadLimits::default();
        let validated = render_and_validate(&record("artifact-1"), &limits)
            .expect("well-formed record should validate");
        let payload = validated.payload();
        assert_eq!(payload.gate_id, "approve-and-run:artifact-1");
        assert!(payload.rendered_summary.contains("docs/foo.md"));
        assert!(payload.digest_matches());
    }

    #[test]
    fn record_that_cannot_be_reduced_fails_validation_with_specific_error() {
        // A tiny label limit means even the shortest fixed label ("Approve",
        // 7 characters) cannot be reduced to fit — this record is not
        // reducible to a conforming payload under these limits, regardless
        // of how the summary is truncated. `validate` reports rejections in
        // declaration order, so the first option checked ("approve") is the
        // one named in the error.
        let limits = OperatorPayloadLimits {
            max_options: 3,
            min_options: 2,
            max_label_chars: 5,
            max_summary_chars: 1024,
        };
        let err = render_and_validate(&record("artifact-1"), &limits)
            .expect_err("tiny label limit should fail validation");
        assert_eq!(
            err,
            OperatorValidationError::OptionLabelTooLong {
                key: OPTION_APPROVE.to_string(),
                chars: "Approve".chars().count(),
                max: 5,
            }
        );
    }

    #[test]
    fn zero_summary_budget_fails_with_missing_summary() {
        let limits = OperatorPayloadLimits {
            max_options: 3,
            min_options: 2,
            max_label_chars: 20,
            max_summary_chars: 0,
        };
        let err = render_and_validate(&record("artifact-1"), &limits)
            .expect_err("zero summary budget should fail validation");
        assert_eq!(err, OperatorValidationError::MissingRenderedSummary);
    }

    #[test]
    fn gate_id_is_deterministic_for_same_artifact_id() {
        assert_eq!(gate_id_for("artifact-1"), gate_id_for("artifact-1"));
    }

    #[test]
    fn gate_id_is_distinct_across_artifact_ids() {
        assert_ne!(gate_id_for("artifact-1"), gate_id_for("artifact-2"));
    }

    #[test]
    fn option_keys_are_exactly_the_expected_set() {
        let payload = render(&record("artifact-1"), &OperatorPayloadLimits::default());
        let keys: Vec<&str> = payload.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, vec![OPTION_APPROVE, OPTION_SKIP, OPTION_OPEN_SESSION]);
    }

    #[test]
    fn summary_is_truncated_to_fit_max_summary_chars() {
        let big_payload = json!({"body": "x".repeat(2000)});
        let value = pending_harvest_record(
            "artifact-1",
            "https://synapse.example/ingest/learning-artifact",
            big_payload,
            vec![],
        );
        let record = PendingHarvestRecord::from_value(&value).expect("record parses");
        let limits = OperatorPayloadLimits {
            max_options: 3,
            min_options: 2,
            max_label_chars: 20,
            max_summary_chars: 100,
        };
        let payload = render(&record, &limits);
        assert!(payload.rendered_summary.chars().count() <= 100);
        assert!(payload.rendered_summary.ends_with('…'));
    }
}
