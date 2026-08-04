//! `EvalCase`: one scored assertion against a selected field of a scored
//! record (a `RunTelemetry`/`ctx.nodes` output, expressed as
//! `serde_json::Value`) — `engine-core::evals` (EN.5.B1 task 2).

use serde_json::Value;

use super::scorers::{score_deterministic, score_reference_based, score_structural, ScoreResult};

/// Which of the three generic scorer functions an [`EvalCase`] applies.
/// Mirrors the three scorers ported in task 1 — no fourth (embedding-based)
/// kind exists, per the D51 boundary guard (see `crate::evals` module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScorerKind {
    /// Exact-match pass/fail via [`score_deterministic`].
    Deterministic,
    /// Shape/schema conformance via [`score_structural`].
    Structural,
    /// Similarity against a reference string via [`score_reference_based`].
    ReferenceBased,
}

/// One eval case: names a scorer kind, a selector for which field of a
/// scored record (a `RunTelemetry`/`ctx.nodes` output, as JSON) it reads,
/// and an expected/reference value to score that field against.
///
/// `expected` is interpreted per `scorer_kind`: for [`ScorerKind::Deterministic`]
/// it's the expected value compared for structural equality; for
/// [`ScorerKind::Structural`] it's the expected shape (an object whose keys
/// declare the required fields/types); for [`ScorerKind::ReferenceBased`]
/// its string form (`Value::as_str`, or the value's JSON text if it isn't a
/// JSON string) is the reference text to compare against.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvalCase {
    /// Human-readable case name, surfaced in reports.
    pub name: String,
    /// Which scorer this case applies.
    pub scorer_kind: ScorerKind,
    /// Dot-separated path selecting a field out of the scored record's
    /// root JSON object (e.g. `"tasks_passed"`, or `"model_tier_used.implement"`
    /// for a nested lookup). Numeric segments index into JSON arrays.
    pub selector: String,
    /// The expected/reference value this case's scorer compares the
    /// selected field against (see the type-level doc for how each scorer
    /// kind interprets it).
    pub expected: Value,
}

impl EvalCase {
    /// Construct a new case.
    pub fn new(
        name: impl Into<String>,
        scorer_kind: ScorerKind,
        selector: impl Into<String>,
        expected: Value,
    ) -> Self {
        Self {
            name: name.into(),
            scorer_kind,
            selector: selector.into(),
            expected,
        }
    }

    /// Select this case's field out of `record` (via [`select_field`]) and
    /// score it with the scorer named by `scorer_kind`. A selector that
    /// resolves to nothing scores against `Value::Null`, which every
    /// scorer treats as an ordinary (almost always failing) input rather
    /// than a special case.
    #[must_use]
    pub fn score(&self, record: &Value) -> ScoreResult {
        let actual = select_field(record, &self.selector)
            .cloned()
            .unwrap_or(Value::Null);

        match self.scorer_kind {
            ScorerKind::Deterministic => score_deterministic(&actual, &self.expected),
            ScorerKind::Structural => score_structural(&actual, &self.expected),
            ScorerKind::ReferenceBased => {
                let actual_str = value_as_text(&actual);
                let reference_str = value_as_text(&self.expected);
                score_reference_based(&actual_str, &reference_str)
            }
        }
    }
}

/// Render a JSON value as scorable text: a JSON string's own contents
/// verbatim (no surrounding quotes), or the value's JSON text for every
/// other shape (numbers, bools, objects, arrays, null).
fn value_as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Walk a dot-separated `selector` path into `root`, returning the value at
/// that path or `None` if any segment is missing. A numeric segment (e.g.
/// `"0"`) indexes into a JSON array at that position; any other segment
/// looks up an object key. An empty selector returns `root` itself.
fn select_field<'a>(root: &'a Value, selector: &str) -> Option<&'a Value> {
    if selector.is_empty() {
        return Some(root);
    }

    selector.split('.').try_fold(root, |value, segment| {
        if let Ok(index) = segment.parse::<usize>() {
            value.get(index)
        } else {
            value.get(segment)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn select_field_reads_a_top_level_key() {
        let record = json!({"tasks_passed": 4});
        assert_eq!(select_field(&record, "tasks_passed"), Some(&json!(4)));
    }

    #[test]
    fn select_field_reads_a_nested_dotted_path() {
        let record = json!({"model_tier_used": {"implement": "sonnet"}});
        assert_eq!(
            select_field(&record, "model_tier_used.implement"),
            Some(&json!("sonnet"))
        );
    }

    #[test]
    fn select_field_indexes_into_an_array() {
        let record = json!({"review_verdicts": ["A:PASS", "B:FAIL"]});
        assert_eq!(
            select_field(&record, "review_verdicts.1"),
            Some(&json!("B:FAIL"))
        );
    }

    #[test]
    fn select_field_missing_path_returns_none() {
        let record = json!({"tasks_passed": 4});
        assert_eq!(select_field(&record, "nope"), None);
    }

    #[test]
    fn deterministic_case_scores_a_selected_field() {
        let case = EvalCase::new(
            "tasks_passed matches",
            ScorerKind::Deterministic,
            "tasks_passed",
            json!(4),
        );
        let record = json!({"tasks_passed": 4});
        let result = case.score(&record);
        assert!(result.passed);
    }

    #[test]
    fn deterministic_case_fails_on_mismatch() {
        let case = EvalCase::new(
            "tasks_passed matches",
            ScorerKind::Deterministic,
            "tasks_passed",
            json!(4),
        );
        let record = json!({"tasks_passed": 1});
        let result = case.score(&record);
        assert!(!result.passed);
    }

    #[test]
    fn structural_case_checks_shape() {
        let case = EvalCase::new(
            "review verdicts shape",
            ScorerKind::Structural,
            "outcome",
            json!({"verdict": ""}),
        );
        let record = json!({"outcome": {"verdict": "PASS"}});
        assert!(case.score(&record).passed);
    }

    #[test]
    fn reference_based_case_compares_text() {
        let case = EvalCase::new(
            "detail mentions pass",
            ScorerKind::ReferenceBased,
            "detail",
            json!("tasks passed"),
        );
        let record = json!({"detail": "all four tasks passed with no regressions"});
        assert!(case.score(&record).passed);
    }

    #[test]
    fn missing_selector_scores_against_null() {
        let case = EvalCase::new(
            "missing field",
            ScorerKind::Deterministic,
            "does_not_exist",
            json!(4),
        );
        let record = json!({"tasks_passed": 4});
        let result = case.score(&record);
        assert!(!result.passed);
    }
}
