//! Generic cross-run `(policy -> cost, time, quality)` aggregator, lifted
//! from `workflows::sdlc_flow::aggregate` (EN.4.0 task 2). Generic over any
//! policy type `P: Policy` (plus the bounds needed to group and clone it) —
//! groups `(policy, telemetry)` pairs by resolved policy and tabulates
//! summed/averaged cost, token, wall-clock, retry, and review-verdict
//! columns per distinct policy.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::resolve::Policy;
use super::telemetry::RunTelemetry;

/// One row of the cross-run aggregation table: a distinct resolved policy
/// `P` plus the summed/averaged outcome metrics across every run that
/// resolved to it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PolicyAggregate<P> {
    /// The resolved policy this row summarizes.
    pub policy: P,
    /// Number of runs that resolved to this exact policy.
    pub run_count: usize,
    /// Sum of `RunTelemetry::total_cost_usd` across every run in this group.
    pub total_cost_usd: f64,
    /// `total_cost_usd / run_count`.
    pub avg_cost_usd: f64,
    /// Sum of `RunTelemetry::wall_clock_secs` across every run in this group.
    pub total_wall_clock_secs: f64,
    /// `total_wall_clock_secs / run_count`.
    pub avg_wall_clock_secs: f64,
    /// Sum of `RunTelemetry::total_input_tokens`.
    pub total_input_tokens: u64,
    /// Sum of `RunTelemetry::total_output_tokens`.
    pub total_output_tokens: u64,
    /// Sum of `RunTelemetry::total_attempts`.
    pub total_attempts: u32,
    /// Sum of `RunTelemetry::total_retries`.
    pub total_retries: u32,
    /// Sum of `RunTelemetry::tasks_passed` (quality numerator).
    pub total_tasks_passed: u32,
    /// Sum of `RunTelemetry::tasks_failed` (quality denominator, with
    /// `total_tasks_passed`).
    pub total_tasks_failed: u32,
    /// `total_tasks_passed / (total_tasks_passed + total_tasks_failed)`, or
    /// `0.0` if no tasks were recorded in this group.
    pub pass_rate: f64,
    /// Tally of every `review_verdicts` entry observed across the group's
    /// runs (e.g. `"ConsolidatedReviewNode:PASS" -> 4`).
    pub review_verdict_counts: BTreeMap<String, u32>,
}

/// Canonical grouping key for a resolved policy: its serde-serialized JSON,
/// so two policies are "the same" for aggregation purposes iff they'd
/// serialize identically.
fn policy_key<P: Serialize>(policy: &P) -> String {
    serde_json::to_string(policy).expect("policy always serializes")
}

/// Group `(policy, telemetry)` pairs by resolved policy and tabulate one
/// [`PolicyAggregate`] row per distinct policy. Rows are returned sorted by
/// their policy's canonical JSON key, for deterministic output.
#[must_use]
pub fn aggregate<P>(runs: &[(P, RunTelemetry)]) -> Vec<PolicyAggregate<P>>
where
    P: Policy + Serialize + Clone,
{
    let mut groups: BTreeMap<String, PolicyAggregate<P>> = BTreeMap::new();

    for (policy, telemetry) in runs {
        let key = policy_key(policy);
        let row = groups.entry(key).or_insert_with(|| PolicyAggregate {
            policy: policy.clone(),
            run_count: 0,
            total_cost_usd: 0.0,
            avg_cost_usd: 0.0,
            total_wall_clock_secs: 0.0,
            avg_wall_clock_secs: 0.0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_attempts: 0,
            total_retries: 0,
            total_tasks_passed: 0,
            total_tasks_failed: 0,
            pass_rate: 0.0,
            review_verdict_counts: BTreeMap::new(),
        });

        row.run_count += 1;
        row.total_cost_usd += telemetry.total_cost_usd;
        row.total_wall_clock_secs += telemetry.wall_clock_secs;
        row.total_input_tokens += telemetry.total_input_tokens;
        row.total_output_tokens += telemetry.total_output_tokens;
        row.total_attempts += telemetry.total_attempts;
        row.total_retries += telemetry.total_retries;
        row.total_tasks_passed += telemetry.tasks_passed;
        row.total_tasks_failed += telemetry.tasks_failed;
        for verdict in &telemetry.review_verdicts {
            *row.review_verdict_counts
                .entry(verdict.clone())
                .or_insert(0) += 1;
        }
    }

    let mut rows: Vec<PolicyAggregate<P>> = groups.into_values().collect();
    for row in &mut rows {
        row.avg_cost_usd = row.total_cost_usd / row.run_count as f64;
        row.avg_wall_clock_secs = row.total_wall_clock_secs / row.run_count as f64;
        let total_tasks = row.total_tasks_passed + row.total_tasks_failed;
        row.pass_rate = if total_tasks > 0 {
            f64::from(row.total_tasks_passed) / f64::from(total_tasks)
        } else {
            0.0
        };
    }
    rows
}

/// Read a set of on-disk JSON state files and aggregate them via
/// [`aggregate`]. Each file is parsed as a raw [`serde_json::Value`] and
/// handed to `extract`, which pulls a `(policy, telemetry)` pair out of it
/// (returning `None` to skip a state missing either block) — this keeps
/// the function agnostic of any workflow's concrete state shape (e.g.
/// `sdlc_flow::schema::SDLCState`). Returns an `io::Error` if any file
/// can't be read or fails to parse as JSON.
pub fn aggregate_state_files<P, F>(
    paths: &[impl AsRef<Path>],
    extract: F,
) -> std::io::Result<Vec<PolicyAggregate<P>>>
where
    P: Policy + Serialize + Clone,
    F: Fn(&serde_json::Value) -> Option<(P, RunTelemetry)>,
{
    let mut runs = Vec::with_capacity(paths.len());
    for path in paths {
        let content = std::fs::read_to_string(path.as_ref())?;
        let value: serde_json::Value = serde_json::from_str(&content).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {err}", path.as_ref().display()),
            )
        })?;
        if let Some(run) = extract(&value) {
            runs.push(run);
        }
    }
    Ok(aggregate(&runs))
}

/// Convenience wrapper around [`aggregate_state_files`] for the common case
/// where a state's policy/telemetry blocks are plain optional fields keyed
/// `policy_field`/`telemetry_field` at the top level of the state JSON.
#[must_use]
pub fn extract_policy_telemetry<P: DeserializeOwned>(
    value: &serde_json::Value,
    policy_field: &str,
    telemetry_field: &str,
) -> Option<(P, RunTelemetry)> {
    let policy = value.get(policy_field)?.clone();
    let telemetry = value.get(telemetry_field)?.clone();
    if policy.is_null() || telemetry.is_null() {
        return None;
    }
    let policy: P = serde_json::from_value(policy).ok()?;
    let telemetry: RunTelemetry = serde_json::from_value(telemetry).ok()?;
    Some((policy, telemetry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::resolve::merge_opt;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
    struct TestPolicy {
        retries: u32,
    }

    #[derive(Debug, Clone, Default, serde::Deserialize)]
    struct PartialTestPolicy {
        retries: Option<u32>,
    }

    impl Policy for TestPolicy {
        type Partial = PartialTestPolicy;

        fn apply(self, over: &Self::Partial) -> Self {
            Self {
                retries: merge_opt(self.retries, over.retries),
            }
        }
    }

    fn telemetry(
        cost: f64,
        wall_clock: f64,
        passed: u32,
        failed: u32,
        verdicts: &[&str],
    ) -> RunTelemetry {
        RunTelemetry {
            wall_clock_secs: wall_clock,
            total_attempts: passed + failed,
            total_retries: 0,
            tasks_passed: passed,
            tasks_failed: failed,
            review_verdicts: verdicts.iter().map(|s| s.to_string()).collect(),
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_cost_usd: cost,
            model_tier_used: BTreeMap::new(),
        }
    }

    #[test]
    fn groups_two_distinct_policies_into_two_rows() {
        let p1 = TestPolicy { retries: 1 };
        let p2 = TestPolicy { retries: 2 };
        let runs = vec![
            (p1, telemetry(1.0, 10.0, 1, 0, &["A:PASS"])),
            (p1, telemetry(2.0, 20.0, 1, 0, &["A:PASS"])),
            (p2, telemetry(0.5, 5.0, 0, 1, &["A:FAIL"])),
        ];

        let rows = aggregate(&runs);
        assert_eq!(rows.len(), 2);

        let row1 = rows.iter().find(|r| r.policy == p1).unwrap();
        assert_eq!(row1.run_count, 2);
        assert_eq!(row1.total_cost_usd, 3.0);
        assert_eq!(row1.avg_cost_usd, 1.5);
        assert_eq!(row1.total_wall_clock_secs, 30.0);
        assert_eq!(row1.avg_wall_clock_secs, 15.0);
        assert_eq!(row1.total_tasks_passed, 2);
        assert_eq!(row1.pass_rate, 1.0);
        assert_eq!(row1.review_verdict_counts.get("A:PASS"), Some(&2));

        let row2 = rows.iter().find(|r| r.policy == p2).unwrap();
        assert_eq!(row2.run_count, 1);
        assert_eq!(row2.pass_rate, 0.0);
    }

    #[test]
    fn empty_runs_yields_no_rows() {
        let rows: Vec<PolicyAggregate<TestPolicy>> = aggregate(&[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn aggregate_state_files_reads_and_groups_fixtures_from_disk() {
        let dir =
            std::env::temp_dir().join(format!("en4-policy-aggregate-test-{}", std::process::id()));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let policy = TestPolicy { retries: 3 };
        let state_a = serde_json::json!({
            "policy": policy,
            "outcomes": telemetry(1.0, 10.0, 1, 0, &[]),
        });
        let state_b = serde_json::json!({
            "policy": policy,
            "outcomes": telemetry(3.0, 30.0, 1, 0, &[]),
        });
        let state_missing = serde_json::json!({ "policy": policy });

        let path_a = dir.join("a.json");
        let path_b = dir.join("b.json");
        let path_missing = dir.join("missing.json");
        std::fs::write(&path_a, state_a.to_string()).unwrap();
        std::fs::write(&path_b, state_b.to_string()).unwrap();
        std::fs::write(&path_missing, state_missing.to_string()).unwrap();

        let rows = aggregate_state_files(&[&path_a, &path_b, &path_missing], |value| {
            extract_policy_telemetry::<TestPolicy>(value, "policy", "outcomes")
        })
        .expect("aggregates from disk");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_count, 2);
        assert_eq!(rows[0].total_cost_usd, 4.0);
        assert_eq!(rows[0].avg_cost_usd, 2.0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
