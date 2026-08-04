//! Pure scorer functions for `engine-core::evals` (EN.5.B1 task 1).
//!
//! Ported from Synapse's `app/brain/eval/scorer.py`, generalized off the
//! retrieval-specific `RetrievalCase`/`CaseResult` shapes there (recall@k,
//! reciprocal rank, groundedness — all retrieval-eval concerns that stay in
//! Synapse's `OR.K2` per the D51 boundary) down to three generic scorer
//! kinds over `serde_json::Value`/`String` inputs:
//!
//! - [`score_deterministic`] — exact-match pass/fail against an expected value.
//! - [`score_structural`] — shape/schema conformance (expected keys + JSON
//!   types present), without checking exact values.
//! - [`score_reference_based`] — similarity against a reference string via
//!   containment or token-overlap ratio. Deliberately **not**
//!   embedding-based — an embedding call here would cross the D51 boundary
//!   into Synapse's territory (see `crate::evals` module docs).
//!
//! Every scorer is a pure function: no I/O, no network, no LLM, no corpus
//! or retrieval data touched.

use serde_json::Value;
use std::collections::HashSet;

/// The uniform result shape every scorer in this module returns.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ScoreResult {
    /// Whether this case is considered a pass.
    pub passed: bool,
    /// A continuous score in `[0.0, 1.0]` (deterministic scorers use only
    /// the endpoints; structural/reference-based scorers use the full range).
    pub score: f64,
    /// Human-readable explanation of how the score was reached.
    pub detail: String,
}

/// Deterministic scorer: exact-match / boolean pass-fail of `actual`
/// against `expected`.
///
/// Compares the two `serde_json::Value`s for structural equality — this
/// covers scalars (numbers, strings, bools), and also objects/arrays when a
/// caller wants byte-for-byte equality rather than the looser shape check
/// [`score_structural`] performs.
pub fn score_deterministic(actual: &Value, expected: &Value) -> ScoreResult {
    if actual == expected {
        ScoreResult {
            passed: true,
            score: 1.0,
            detail: format!("exact match: {actual}"),
        }
    } else {
        ScoreResult {
            passed: false,
            score: 0.0,
            detail: format!("expected {expected}, got {actual}"),
        }
    }
}

/// Structural scorer: does `actual` have the keys `expected_shape`
/// declares, with matching JSON types — without checking exact values.
///
/// Both `actual` and `expected_shape` must be JSON objects; any other shape
/// pairing is scored as a hard fail (score `0.0`, `passed: false`) since
/// there is no key set to compare. The score is the fraction of
/// `expected_shape`'s keys that are present in `actual` with a matching
/// [`serde_json::Value`] type (missing key and type-mismatch both count as
/// one miss each).
pub fn score_structural(actual: &Value, expected_shape: &Value) -> ScoreResult {
    let (Value::Object(actual_map), Value::Object(shape_map)) = (actual, expected_shape) else {
        return ScoreResult {
            passed: false,
            score: 0.0,
            detail: "structural scorer requires both actual and expected_shape to be JSON objects"
                .to_string(),
        };
    };

    if shape_map.is_empty() {
        return ScoreResult {
            passed: true,
            score: 1.0,
            detail: "expected_shape declares no keys; vacuously satisfied".to_string(),
        };
    }

    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    for (key, expected_val) in shape_map {
        match actual_map.get(key) {
            None => missing.push(key.clone()),
            Some(actual_val) => {
                if type_name(actual_val) != type_name(expected_val) {
                    mismatched.push(format!(
                        "{key} (expected {}, got {})",
                        type_name(expected_val),
                        type_name(actual_val)
                    ));
                }
            }
        }
    }

    let total = shape_map.len() as f64;
    let bad = (missing.len() + mismatched.len()) as f64;
    let score = ((total - bad) / total).clamp(0.0, 1.0);
    let passed = missing.is_empty() && mismatched.is_empty();
    let detail = if passed {
        format!(
            "all {} expected keys present with matching types",
            shape_map.len()
        )
    } else {
        format!("missing keys: {missing:?}; type mismatches: {mismatched:?}")
    };

    ScoreResult {
        passed,
        score,
        detail,
    }
}

/// Reference-based scorer: similarity of `actual` against `reference`.
///
/// Two-stage: an exact-containment short-circuit (score `1.0` if `actual`
/// contains `reference` verbatim), then a token-overlap ratio — the
/// fraction of `reference`'s lowercased whitespace tokens also present in
/// `actual`'s token set. Passes at a fixed `0.5` overlap threshold.
///
/// Deliberately **no embedding-based similarity metric** — that would
/// reach for corpus/retrieval machinery and cross the D51 boundary into
/// Synapse's territory (see `crate::evals` module docs and this block's
/// Notes in `planning/en-5b1-eval-slice-runner/tasks.md`).
pub fn score_reference_based(actual: &str, reference: &str) -> ScoreResult {
    const PASS_THRESHOLD: f64 = 0.5;

    if reference.is_empty() {
        let matches = actual.is_empty();
        return ScoreResult {
            passed: matches,
            score: if matches { 1.0 } else { 0.0 },
            detail: "reference is empty; actual must also be empty to pass".to_string(),
        };
    }

    if actual.contains(reference) {
        return ScoreResult {
            passed: true,
            score: 1.0,
            detail: "actual contains reference verbatim".to_string(),
        };
    }

    let reference_tokens = tokenize(reference);
    if reference_tokens.is_empty() {
        // Reference had only punctuation/whitespace and produced no tokens;
        // fall back to a straight containment-failed verdict.
        return ScoreResult {
            passed: false,
            score: 0.0,
            detail: "reference produced no comparable tokens".to_string(),
        };
    }

    let actual_tokens = tokenize(actual);
    let overlap = reference_tokens.intersection(&actual_tokens).count();
    let score = overlap as f64 / reference_tokens.len() as f64;
    let passed = score >= PASS_THRESHOLD;

    ScoreResult {
        passed,
        score,
        detail: format!(
            "token overlap {overlap}/{} ({score:.2}) against pass threshold {PASS_THRESHOLD:.2}",
            reference_tokens.len()
        ),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn tokenize(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|tok| {
            tok.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|tok| !tok.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- score_deterministic ---

    #[test]
    fn deterministic_exact_match_passes() {
        let result = score_deterministic(&json!({"tasks_passed": 4}), &json!({"tasks_passed": 4}));
        assert!(result.passed);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn deterministic_mismatch_fails() {
        let result = score_deterministic(&json!("review"), &json!("verify"));
        assert!(!result.passed);
        assert_eq!(result.score, 0.0);
        assert!(result.detail.contains("verify"));
    }

    // --- score_structural ---

    #[test]
    fn structural_matching_shape_passes() {
        let actual = json!({"tasks_passed": 4, "tasks_failed": 0, "verdict": "PASS"});
        let shape = json!({"tasks_passed": 0, "tasks_failed": 0, "verdict": ""});
        let result = score_structural(&actual, &shape);
        assert!(result.passed);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn structural_missing_key_fails() {
        let actual = json!({"tasks_passed": 4});
        let shape = json!({"tasks_passed": 0, "verdict": ""});
        let result = score_structural(&actual, &shape);
        assert!(!result.passed);
        assert!(result.score < 1.0);
        assert!(result.detail.contains("verdict"));
    }

    #[test]
    fn structural_type_mismatch_fails() {
        let actual = json!({"verdict": 1});
        let shape = json!({"verdict": ""});
        let result = score_structural(&actual, &shape);
        assert!(!result.passed);
        assert!(result.detail.contains("type mismatches"));
    }

    #[test]
    fn structural_ignores_exact_values() {
        // Values differ, but keys/types line up — structural scoring must
        // not require value equality (that's score_deterministic's job).
        let actual = json!({"tasks_passed": 99});
        let shape = json!({"tasks_passed": 0});
        let result = score_structural(&actual, &shape);
        assert!(result.passed);
    }

    #[test]
    fn structural_non_object_inputs_fail() {
        let result = score_structural(&json!([1, 2, 3]), &json!({"a": 1}));
        assert!(!result.passed);
        assert_eq!(result.score, 0.0);
    }

    // --- score_reference_based ---

    #[test]
    fn reference_based_containment_passes() {
        let result =
            score_reference_based("all four tasks passed with no regressions", "tasks passed");
        assert!(result.passed);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn reference_based_high_token_overlap_passes() {
        let result = score_reference_based(
            "coding slice pass-rate grouped by domain model profile",
            "pass-rate grouped by domain and model",
        );
        assert!(result.passed);
        assert!(result.score >= 0.5);
    }

    #[test]
    fn reference_based_low_overlap_fails() {
        let result = score_reference_based(
            "completely unrelated output text",
            "expected reference value",
        );
        assert!(!result.passed);
        assert!(result.score < 0.5);
    }

    #[test]
    fn reference_based_empty_reference_requires_empty_actual() {
        let passing = score_reference_based("", "");
        assert!(passing.passed);

        let failing = score_reference_based("non-empty", "");
        assert!(!failing.passed);
    }
}
