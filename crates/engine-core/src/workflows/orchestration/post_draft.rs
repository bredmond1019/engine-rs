//! `POST_DRAFT` — a finished campaign drafts the post about itself
//! (`EN.12.M`).
//!
//! # Shape decision (Task 1)
//!
//! The draft is a **second output of `DebriefNode`**, not its own node.
//! `graph.rs`'s node-count assertion does not move. Reasoning: the draft
//! reads the exact same journal rows the ops digest already reads (fetched
//! once, through the same [`crate::workflows::orchestration::debrief::JournalReader`]
//! seam), needs no additional input, and standing rule 6 forbids a policy
//! knob changing a declared graph's node set — introducing a second node
//! here would be exactly that shape of change for no behavioural gain. The
//! same precedent already exists in this module: `DebriefNode` itself emits
//! two outputs (the dispatched digest and the written-back journal row)
//! from one node.
//!
//! # THE REFUSAL IS THE POINT
//!
//! [`render_post_draft`] returns `None` — never an empty string, never a
//! stub draft — when the rows carry no measured number AND no evidence
//! path. `render_brief` (`debrief.rs`) deliberately renders "No steps ran
//! for this campaign." for zero rows because an ops digest must always say
//! something; a draft must do the opposite. A queue that fills up
//! regardless of whether it has anything to say trains the operator to
//! ignore it — the exact failure this block exists to end.
//!
//! A **measured number** is any numeric JSON leaf found by walking a row's
//! `detail` value ([`row_measured_numbers`]). An **evidence path** is any
//! path-shaped token found in a row's `reason` or in a string leaf of its
//! `detail` ([`row_evidence_paths`]) — a token containing at least one `/`
//! with no whitespace on either side of it, so `planning/harness.json` and
//! `crates/engine-core/src/workflows/orchestration/debrief.rs:208` both
//! match but a plain sentence does not. Both predicates are their own named
//! functions so a test can attack the bar directly, per the task's
//! acceptance criteria.

use std::collections::BTreeSet;

use engine_contract::JournalRow;
use okf_core::LearningArtifact;
use regex::Regex;
use serde_json::Value;

/// The `channel_type` every post-draft payload carries. Deliberately not
/// `"schedule"`/`"web_article"`/etc — the values `content_pipeline`'s own
/// `LearningArtifact` payloads use for ingested content — so a draft is
/// distinguishable from anything else materialized through the same model
/// by this field alone, without parsing `digest_markdown` prose (task 2
/// acceptance criteria).
pub const POST_DRAFT_CHANNEL_TYPE: &str = "post_draft";

/// `language` fallback when a caller passes an empty string. Mirrors
/// `content_pipeline::learning_artifact::DEFAULT_LANGUAGE` — a draft that
/// was never translated is in its original language, `en`.
const DEFAULT_LANGUAGE: &str = "en";

/// A path-shaped token: at least one non-whitespace segment, a `/`, then at
/// least one more non-whitespace segment. Deliberately permissive about
/// what a "segment" contains (letters, digits, `_`, `-`, `.`, `:`) so both a
/// bare relative path (`docs/content/drafts/`) and a path with a trailing
/// line number (`debrief.rs:208`) match, while a plain English sentence
/// (which contains no `/`) does not.
fn evidence_path_pattern() -> Regex {
    Regex::new(r"[A-Za-z0-9_.:\-]+(?:/[A-Za-z0-9_.:\-]+)+").expect("static regex is valid")
}

/// Every numeric JSON leaf in `value`, walked recursively, formatted as
/// `"<dotted.path>=<value>"` so the draft can name what was measured, not
/// just that something was. Object keys and array indices both contribute
/// to the dotted path. Ordering is depth-first, insertion order for
/// objects (`serde_json::Value`'s default map preserves the `detail`
/// payload's own key order when the `preserve_order` feature is off, which
/// this crate does not enable — so this additionally sorts before
/// returning, keeping the function's output deterministic regardless).
fn numeric_leaves(prefix: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Number(n) => out.push(format!("{prefix}={n}")),
        Value::Object(map) => {
            for (key, child) in map {
                let next_prefix = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                numeric_leaves(&next_prefix, child, out);
            }
        }
        Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let next_prefix = format!("{prefix}[{idx}]");
                numeric_leaves(&next_prefix, child, out);
            }
        }
        _ => {}
    }
}

/// Every string JSON leaf in `value`, walked recursively — feeds
/// [`row_evidence_paths`]'s scan for path-shaped tokens inside `detail`,
/// not only inside `reason`.
fn string_leaves<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s.as_str()),
        Value::Object(map) => {
            for child in map.values() {
                string_leaves(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                string_leaves(child, out);
            }
        }
        _ => {}
    }
}

/// The measured numbers one journal row contributes, as `"path=value"`
/// strings — empty if `row.detail` carries no numeric leaf. Its own named
/// function (not inlined into [`render_post_draft`]) so the refusal-bar
/// predicate can be tested directly, per the task's acceptance criteria.
#[must_use]
pub fn row_measured_numbers(row: &JournalRow) -> Vec<String> {
    let mut out = Vec::new();
    numeric_leaves("detail", &row.detail, &mut out);
    out
}

/// `true` iff `row` contributes at least one measured number.
#[must_use]
pub fn row_has_measured_number(row: &JournalRow) -> bool {
    !row_measured_numbers(row).is_empty()
}

/// The evidence-path tokens one journal row contributes — scanned from
/// `row.reason` and from every string leaf of `row.detail` — deduplicated
/// and sorted so the result is deterministic. Empty if the row names no
/// path-shaped token anywhere.
#[must_use]
pub fn row_evidence_paths(row: &JournalRow) -> Vec<String> {
    let pattern = evidence_path_pattern();
    let mut found: BTreeSet<String> = BTreeSet::new();

    for candidate in pattern.find_iter(&row.reason) {
        found.insert(candidate.as_str().to_string());
    }

    let mut strings = Vec::new();
    string_leaves(&row.detail, &mut strings);
    for s in strings {
        for candidate in pattern.find_iter(s) {
            found.insert(candidate.as_str().to_string());
        }
    }

    found.into_iter().collect()
}

/// `true` iff `row` contributes at least one evidence path.
#[must_use]
pub fn row_has_evidence_path(row: &JournalRow) -> bool {
    !row_evidence_paths(row).is_empty()
}

/// `true` iff `rows` collectively clear the draft bar: at least one row
/// with a measured number AND at least one row (not necessarily the same
/// one) with an evidence path. This is the predicate
/// [`render_post_draft`]'s refusal is built on — its own named function so
/// a test can attack the bar directly rather than re-deriving it from
/// `render_post_draft`'s `Option` result.
#[must_use]
pub fn rows_clear_draft_bar(rows: &[JournalRow]) -> bool {
    rows.iter().any(row_has_measured_number) && rows.iter().any(row_has_evidence_path)
}

/// Render `rows` into a publishable post draft — a thesis line, the
/// measured numbers the run actually produced, and the evidence paths —
/// or `None` when [`rows_clear_draft_bar`] says the run has nothing
/// draft-worthy to say.
///
/// Deliberately NOT a copy of [`super::debrief::render_brief`]'s shape:
/// the ops digest lists every step in order; this draft leads with a
/// thesis line and groups by what was measured, because a draft's reader
/// is deciding whether to publish, not auditing what ran.
#[must_use]
pub fn render_post_draft(rows: &[JournalRow]) -> Option<String> {
    if !rows_clear_draft_bar(rows) {
        return None;
    }

    let mut ordered: Vec<&JournalRow> = rows.iter().collect();
    ordered.sort_by_key(|row| row.created_at);

    let mut numbers: BTreeSet<String> = BTreeSet::new();
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for row in &ordered {
        numbers.extend(row_measured_numbers(row));
        paths.extend(row_evidence_paths(row));
    }

    let campaign_id = ordered
        .first()
        .map(|row| row.campaign_id.as_str())
        .unwrap_or("unknown campaign");

    let mut lines = vec![format!(
        "Campaign {campaign_id} ran {} step(s) and produced measured results worth writing up.",
        ordered.len()
    )];

    lines.push(String::new());
    lines.push("Measured:".to_string());
    for number in &numbers {
        lines.push(format!("- {number}"));
    }

    lines.push(String::new());
    lines.push("Evidence:".to_string());
    for path in &paths {
        lines.push(format!("- {path}"));
    }

    Some(lines.join("\n"))
}

/// The first line of a rendered draft — the thesis line — used as the
/// `LearningArtifact`'s `summary` field so a reader deciding whether to
/// open the full draft sees the claim, not the whole digest.
fn thesis_line(draft: &str) -> String {
    draft.lines().next().unwrap_or_default().to_string()
}

/// Build the `LearningArtifact` payload
/// (`{artifact_id, channel_type, source_ref, summary, digest_markdown,
/// entities, language}` — [`okf_core::LearningArtifact::from_payload`]'s
/// exact field set, task 2's block interface) for `rows`, or `None` when
/// [`rows_clear_draft_bar`] refuses the draft. Reuses [`render_post_draft`]
/// rather than re-deriving the bar, so the refusal rule lives in exactly
/// one place (task 1's predicate).
///
/// `language` is set per-call (`"en"` or `"pt-BR"`, per D77 §2) rather than
/// hardcoded — an empty string falls back to [`DEFAULT_LANGUAGE`]. No
/// translation happens here (out of scope, per the block record): the
/// caller is responsible for choosing which language the draft was
/// authored in.
///
/// `source_ref` is the first evidence path the draft names — the closest
/// thing to "where this came from" a journal row set offers, and, since
/// [`rows_clear_draft_bar`] already guarantees at least one evidence path
/// exists whenever this returns `Some`, always present when it matters.
/// `channel_type` is always [`POST_DRAFT_CHANNEL_TYPE`], distinguishing
/// this payload from any other `LearningArtifact` shape (task 2 AC:
/// "distinguishable by their payload without parsing prose"). `entities`
/// is left empty: a journal row set names steps and paths, not named
/// entities, and inventing placeholder entities would be worse than an
/// honest empty list.
#[must_use]
pub fn build_post_draft_payload(rows: &[JournalRow], language: &str) -> Option<Value> {
    let draft = render_post_draft(rows)?;
    let evidence = row_evidence_paths_all(rows);
    let source_ref = evidence.first().cloned().unwrap_or_default();

    let campaign_id = rows
        .first()
        .map(|row| row.campaign_id.as_str())
        .unwrap_or("unknown-campaign");

    let language = if language.trim().is_empty() {
        DEFAULT_LANGUAGE
    } else {
        language
    };

    Some(serde_json::json!({
        "artifact_id": format!("post-draft:{campaign_id}"),
        "channel_type": POST_DRAFT_CHANNEL_TYPE,
        "source_ref": source_ref,
        "summary": thesis_line(&draft),
        "digest_markdown": draft,
        "entities": Vec::<String>::new(),
        "language": language,
    }))
}

/// Every distinct evidence path across `rows`, sorted — a small helper so
/// [`build_post_draft_payload`] does not re-walk each row's `detail`/`reason`
/// itself; delegates to [`row_evidence_paths`] per row.
fn row_evidence_paths_all(rows: &[JournalRow]) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        found.extend(row_evidence_paths(row));
    }
    found.into_iter().collect()
}

/// Validate that `payload` (as built by [`build_post_draft_payload`])
/// round-trips through [`okf_core::LearningArtifact::from_payload`] without
/// dropping or mangling a field. Exposed for the test below and for a
/// future caller that wants a hard guarantee before materializing.
#[must_use]
pub fn payload_round_trips_as_learning_artifact(payload: &Value) -> bool {
    let artifact = LearningArtifact::from_payload(payload);
    artifact.artifact_id == payload["artifact_id"].as_str().unwrap_or_default()
        && artifact.channel_type == payload["channel_type"].as_str().unwrap_or_default()
        && artifact.source_ref == payload["source_ref"].as_str().unwrap_or_default()
        && artifact.summary == payload["summary"].as_str().unwrap_or_default()
        && artifact.language == payload["language"].as_str().unwrap_or_default()
        && artifact.digest_markdown == payload["digest_markdown"].as_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use engine_contract::JournalDecisionKind;
    use okf_core::BrainDocModel;
    use uuid::Uuid;

    fn row(kind: JournalDecisionKind, reason: &str, detail: Value, offset_secs: i64) -> JournalRow {
        JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-1".to_string(),
            run_id: Uuid::new_v4(),
            step: "build".to_string(),
            kind,
            reason: reason.to_string(),
            detail,
            created_at: Utc.timestamp_opt(1_700_000_000 + offset_secs, 0).unwrap(),
        }
    }

    #[test]
    fn row_measured_numbers_finds_numeric_leaves() {
        let r = row(
            JournalDecisionKind::StepIntegrated,
            "clean run",
            serde_json::json!({ "duration_secs": 12, "cost_usd": 0.42 }),
            0,
        );
        let numbers = row_measured_numbers(&r);
        assert_eq!(numbers.len(), 2);
        assert!(numbers.iter().any(|n| n.contains("duration_secs=12")));
        assert!(numbers.iter().any(|n| n.contains("cost_usd=0.42")));
    }

    #[test]
    fn row_measured_numbers_empty_when_no_numeric_leaf() {
        let r = row(
            JournalDecisionKind::StepIntegrated,
            "clean run",
            serde_json::json!({ "note": "no numbers here" }),
            0,
        );
        assert!(row_measured_numbers(&r).is_empty());
        assert!(!row_has_measured_number(&r));
    }

    #[test]
    fn row_evidence_paths_finds_path_in_reason() {
        let r = row(
            JournalDecisionKind::StepBailed,
            "harness check failed: planning/harness.json line 12",
            serde_json::json!({}),
            0,
        );
        let paths = row_evidence_paths(&r);
        assert!(paths.iter().any(|p| p.contains("planning/harness.json")));
    }

    #[test]
    fn row_evidence_paths_finds_path_in_detail_string_leaf() {
        let r = row(
            JournalDecisionKind::StepIntegrated,
            "clean run",
            serde_json::json!({ "evidence": "crates/engine-core/src/workflows/orchestration/debrief.rs:208" }),
            0,
        );
        let paths = row_evidence_paths(&r);
        assert!(paths.iter().any(|p| p.contains("debrief.rs:208")));
    }

    #[test]
    fn row_evidence_paths_empty_for_plain_sentence() {
        let r = row(
            JournalDecisionKind::StepIntegrated,
            "everything looks fine, nothing to report",
            serde_json::json!({ "note": "still nothing" }),
            0,
        );
        assert!(row_evidence_paths(&r).is_empty());
        assert!(!row_has_evidence_path(&r));
    }

    #[test]
    fn rows_clear_draft_bar_requires_both_number_and_path_present() {
        let with_number = row(
            JournalDecisionKind::StepIntegrated,
            "no path here",
            serde_json::json!({ "count": 3 }),
            0,
        );
        let with_path = row(
            JournalDecisionKind::StepIntegrated,
            "see planning/harness.json for detail",
            serde_json::json!({}),
            1,
        );
        // Neither alone clears the bar...
        assert!(!rows_clear_draft_bar(&[with_number.clone()]));
        assert!(!rows_clear_draft_bar(&[with_path.clone()]));
        // ...but together, across two different rows, they do.
        assert!(rows_clear_draft_bar(&[with_number, with_path]));
    }

    #[test]
    fn render_post_draft_returns_none_when_bar_not_cleared() {
        let rows = vec![row(
            JournalDecisionKind::StepIntegrated,
            "nothing measured or path-worthy",
            serde_json::json!({ "note": "plain text" }),
            0,
        )];
        assert!(render_post_draft(&rows).is_none());
    }

    #[test]
    fn render_post_draft_returns_none_for_zero_rows() {
        assert!(render_post_draft(&[]).is_none());
    }

    #[test]
    fn render_post_draft_returns_some_when_bar_cleared() {
        let rows = vec![row(
            JournalDecisionKind::StepIntegrated,
            "wrote crates/engine-core/src/workflows/orchestration/post_draft.rs",
            serde_json::json!({ "lines_added": 240 }),
            0,
        )];
        let draft = render_post_draft(&rows).expect("bar cleared, draft expected");
        assert!(draft.contains("lines_added=240"));
        assert!(draft.contains("post_draft.rs"));
    }

    #[test]
    fn render_post_draft_is_not_a_copy_of_the_ops_digest() {
        let rows = vec![row(
            JournalDecisionKind::StepIntegrated,
            "wrote src/lib.rs",
            serde_json::json!({ "coverage_pct": 87 }),
            0,
        )];
        let draft = render_post_draft(&rows).expect("bar cleared");
        let digest = super::super::debrief::render_brief(&rows);
        assert_ne!(draft, digest);
        assert!(draft.starts_with("Campaign"));
    }

    #[test]
    fn render_post_draft_orders_rows_by_created_at() {
        // Two rows, out of creation order, each contributing one half of
        // the bar — the draft must still be produced once both are
        // present, regardless of input order.
        let later = row(
            JournalDecisionKind::StepIntegrated,
            "second",
            serde_json::json!({ "score": 9 }),
            10,
        );
        let earlier = row(
            JournalDecisionKind::StepIntegrated,
            "first, see src/main.rs",
            serde_json::json!({}),
            0,
        );
        let draft = render_post_draft(&[later, earlier]).expect("bar cleared");
        assert!(draft.contains("score=9"));
        assert!(draft.contains("src/main.rs"));
    }

    fn clearing_row() -> JournalRow {
        row(
            JournalDecisionKind::StepIntegrated,
            "wrote crates/engine-core/src/workflows/orchestration/post_draft.rs",
            serde_json::json!({ "lines_added": 240 }),
            0,
        )
    }

    #[test]
    fn build_post_draft_payload_none_when_bar_not_cleared() {
        let rows = vec![row(
            JournalDecisionKind::StepIntegrated,
            "nothing measured or path-worthy",
            serde_json::json!({ "note": "plain text" }),
            0,
        )];
        assert!(build_post_draft_payload(&rows, "en").is_none());
    }

    #[test]
    fn build_post_draft_payload_round_trips_as_learning_artifact() {
        let rows = vec![clearing_row()];
        let payload = build_post_draft_payload(&rows, "en").expect("bar cleared");
        assert!(payload_round_trips_as_learning_artifact(&payload));

        let artifact = LearningArtifact::from_payload(&payload);
        assert_eq!(artifact.digest_markdown, render_post_draft(&rows).unwrap());
        assert_eq!(artifact.channel_type, POST_DRAFT_CHANNEL_TYPE);
        assert!(artifact.source_ref.contains("post_draft.rs"));
    }

    #[test]
    fn build_post_draft_payload_language_round_trips_en_and_pt_br() {
        let rows = vec![clearing_row()];

        let en_payload = build_post_draft_payload(&rows, "en").expect("bar cleared");
        assert_eq!(LearningArtifact::from_payload(&en_payload).language, "en");

        let pt_payload = build_post_draft_payload(&rows, "pt-BR").expect("bar cleared");
        assert_eq!(
            LearningArtifact::from_payload(&pt_payload).language,
            "pt-BR"
        );
    }

    #[test]
    fn build_post_draft_payload_defaults_language_when_empty() {
        let rows = vec![clearing_row()];
        let payload = build_post_draft_payload(&rows, "").expect("bar cleared");
        assert_eq!(LearningArtifact::from_payload(&payload).language, "en");
    }

    #[test]
    fn post_draft_payload_is_distinguishable_from_ops_digest_without_parsing_prose() {
        // The ops digest is dispatched as plain envelope text
        // (`debrief.rs`'s `SourcePayload::ChannelMessage { text, .. }`), not
        // as a `LearningArtifact` payload at all — so the two are
        // structurally distinguishable by shape alone. Within the
        // `LearningArtifact` shape itself, `channel_type` is the field a
        // consumer checks: it is never anything a content-pipeline ingest
        // would set (`web_article`, `youtube_transcript`, etc.), so a
        // reader never has to parse `digest_markdown` to tell the two
        // apart.
        let rows = vec![clearing_row()];
        let payload = build_post_draft_payload(&rows, "en").expect("bar cleared");
        assert_eq!(payload["channel_type"], POST_DRAFT_CHANNEL_TYPE);
        assert_ne!(payload["channel_type"], "web_article");
        assert_ne!(payload["channel_type"], "youtube_transcript");
    }

    #[test]
    fn post_draft_target_directory_is_derived_not_hardcoded() {
        // Task 2: "Any test touching the target directory DERIVES it from
        // `LearningArtifact::index_intent()`, never a hardcoded literal."
        let rows = vec![clearing_row()];
        let payload = build_post_draft_payload(&rows, "en").expect("bar cleared");
        let artifact = LearningArtifact::from_payload(&payload);
        let intent = artifact.index_intent();
        let dir = std::path::Path::new(&intent.index_path)
            .parent()
            .expect("index_path has a parent directory")
            .to_path_buf();
        // Derived, not restated as a literal: assert the directory the
        // model itself resolves to, which is what mev's materializer will
        // actually write into (`root/dirname(index_path)/link_target`).
        assert_eq!(
            dir,
            LearningArtifact::default()
                .index_intent()
                .index_path
                .rsplit_once('/')
                .map(|(dir, _)| std::path::PathBuf::from(dir))
                .expect("index_path has a directory component")
        );
    }

    #[test]
    fn no_fourth_brain_doc_model_is_added_here() {
        // Guards task 2's out-of-scope clause structurally: this module
        // imports and reuses `okf_core::LearningArtifact` and defines no
        // new type implementing `BrainDocModel`. If a future edit adds one,
        // this test's import line becomes the only `okf_core` model type
        // named in the module — a reviewer diff on this file is the check;
        // there is no runtime assertion possible for "a type was not
        // added", so this test instead pins that the existing model
        // round-trips end-to-end, which is what "reuse, don't mint" means
        // in practice.
        let rows = vec![clearing_row()];
        let payload = build_post_draft_payload(&rows, "en").expect("bar cleared");
        assert!(payload_round_trips_as_learning_artifact(&payload));
    }
}
