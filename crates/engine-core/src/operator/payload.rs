//! The operator-facing payload type (`EN.8.A` task 2).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md` and the contract at
//! `initial-architecture.md` §7.5, what reaches the operator is a validated
//! shape, not a convention. [`OperatorPayload`] is that shape: an inline
//! rendered summary (the diff or decision text as it will appear in the
//! channel — spec Invariant 1, "a decision, never a task"), a small fixed
//! set of named response options, a digest computed over the rendered
//! payload, and the identity of the gate that produced it.
//!
//! The rendered summary is the artifact the operator actually sees, so it is
//! a required `String` field here, not `Option<String>` — there is no
//! "payload with no summary yet" state to represent.
//!
//! The digest is computed over the *rendered* payload (the summary plus the
//! response options as they will be shown to the operator) — never over
//! whatever source object (diff, gate config, workflow state) produced them.
//! That is what makes it digest-*bound*: re-rendering the same summary and
//! options from a different source object yields the same digest, and
//! changing either the summary or an option changes it, which is what lets a
//! changed payload re-queue instead of executing (`EN.8.A` task 3 builds the
//! validation on top of this; the digest-change behavior itself is proven by
//! this module's tests).

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// One named response option offered to the operator — e.g. `("approve",
/// "Approve")`. `key` is the stable machine identifier a response resolves
/// against; `label` is the operator-visible text and is what the digest and
/// the channel's label-length limit (`EN.8.A` task 1) apply to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperatorResponseOption {
    /// Stable machine identifier for this option, e.g. `"approve"`.
    pub key: String,
    /// Operator-visible label rendered in the channel, e.g. `"Approve"`.
    pub label: String,
}

impl OperatorResponseOption {
    /// Construct a named response option.
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

/// The validated shape of "what reaches the operator" (`EN.8.A`).
///
/// `digest` is computed over `rendered_summary` and `options` at
/// construction time via [`OperatorPayload::new`] — see [`Self::recomputed_digest`]
/// to recompute it from the current fields and [`Self::digest_matches`] to
/// check the stored digest is still current. `gate_id` deliberately does
/// **not** participate in the digest: it identifies which gate produced the
/// payload, not what was rendered, and the digest is scoped to the rendered
/// artifact only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OperatorPayload {
    /// Identity of the gate that produced this payload (`EN.8.A` task 4
    /// wires this to the gate definition's declared channel).
    pub gate_id: String,
    /// The inline rendered summary — the diff or decision text exactly as it
    /// will appear in the channel. Required: there is no unrendered
    /// payload.
    pub rendered_summary: String,
    /// The small fixed set of named response options (validated against
    /// [`crate::operator::OperatorPayloadLimits`] by `EN.8.A` task 3).
    pub options: Vec<OperatorResponseOption>,
    /// Digest over `rendered_summary` + `options`, computed at construction
    /// by [`OperatorPayload::new`]. A changed rendered payload produces a
    /// different digest (see [`Self::recomputed_digest`]).
    pub digest: String,
}

impl OperatorPayload {
    /// Construct a payload, computing `digest` from `rendered_summary` and
    /// `options` so the two can never be built out of sync.
    pub fn new(
        gate_id: impl Into<String>,
        rendered_summary: impl Into<String>,
        options: Vec<OperatorResponseOption>,
    ) -> Self {
        let rendered_summary = rendered_summary.into();
        let digest = Self::digest_of(&rendered_summary, &options);
        Self {
            gate_id: gate_id.into(),
            rendered_summary,
            options,
            digest,
        }
    }

    /// Compute the digest a rendered summary + option set would carry,
    /// independent of any particular [`OperatorPayload`] instance. This is
    /// the digest — over the rendered payload, never over a source object —
    /// that [`OperatorPayload::new`] stamps into `digest` and that
    /// [`Self::recomputed_digest`] / [`Self::digest_matches`] check against.
    #[must_use]
    pub fn digest_of(rendered_summary: &str, options: &[OperatorResponseOption]) -> String {
        let mut hasher = Sha256::new();
        // Length-prefix the summary so it cannot be confused with the start
        // of the option stream (a summary ending mid-option-boundary must
        // not collide with a shorter summary plus an extra option).
        hasher.update((rendered_summary.len() as u64).to_le_bytes());
        hasher.update(rendered_summary.as_bytes());
        hasher.update((options.len() as u64).to_le_bytes());
        for opt in options {
            hasher.update((opt.key.len() as u64).to_le_bytes());
            hasher.update(opt.key.as_bytes());
            hasher.update((opt.label.len() as u64).to_le_bytes());
            hasher.update(opt.label.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Recompute the digest from this payload's current `rendered_summary`
    /// and `options`, ignoring the stored `digest` field.
    #[must_use]
    pub fn recomputed_digest(&self) -> String {
        Self::digest_of(&self.rendered_summary, &self.options)
    }

    /// Whether the stored `digest` still matches `rendered_summary` +
    /// `options`. `false` means the payload was rendered, then mutated
    /// without going back through [`OperatorPayload::new`] — the
    /// digest-bound re-queue case (`EN.8.A` task 3/5).
    #[must_use]
    pub fn digest_matches(&self) -> bool {
        self.digest == self.recomputed_digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approve_reject() -> Vec<OperatorResponseOption> {
        vec![
            OperatorResponseOption::new("approve", "Approve"),
            OperatorResponseOption::new("reject", "Reject"),
        ]
    }

    #[test]
    fn serde_round_trip_is_lossless() {
        let payload = OperatorPayload::new(
            "gate-1",
            "diff: 4 identical one-line edits",
            approve_reject(),
        );
        let json = serde_json::to_string(&payload).expect("serialize");
        let back: OperatorPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn rendered_summary_is_required_not_optional() {
        // A payload JSON with no `rendered_summary` field fails to
        // deserialize — the field is a required `String`, not
        // `Option<String>`, so there is no "missing summary" state that
        // silently round-trips as `None`.
        let json = serde_json::json!({
            "gate_id": "gate-1",
            "options": [{"key": "approve", "label": "Approve"}],
            "digest": "deadbeef",
        });
        let result: Result<OperatorPayload, _> = serde_json::from_value(json);
        assert!(
            result.is_err(),
            "expected missing rendered_summary to fail deserialization"
        );
    }

    #[test]
    fn digest_is_computed_over_rendered_payload_not_gate_id() {
        let a = OperatorPayload::new("gate-1", "same summary", approve_reject());
        let b = OperatorPayload::new("gate-2", "same summary", approve_reject());
        assert_eq!(
            a.digest, b.digest,
            "digest must depend only on the rendered summary + options, not the source gate"
        );
    }

    #[test]
    fn changed_summary_changes_digest() {
        let a = OperatorPayload::new("gate-1", "summary A", approve_reject());
        let b = OperatorPayload::new("gate-1", "summary B", approve_reject());
        assert_ne!(a.digest, b.digest);
    }

    #[test]
    fn changed_option_label_changes_digest() {
        let a = OperatorPayload::new("gate-1", "same summary", approve_reject());
        let mut changed = approve_reject();
        changed[0].label = "Approved".to_string();
        let b = OperatorPayload::new("gate-1", "same summary", changed);
        assert_ne!(a.digest, b.digest);
    }

    #[test]
    fn digest_matches_detects_post_construction_mutation() {
        let mut payload = OperatorPayload::new("gate-1", "original", approve_reject());
        assert!(payload.digest_matches());
        payload.rendered_summary = "mutated".to_string();
        assert!(!payload.digest_matches());
        assert_eq!(
            payload.recomputed_digest(),
            OperatorPayload::digest_of(&payload.rendered_summary, &payload.options)
        );
    }
}
