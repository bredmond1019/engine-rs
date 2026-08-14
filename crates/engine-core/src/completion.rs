//! A run-level completion marker for terminal exits (EN.9.C task 1).
//!
//! Mirrors [`crate::cancellation::stamp_cancelled`] field for field:
//! `metadata.completion` is another engine-rs-only run-level annotation in
//! `TaskContext::metadata` (free-form JSON per contract §5), not a canonical
//! contract re-pin. See `planning/EN.9.C/tasks.md` for the design decision —
//! there is no `status` column on `events`, so a boot sweep for crash-stranded
//! runs must be able to tell "finished cleanly" apart from "never finished" by
//! the *absence* of this marker rather than by deriving status alone (a run
//! that crashed mid-walk has no failure marker either, so
//! `derive_terminal_status` would otherwise read it as `succeeded`).

use chrono::Utc;

/// The `TaskContext::metadata` key under which a terminal run's completion
/// marker is recorded — see [`stamp_completion`].
pub const COMPLETION_METADATA_KEY: &str = "completion";

/// Stamps `metadata` with the terminal completion marker:
/// `{ "completion": { "terminal": true, "status": <status>, "at": <rfc3339> } }`.
///
/// If `metadata` is not already a JSON object, it is replaced with one first
/// (matching `TaskContext::metadata`'s default shape of `{}` — same defensive
/// branch as [`crate::cancellation::stamp_cancelled`]). Sibling annotations
/// (`cancellation`, `budget`, `suspension`) are untouched.
///
/// `status` is expected to be whatever `derive_terminal_status` reports for
/// the same snapshot (`succeeded|failed|cancelled|budget_halted`), but this
/// module does not depend on `engine-serve` and does not validate the value —
/// the caller owns that vocabulary.
pub fn stamp_completion(metadata: &mut serde_json::Value, status: &str) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    let at = Utc::now().to_rfc3339();
    metadata[COMPLETION_METADATA_KEY] = serde_json::json!({
        "terminal": true,
        "status": status,
        "at": at,
    });
}

/// Returns `true` only when `metadata.completion.terminal == true`.
///
/// Absent, malformed, or explicitly `terminal: false` metadata all return
/// `false` — this is the predicate the orphan sweep negates to find
/// crash-stranded runs (task 3).
pub fn is_complete(metadata: &serde_json::Value) -> bool {
    metadata
        .get(COMPLETION_METADATA_KEY)
        .and_then(|c| c.get("terminal"))
        .and_then(|t| t.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::TaskContext;
    use std::collections::HashMap;

    #[test]
    fn stamp_completion_sets_the_metadata_shape() {
        let mut metadata = serde_json::json!({});
        stamp_completion(&mut metadata, "succeeded");

        let completion = &metadata[COMPLETION_METADATA_KEY];
        assert_eq!(completion["terminal"], serde_json::json!(true));
        assert_eq!(completion["status"], serde_json::json!("succeeded"));
        assert!(completion["at"].is_string());
    }

    #[test]
    fn stamp_completion_replaces_non_object_metadata() {
        let mut metadata = serde_json::Value::Null;
        stamp_completion(&mut metadata, "failed");

        assert!(metadata.is_object());
        assert_eq!(
            metadata[COMPLETION_METADATA_KEY]["terminal"],
            serde_json::json!(true)
        );
        assert_eq!(
            metadata[COMPLETION_METADATA_KEY]["status"],
            serde_json::json!("failed")
        );
    }

    #[test]
    fn stamp_completion_preserves_sibling_annotations() {
        let mut metadata = serde_json::json!({
            "cancellation": { "cancelled": true, "at": "2026-01-01T00:00:00Z" },
            "budget": { "halted": true },
            "suspension": { "suspended": true },
        });
        stamp_completion(&mut metadata, "cancelled");

        assert_eq!(
            metadata["cancellation"]["cancelled"],
            serde_json::json!(true)
        );
        assert_eq!(metadata["budget"]["halted"], serde_json::json!(true));
        assert_eq!(metadata["suspension"]["suspended"], serde_json::json!(true));
        assert_eq!(
            metadata[COMPLETION_METADATA_KEY]["status"],
            serde_json::json!("cancelled")
        );
    }

    #[test]
    fn is_complete_true_when_terminal_true() {
        let mut metadata = serde_json::json!({});
        stamp_completion(&mut metadata, "succeeded");
        assert!(is_complete(&metadata));
    }

    #[test]
    fn is_complete_false_when_absent() {
        let metadata = serde_json::json!({});
        assert!(!is_complete(&metadata));
    }

    #[test]
    fn is_complete_false_when_malformed() {
        let metadata = serde_json::json!({ "completion": "not-an-object" });
        assert!(!is_complete(&metadata));

        let metadata = serde_json::json!({ "completion": { "terminal": "yes" } });
        assert!(!is_complete(&metadata));
    }

    #[test]
    fn is_complete_false_when_terminal_explicitly_false() {
        let metadata = serde_json::json!({ "completion": { "terminal": false } });
        assert!(!is_complete(&metadata));
    }

    #[test]
    fn metadata_helper_shape_survives_task_context_round_trip() {
        let mut metadata = serde_json::json!({});
        stamp_completion(&mut metadata, "budget_halted");

        let tc = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata,
            node_runs: HashMap::new(),
        };

        let v = serde_json::to_value(&tc).unwrap();
        let round_tripped: TaskContext = serde_json::from_value(v).unwrap();

        assert_eq!(
            round_tripped.metadata[COMPLETION_METADATA_KEY]["terminal"],
            serde_json::json!(true)
        );
        assert_eq!(
            round_tripped.metadata[COMPLETION_METADATA_KEY]["status"],
            serde_json::json!("budget_halted")
        );
        assert!(is_complete(&round_tripped.metadata));
    }
}
