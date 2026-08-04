//! `engine-core::evals` — the eval slice runner (EN.5.B1, first half of the
//! `OR.U` port).
//!
//! Ports Synapse's deterministic/structural/reference-based scorer library
//! (`app/brain/eval/scorer.py` in `core/orchestrator`) plus its
//! `EvalCase`/`EvalSlice` concepts, scoped down to generic run/workflow
//! telemetry — **not** retrieval scoring (recall@k, MRR, abstain
//! correctness, citation groundedness), which stays in Synapse's `OR.K2`
//! per the repo `CLAUDE.md` boundary test and this block's Notes. If a
//! scorer here ever wants an embedding call, that is the signal it has
//! drifted onto the wrong side of the D51 boundary.
//!
//! Task 1 (`scorers.rs`) lands the pure scorer functions. Task 2
//! (`case.rs`, `slice.rs`) lands `EvalCase`/`EvalSlice`. Task 3
//! (`runner.rs` + [`coding_slice`] below) lands the runner over
//! `policy::aggregate::aggregate_state_files` and the concrete coding slice.

pub mod case;
pub mod runner;
pub mod scorers;
pub mod slice;

use serde_json::json;

pub use case::{EvalCase, ScorerKind};
pub use runner::{run_slice, UnitPolicy};
pub use scorers::{score_deterministic, score_reference_based, score_structural, ScoreResult};
pub use slice::{CaseReport, EvalSlice, SliceReport};

/// The `coding` domain's `EN.5.B1` eval slice: scores SDLC-flow telemetry
/// (via `EN.4.0`'s `RunTelemetry`/`PolicyAggregate`, as harvested by
/// [`run_slice`]) against three generic cases, one per scorer kind:
///
/// - `pass_rate is perfect` (deterministic) — the aggregated
///   `tasks_passed`/`tasks_failed` ratio should be `1.0` for a clean run.
/// - `review verdict counts shape` (structural) — the aggregated
///   `review_verdict_counts` map should carry a `ConsolidatedReviewNode:PASS`
///   tally.
/// - `policy group present` (reference-based) — the serialized `policy`
///   field (empty for the eval runner's [`UnitPolicy`]) should be present
///   in the aggregated record's own JSON text.
///
/// Retrieval-eval domains (recall@k, MRR, abstain correctness, citation
/// groundedness) are deliberately out of scope here — see the module docs'
/// D51 boundary note.
#[must_use]
pub fn coding_slice() -> EvalSlice {
    EvalSlice::new(
        "coding",
        "coding",
        "claude-sonnet-4-5",
        "baseline",
        vec![
            EvalCase::new(
                "pass_rate is perfect",
                ScorerKind::Deterministic,
                "pass_rate",
                json!(1.0),
            ),
            EvalCase::new(
                "review verdict counts shape",
                ScorerKind::Structural,
                "review_verdict_counts",
                json!({"ConsolidatedReviewNode:PASS": 0}),
            ),
            EvalCase::new(
                "policy group present",
                ScorerKind::ReferenceBased,
                "policy",
                json!("{}"),
            ),
        ],
    )
}

#[cfg(test)]
mod coding_slice_tests {
    use super::*;

    #[test]
    fn coding_slice_has_one_case_per_scorer_kind() {
        let slice = coding_slice();
        assert_eq!(slice.cases.len(), 3);
        assert_eq!(slice.domain, "coding");
    }

    #[test]
    fn coding_slice_scores_a_clean_aggregate_record() {
        let slice = coding_slice();
        let record = json!({
            "pass_rate": 1.0,
            "review_verdict_counts": {"ConsolidatedReviewNode:PASS": 2},
            "policy": {},
        });
        let report = slice.score(&record);
        assert_eq!(report.pass_count, 3);
        assert_eq!(report.total_count, 3);
    }
}
