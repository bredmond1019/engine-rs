//! Integration test for `engine-core::evals::run_slice` — `EN.5.B1` task 3.
//!
//! Runs the concrete `coding_slice()` against a small synthetic
//! `*-state.json` fixture (`tests/fixtures/eval_coding_state.json`, not a
//! real captured production run) and asserts a pass-rate report grouped by
//! `(domain, model, profile)`, sourced entirely through `EN.4.0`'s
//! `aggregate_state_files`/`aggregate` — no second aggregation path.

use std::path::PathBuf;

use engine_core::evals::{coding_slice, run_slice};

#[test]
fn coding_slice_scores_a_captured_state_file_and_reports_pass_rate() {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval_coding_state.json");

    let slice = coding_slice();
    let report = run_slice(&slice, &[fixture]);

    // Grouped by (domain, model, profile) per the slice's own identity.
    assert_eq!(report.domain, "coding");
    assert_eq!(report.model, "claude-sonnet-4-5");
    assert_eq!(report.profile, "baseline");

    // The fixture's tasks_passed=4/tasks_failed=0 aggregates to a perfect
    // pass_rate, its review_verdicts carries a ConsolidatedReviewNode:PASS
    // entry, and its policy group is present — all three coding_slice
    // cases (deterministic, structural, reference-based) should pass.
    assert_eq!(report.total_count, 3);
    assert_eq!(report.pass_count, 3);
    assert_eq!(report.pass_rate, 1.0);
    assert!(report.case_results.iter().all(|c| c.result.passed));
}

#[test]
fn coding_slice_reports_a_partial_pass_rate_on_a_failing_run() {
    let dir = std::env::temp_dir().join(format!("en5b1-eval-slice-it-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let failing_state = serde_json::json!({
        "policy": {},
        "outcomes": {
            "wall_clock_secs": 60.0,
            "total_attempts": 2,
            "total_retries": 2,
            "tasks_passed": 1,
            "tasks_failed": 1,
            "review_verdicts": ["ConsolidatedReviewNode:FAIL"],
            "total_input_tokens": 500,
            "total_output_tokens": 200,
            "total_cost_usd": 0.1,
            "model_tier_used": {}
        }
    });
    let path = dir.join("failing-state.json");
    std::fs::write(&path, failing_state.to_string()).expect("write fixture");

    let slice = coding_slice();
    let report = run_slice(&slice, &[path]);

    // pass_rate is 0.5, not 1.0, and the review verdict shape case fails
    // (no ConsolidatedReviewNode:PASS key) — at most the reference-based
    // "policy group present" case passes.
    assert!(report.pass_rate < 1.0);
    assert!(!report.case_results.iter().all(|c| c.result.passed));

    std::fs::remove_dir_all(&dir).ok();
}
