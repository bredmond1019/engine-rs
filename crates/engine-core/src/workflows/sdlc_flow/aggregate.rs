//! Cross-run `(policy -> cost, time, quality)` aggregator (EN.3.C task 7).
//!
//! Reads a set of completed `sdlc-flow-state.json` snapshots (each carrying
//! the `policy`/`outcomes` blocks `WrapUpNode` stamps at the run tail — see
//! `schema::SDLCState`/`schema::RunOutcomes`), groups runs by their resolved
//! [`SdlcPolicy`], and tabulates summed/averaged cost, token, wall-clock,
//! retry, and review-verdict columns per distinct policy — the same shape as
//! the 66-run aggregation in `planning/sdlc-token-time-economics/notes.md`
//! ("Consolidated ranking" table), so the best-performing policy per
//! workload can be read off real run data rather than guessed.
//!
//! States missing either block (no run ever reached `WrapUpNode`, or a
//! pre-EN.3.C state file) are skipped — there is no policy to key them by.
//!
//! **EN.4.0:** delegates to the generic `crate::policy::aggregate` — this
//! module now only converts `(SdlcPolicy, RunOutcomes)` pairs into
//! `(SdlcPolicy, RunTelemetry)` pairs (via `RunOutcomes`'s `Into` impl,
//! `schema.rs`) and hands them to the generic `aggregate`/
//! `aggregate_state_files`. `PolicyAggregate` is a type alias for the
//! generic `crate::policy::PolicyAggregate<SdlcPolicy>`, so its field shape
//! is unchanged.

use std::path::Path;

#[cfg(test)]
use std::collections::BTreeMap;

use super::policy::SdlcPolicy;
use super::schema::{RunOutcomes, SDLCState};

/// One row of the cross-run aggregation table: a distinct resolved
/// [`SdlcPolicy`] plus the summed/averaged outcome metrics across every run
/// that resolved to it. A type alias for the generic
/// `crate::policy::PolicyAggregate<SdlcPolicy>` (EN.4.0) — same fields,
/// same shape, as the pre-hoist concrete struct.
pub type PolicyAggregate = crate::policy::PolicyAggregate<SdlcPolicy>;

/// Group `(policy, outcomes)` pairs by resolved policy and tabulate one
/// [`PolicyAggregate`] row per distinct policy. Rows are returned sorted by
/// their policy's canonical JSON key, for deterministic output. Delegates
/// to the generic `crate::policy::aggregate::aggregate` (EN.4.0).
#[must_use]
pub fn aggregate_outcomes(runs: &[(SdlcPolicy, RunOutcomes)]) -> Vec<PolicyAggregate> {
    let converted: Vec<(SdlcPolicy, crate::policy::RunTelemetry)> = runs
        .iter()
        .map(|(policy, outcomes)| (policy.clone(), outcomes.clone().into()))
        .collect();
    crate::policy::aggregate::aggregate(&converted)
}

/// Extract `(policy, outcomes)` pairs from a set of completed
/// [`SDLCState`] snapshots, skipping any state missing either block, then
/// aggregate via [`aggregate_outcomes`].
#[must_use]
pub fn aggregate_states(states: &[SDLCState]) -> Vec<PolicyAggregate> {
    let runs: Vec<(SdlcPolicy, RunOutcomes)> = states
        .iter()
        .filter_map(|state| match (&state.policy, &state.outcomes) {
            (Some(policy), Some(outcomes)) => Some((policy.clone(), outcomes.clone())),
            _ => None,
        })
        .collect();
    aggregate_outcomes(&runs)
}

/// Read a set of `sdlc-flow-state.json` files from disk and aggregate them
/// via the generic `crate::policy::aggregate::aggregate_state_files`
/// (EN.4.0), extracting `(policy, outcomes)` from each state's `policy`/
/// `outcomes` fields. Returns an `io::Error` if any file can't be read or
/// fails to parse as [`SDLCState`] JSON.
pub fn aggregate_state_files<P: AsRef<Path>>(paths: &[P]) -> std::io::Result<Vec<PolicyAggregate>> {
    crate::policy::aggregate::aggregate_state_files(paths, |value| {
        crate::policy::aggregate::extract_policy_telemetry::<SdlcPolicy>(
            value, "policy", "outcomes",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::policy::{ModelTier, OutputVerbosity};

    struct OutcomeSpec {
        cost: f64,
        wall_clock: f64,
        input_tok: u64,
        output_tok: u64,
        attempts: u32,
        retries: u32,
        passed: u32,
        failed: u32,
        verdicts: &'static [&'static str],
    }

    fn outcomes(spec: OutcomeSpec) -> RunOutcomes {
        RunOutcomes {
            wall_clock_secs: spec.wall_clock,
            total_attempts: spec.attempts,
            total_retries: spec.retries,
            tasks_passed: spec.passed,
            tasks_failed: spec.failed,
            review_verdicts: spec.verdicts.iter().map(|s| s.to_string()).collect(),
            total_input_tokens: spec.input_tok,
            total_output_tokens: spec.output_tok,
            total_cost_usd: spec.cost,
            model_tier_used: BTreeMap::new(),
        }
    }

    fn state_with(policy: SdlcPolicy, outcomes: RunOutcomes, slug: &str) -> SDLCState {
        let mut state = SDLCState::new(slug);
        state.policy = Some(policy);
        state.outcomes = Some(outcomes);
        state
    }

    #[test]
    fn groups_two_distinct_policies_into_two_rows() {
        let default_policy = SdlcPolicy::default();
        let terse_policy = SdlcPolicy {
            output_verbosity: OutputVerbosity::Terse,
            ..SdlcPolicy::default()
        };

        let states = vec![
            state_with(
                default_policy.clone(),
                outcomes(OutcomeSpec {
                    cost: 1.0,
                    wall_clock: 100.0,
                    input_tok: 1000,
                    output_tok: 500,
                    attempts: 3,
                    retries: 0,
                    passed: 1,
                    failed: 0,
                    verdicts: &["ConsolidatedReviewNode:PASS"],
                }),
                "run-1",
            ),
            state_with(
                default_policy.clone(),
                outcomes(OutcomeSpec {
                    cost: 2.0,
                    wall_clock: 200.0,
                    input_tok: 2000,
                    output_tok: 1000,
                    attempts: 4,
                    retries: 1,
                    passed: 1,
                    failed: 0,
                    verdicts: &["ConsolidatedReviewNode:PASS"],
                }),
                "run-2",
            ),
            state_with(
                terse_policy.clone(),
                outcomes(OutcomeSpec {
                    cost: 0.5,
                    wall_clock: 50.0,
                    input_tok: 500,
                    output_tok: 250,
                    attempts: 3,
                    retries: 0,
                    passed: 0,
                    failed: 1,
                    verdicts: &["ConsolidatedReviewNode:FAIL"],
                }),
                "run-3",
            ),
        ];

        let rows = aggregate_states(&states);
        assert_eq!(rows.len(), 2);

        let default_row = rows
            .iter()
            .find(|r| r.policy == default_policy)
            .expect("default policy row present");
        assert_eq!(default_row.run_count, 2);
        assert_eq!(default_row.total_cost_usd, 3.0);
        assert_eq!(default_row.avg_cost_usd, 1.5);
        assert_eq!(default_row.total_wall_clock_secs, 300.0);
        assert_eq!(default_row.avg_wall_clock_secs, 150.0);
        assert_eq!(default_row.total_input_tokens, 3000);
        assert_eq!(default_row.total_output_tokens, 1500);
        assert_eq!(default_row.total_attempts, 7);
        assert_eq!(default_row.total_retries, 1);
        assert_eq!(default_row.total_tasks_passed, 2);
        assert_eq!(default_row.total_tasks_failed, 0);
        assert_eq!(default_row.pass_rate, 1.0);
        assert_eq!(
            default_row
                .review_verdict_counts
                .get("ConsolidatedReviewNode:PASS"),
            Some(&2)
        );

        let terse_row = rows
            .iter()
            .find(|r| r.policy == terse_policy)
            .expect("terse policy row present");
        assert_eq!(terse_row.run_count, 1);
        assert_eq!(terse_row.total_cost_usd, 0.5);
        assert_eq!(terse_row.avg_cost_usd, 0.5);
        assert_eq!(terse_row.total_tasks_passed, 0);
        assert_eq!(terse_row.total_tasks_failed, 1);
        assert_eq!(terse_row.pass_rate, 0.0);
        assert_eq!(
            terse_row
                .review_verdict_counts
                .get("ConsolidatedReviewNode:FAIL"),
            Some(&1)
        );
    }

    #[test]
    fn states_missing_policy_or_outcomes_are_skipped() {
        let mut no_outcomes = SDLCState::new("run-no-outcomes");
        no_outcomes.policy = Some(SdlcPolicy::default());
        // outcomes left None.

        let mut no_policy = SDLCState::new("run-no-policy");
        no_policy.outcomes = Some(outcomes(OutcomeSpec {
            cost: 1.0,
            wall_clock: 1.0,
            input_tok: 1,
            output_tok: 1,
            attempts: 1,
            retries: 0,
            passed: 1,
            failed: 0,
            verdicts: &[],
        }));
        // policy left None.

        let rows = aggregate_states(&[no_outcomes, no_policy]);
        assert!(rows.is_empty());
    }

    #[test]
    fn aggregate_state_files_reads_and_groups_fixtures_from_disk() {
        let dir = std::env::temp_dir().join(format!("en3c-aggregate-test-{}", std::process::id()));
        // Guarantee-empty: see `setup.rs`'s `temp_dir_named` doc comment for
        // why PID-recycling makes this removal necessary, not optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let policy_a = SdlcPolicy {
            model_tiers: super::super::policy::ModelTiers {
                implement: ModelTier::Haiku,
                ..SdlcPolicy::default().model_tiers
            },
            ..SdlcPolicy::default()
        };
        let state_a = state_with(
            policy_a.clone(),
            outcomes(OutcomeSpec {
                cost: 1.0,
                wall_clock: 10.0,
                input_tok: 100,
                output_tok: 50,
                attempts: 1,
                retries: 0,
                passed: 1,
                failed: 0,
                verdicts: &[],
            }),
            "fixture-a",
        );
        let state_b = state_with(
            policy_a.clone(),
            outcomes(OutcomeSpec {
                cost: 3.0,
                wall_clock: 30.0,
                input_tok: 300,
                output_tok: 150,
                attempts: 2,
                retries: 1,
                passed: 1,
                failed: 0,
                verdicts: &[],
            }),
            "fixture-b",
        );

        let path_a = dir.join("state-a.json");
        let path_b = dir.join("state-b.json");
        std::fs::write(
            &path_a,
            serde_json::to_string(&state_a).expect("serializes"),
        )
        .expect("write fixture a");
        std::fs::write(
            &path_b,
            serde_json::to_string(&state_b).expect("serializes"),
        )
        .expect("write fixture b");

        let rows = aggregate_state_files(&[&path_a, &path_b]).expect("aggregates from disk");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_count, 2);
        assert_eq!(rows[0].total_cost_usd, 4.0);
        assert_eq!(rows[0].avg_cost_usd, 2.0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
