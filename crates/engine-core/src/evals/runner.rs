//! Eval runner: score an [`EvalSlice`] against real captured SDLC-flow
//! telemetry, reusing `EN.4.0`'s harvester/aggregator with **no second
//! aggregation path** — `engine-core::evals` (EN.5.B1 task 3).
//!
//! [`run_slice`] loads and groups `*-state.json` files via
//! [`crate::policy::aggregate::aggregate_state_files`] (which itself calls
//! [`crate::policy::aggregate::aggregate`]) — this file imports that
//! function rather than re-parsing state files or re-implementing
//! grouping/summing, which is what makes the master-plan's "no second
//! aggregation path" acceptance criterion true by construction and not by
//! comment.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::aggregate::{aggregate_state_files, extract_policy_telemetry, PolicyAggregate};
use crate::policy::resolve::Policy;

use super::slice::{EvalSlice, SliceReport};

/// Unit policy used to key [`aggregate_state_files`]'s grouping when the
/// eval runner doesn't care about a run's resolved workflow policy — every
/// state file collapses into a single group, and the resulting
/// [`PolicyAggregate`] row (already computed by `EN.4.0`'s `aggregate`, not
/// re-derived here) is the record [`EvalSlice::score`] scores against.
/// A field-less struct (not a unit struct) so it round-trips through JSON
/// as `{}` rather than `null` — `extract_policy_telemetry` reads a state
/// file's `"policy"` key as a JSON object, and a unit struct (`struct
/// UnitPolicy;`) only deserializes from a bare `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UnitPolicy {}

impl Policy for UnitPolicy {
    type Partial = ();

    fn apply(self, _over: &Self::Partial) -> Self {
        self
    }
}

/// Run `slice` against the telemetry captured in `state_file_paths`.
///
/// Loads and groups each file's `(policy, telemetry)` pair via
/// [`aggregate_state_files`] — the same `EN.4.0` harvester/aggregator every
/// other cost/quality report in this repo uses, imported rather than
/// reimplemented (the "no second aggregation path" acceptance criterion).
/// Each state file is expected to carry its telemetry under a top-level
/// `"outcomes"` key (an optional `"policy"` key is read too, but its
/// contents are ignored by scoring here — see [`UnitPolicy`]).
///
/// The resulting aggregated row(s) are reduced to one JSON record (see
/// [`aggregate_rows_to_record`]) and scored via [`EvalSlice::score`], whose
/// report already carries this slice's `(domain, model, profile)` identity.
///
/// # Panics
/// Panics if any path in `state_file_paths` can't be read or parsed as
/// JSON. `run_slice`'s signature is fixed by this block's spec to return a
/// plain [`SliceReport`], not a `Result`, so a bad path here is a
/// programmer/fixture error in the caller, not a recoverable eval-time
/// condition.
#[must_use]
pub fn run_slice(slice: &EvalSlice, state_file_paths: &[PathBuf]) -> SliceReport {
    let rows = aggregate_state_files(state_file_paths, |value| {
        extract_policy_telemetry::<UnitPolicy>(value, "policy", "outcomes")
    })
    .expect("eval state files must be readable and parseable JSON");

    let record = aggregate_rows_to_record(&rows);
    slice.score(&record)
}

/// Reduce one or more [`PolicyAggregate`] rows to the single JSON record
/// [`run_slice`] scores. Since every file is grouped under [`UnitPolicy`],
/// all state files ordinarily collapse into exactly one row; the first row
/// is used if more than one somehow appears (e.g. a mix of files where some
/// are missing the `"policy"`/`"outcomes"` keys and get skipped by
/// `extract_policy_telemetry`, changing which rows form). `run_slice`'s job
/// is to score one slice's telemetry, not to re-aggregate across groups —
/// doing that here would itself be exactly the second aggregation path
/// this task forbids.
fn aggregate_rows_to_record<P: Serialize>(rows: &[PolicyAggregate<P>]) -> Value {
    rows.first()
        .map(|row| serde_json::to_value(row).expect("PolicyAggregate always serializes"))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::case::{EvalCase, ScorerKind};
    use serde_json::json;

    fn write_state_file(dir: &std::path::Path, name: &str, tasks_passed: u32, tasks_failed: u32) {
        let state = json!({
            "policy": {},
            "outcomes": {
                "wall_clock_secs": 42.0,
                "total_attempts": tasks_passed + tasks_failed,
                "total_retries": 0,
                "tasks_passed": tasks_passed,
                "tasks_failed": tasks_failed,
                "review_verdicts": ["ConsolidatedReviewNode:PASS"],
                "total_input_tokens": 100,
                "total_output_tokens": 50,
                "total_cost_usd": 0.25,
                "model_tier_used": {},
            },
        });
        std::fs::write(dir.join(name), state.to_string()).expect("write fixture state file");
    }

    #[test]
    fn run_slice_scores_pass_rate_from_aggregated_state_files() {
        let dir =
            std::env::temp_dir().join(format!("en5b1-eval-runner-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");

        write_state_file(&dir, "run-a-state.json", 4, 0);
        write_state_file(&dir, "run-b-state.json", 4, 0);

        let slice = EvalSlice::new(
            "coding",
            "coding",
            "claude-sonnet-4-5",
            "baseline",
            vec![EvalCase::new(
                "pass_rate is perfect",
                ScorerKind::Deterministic,
                "pass_rate",
                json!(1.0),
            )],
        );

        let paths = vec![dir.join("run-a-state.json"), dir.join("run-b-state.json")];
        let report = run_slice(&slice, &paths);

        assert_eq!(report.domain, "coding");
        assert_eq!(report.total_count, 1);
        assert_eq!(report.pass_count, 1);
        assert_eq!(report.pass_rate, 1.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[should_panic(expected = "readable and parseable JSON")]
    fn run_slice_panics_on_unreadable_state_file() {
        let slice = EvalSlice::new("coding", "coding", "m", "baseline", vec![]);
        let paths = vec![PathBuf::from("/nonexistent/does-not-exist-state.json")];
        let _ = run_slice(&slice, &paths);
    }
}
