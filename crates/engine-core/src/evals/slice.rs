//! `EvalSlice`: a named, ordered collection of [`EvalCase`]s covering one
//! `(domain, model, profile)` combination — `engine-core::evals` (EN.5.B1
//! task 2).
//!
//! The `(domain, model, profile)` grouping deliberately mirrors
//! [`crate::policy::aggregate::PolicyAggregate`]'s grouping shape (it
//! groups runs by resolved policy and reports a `pass_rate` per group) so
//! the eval runner's report (task 3) lines up with the existing aggregate
//! report shape instead of inventing a parallel one.

use serde_json::Value;

use super::case::EvalCase;
use super::scorers::ScoreResult;

/// One [`EvalCase`]'s outcome within a [`SliceReport`], keeping the case
/// name alongside its [`ScoreResult`] for reporting.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CaseReport {
    /// The scored case's name.
    pub case_name: String,
    /// The scorer's verdict for this case.
    pub result: ScoreResult,
}

/// The report [`EvalSlice::score`] produces: per-case results plus a
/// pass-rate, tagged with the slice's `(domain, model, profile)` identity
/// so callers can group/aggregate reports the same way
/// [`crate::policy::aggregate::PolicyAggregate`] groups runs by policy.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SliceReport {
    /// The slice's name.
    pub slice_name: String,
    /// The domain this slice covers (e.g. `"coding"`).
    pub domain: String,
    /// The model this slice's cases were scored against.
    pub model: String,
    /// The policy profile this slice's cases were scored under.
    pub profile: String,
    /// Per-case results, in the slice's declared case order.
    pub case_results: Vec<CaseReport>,
    /// Number of cases in [`case_results`](Self::case_results) that passed.
    pub pass_count: usize,
    /// Total number of cases scored.
    pub total_count: usize,
    /// `pass_count / total_count`, or `0.0` if the slice has no cases.
    pub pass_rate: f64,
}

/// A named, ordered collection of [`EvalCase`]s plus the `(domain, model,
/// profile)` metadata identifying what this slice covers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvalSlice {
    /// The slice's name (e.g. `"coding"`).
    pub name: String,
    /// The domain this slice covers (e.g. `"coding"`, matching
    /// master-plan's "reports pass-rate by domain/model/profile"
    /// requirement).
    pub domain: String,
    /// The model this slice's cases are scored against.
    pub model: String,
    /// The policy profile this slice's cases are scored under (e.g.
    /// `"baseline"`, `"cheap-fast"`, `"thorough"` — the three named
    /// profiles every policy-bearing workflow ships per repo `CLAUDE.md`
    /// standing rule 6).
    pub profile: String,
    /// The cases this slice scores, in declared order.
    pub cases: Vec<EvalCase>,
}

impl EvalSlice {
    /// Construct a new slice.
    pub fn new(
        name: impl Into<String>,
        domain: impl Into<String>,
        model: impl Into<String>,
        profile: impl Into<String>,
        cases: Vec<EvalCase>,
    ) -> Self {
        Self {
            name: name.into(),
            domain: domain.into(),
            model: model.into(),
            profile: profile.into(),
            cases,
        }
    }

    /// Score every case in this slice against `record` and report a
    /// per-case breakdown plus an overall pass-rate.
    #[must_use]
    pub fn score(&self, record: &Value) -> SliceReport {
        let case_results: Vec<CaseReport> = self
            .cases
            .iter()
            .map(|case| CaseReport {
                case_name: case.name.clone(),
                result: case.score(record),
            })
            .collect();

        let total_count = case_results.len();
        let pass_count = case_results.iter().filter(|r| r.result.passed).count();
        let pass_rate = if total_count > 0 {
            pass_count as f64 / total_count as f64
        } else {
            0.0
        };

        SliceReport {
            slice_name: self.name.clone(),
            domain: self.domain.clone(),
            model: self.model.clone(),
            profile: self.profile.clone(),
            case_results,
            pass_count,
            total_count,
            pass_rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::case::ScorerKind;
    use serde_json::json;

    fn fixture_record() -> Value {
        json!({
            "tasks_passed": 4,
            "tasks_failed": 0,
            "outcome": {"verdict": "PASS"},
            "summary": "all four tasks passed with no regressions",
        })
    }

    #[test]
    fn three_case_slice_scores_a_fixture_record_and_reports_pass_rate() {
        let slice = EvalSlice::new(
            "coding",
            "coding",
            "claude-sonnet-4-5",
            "baseline",
            vec![
                EvalCase::new(
                    "tasks_passed exact match",
                    ScorerKind::Deterministic,
                    "tasks_passed",
                    json!(4),
                ),
                EvalCase::new(
                    "outcome has verdict shape",
                    ScorerKind::Structural,
                    "outcome",
                    json!({"verdict": ""}),
                ),
                EvalCase::new(
                    "summary mentions passed",
                    ScorerKind::ReferenceBased,
                    "summary",
                    json!("tasks passed"),
                ),
            ],
        );

        let report = slice.score(&fixture_record());

        assert_eq!(report.domain, "coding");
        assert_eq!(report.model, "claude-sonnet-4-5");
        assert_eq!(report.profile, "baseline");
        assert_eq!(report.total_count, 3);
        assert_eq!(report.pass_count, 3);
        assert_eq!(report.pass_rate, 1.0);
        assert!(report.case_results.iter().all(|r| r.result.passed));
    }

    #[test]
    fn slice_with_a_failing_case_reports_a_partial_pass_rate() {
        let slice = EvalSlice::new(
            "coding",
            "coding",
            "claude-sonnet-4-5",
            "baseline",
            vec![
                EvalCase::new(
                    "tasks_passed exact match",
                    ScorerKind::Deterministic,
                    "tasks_passed",
                    json!(4),
                ),
                EvalCase::new(
                    "tasks_passed wrong expectation",
                    ScorerKind::Deterministic,
                    "tasks_passed",
                    json!(99),
                ),
            ],
        );

        let report = slice.score(&fixture_record());

        assert_eq!(report.total_count, 2);
        assert_eq!(report.pass_count, 1);
        assert_eq!(report.pass_rate, 0.5);
    }

    #[test]
    fn empty_slice_reports_zero_pass_rate() {
        let slice = EvalSlice::new("empty", "coding", "m", "baseline", vec![]);
        let report = slice.score(&fixture_record());
        assert_eq!(report.total_count, 0);
        assert_eq!(report.pass_rate, 0.0);
    }
}
