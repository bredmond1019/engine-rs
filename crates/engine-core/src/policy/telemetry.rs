//! Generic run-telemetry harvester, lifted from
//! `workflows::sdlc_flow::wrap_up`'s `wall_clock_secs`/`total_tokens`/
//! `total_cost_usd`/`review_verdicts`/`model_tier_used`/`finalize_outcomes`
//! (EN.4.0 task 2). Generic over the [`TaskContext`] it reads: the
//! stage/cost-bearing node identities a workflow cares about are passed in
//! rather than hardcoded to SDLC's stage names, so any workflow can harvest
//! the same wall-clock/token/cost/verdict shape from its own `ctx`.
//!
//! The counters that don't come from `ctx` at all (`total_attempts`,
//! `total_retries`, `tasks_passed`, `tasks_failed` — SDLC derives these from
//! its own `SDLCState`/`SDLCTelemetry`, which this module knows nothing
//! about) are supplied by the caller via [`RunTelemetryInputs`] rather than
//! re-derived here.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;

/// The generic outcome-metrics snapshot for one run: the same shape as
/// `workflows::sdlc_flow::schema::RunOutcomes`, generalized so any workflow
/// can produce it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunTelemetry {
    /// Wall-clock seconds from the run's start node to the moment this
    /// snapshot was computed. `0.0` when the start time is unavailable.
    #[serde(default)]
    pub wall_clock_secs: f64,
    /// Total attempts made across every unit of work this run (caller-
    /// supplied; not derivable from `ctx` alone).
    #[serde(default)]
    pub total_attempts: u32,
    /// Total retries triggered across every unit of work this run
    /// (caller-supplied).
    #[serde(default)]
    pub total_retries: u32,
    /// Number of units of work that passed (caller-supplied).
    #[serde(default)]
    pub tasks_passed: u32,
    /// Number of units of work that failed (caller-supplied).
    #[serde(default)]
    pub tasks_failed: u32,
    /// `"<stage>:<verdict>"` entries for every verdict-bearing stage that
    /// has run by the time this snapshot is taken.
    #[serde(default)]
    pub review_verdicts: Vec<String>,
    /// Total input tokens summed across every node's last recorded usage
    /// in `ctx.node_runs`.
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Total output tokens summed across every node's last recorded usage
    /// in `ctx.node_runs`.
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Total dollar cost summed across every cost-bearing stage's last
    /// recorded `cost_usd` in `ctx.nodes`.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Per-stage model tier actually used this run, keyed by whatever
    /// identity the caller chooses (e.g. `SdlcPolicy::ModelTiers`' field
    /// names).
    #[serde(default)]
    pub model_tier_used: BTreeMap<String, String>,
}

/// The parts of a [`RunTelemetry`] snapshot a caller must supply because
/// they aren't derivable from `ctx` alone (workflow-specific state) or name
/// the `ctx` identities this harvest should read.
#[derive(Debug, Clone, Default)]
pub struct RunTelemetryInputs<'a> {
    /// The `ctx.node_runs` identity whose `started_at` anchors
    /// [`wall_clock_secs`]'s wall-clock measurement (e.g. `"SetupWorktreeNode"`).
    pub start_node_identity: &'a str,
    /// The verdict-bearing stages to collect `"<stage>:<verdict>"` entries
    /// from, in declared run order (see [`review_verdicts`]).
    pub verdict_stages: &'a [&'a str],
    /// The stages whose `ctx.nodes` output may carry a `"cost_usd"` field
    /// (see [`total_cost_usd`]).
    pub cost_bearing_stages: &'a [&'a str],
    /// Total attempts across every unit of work this run (caller-derived).
    pub total_attempts: u32,
    /// Total retries across every unit of work this run (caller-derived).
    pub total_retries: u32,
    /// Number of units of work that passed (caller-derived).
    pub tasks_passed: u32,
    /// Number of units of work that failed (caller-derived).
    pub tasks_failed: u32,
    /// Per-stage model tier actually used this run (caller-derived from the
    /// resolved policy).
    pub model_tier_used: BTreeMap<String, String>,
}

/// Wall-clock seconds from `ctx.node_runs[start_node_identity]`'s
/// framework-stamped `started_at` to `now`. `0.0` if that node never ran
/// (e.g. a unit test driving a node directly, with no run start recorded).
#[must_use]
pub fn wall_clock_secs(ctx: &TaskContext, start_node_identity: &str, now: DateTime<Utc>) -> f64 {
    ctx.node_runs
        .get(start_node_identity)
        .and_then(|run| run.started_at)
        .map(|started| (now - started).num_milliseconds().max(0) as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// Sum the input/output tokens last recorded in `ctx.node_runs` (contract
/// §6 `Usage`) across every node that ran a model this run.
#[must_use]
pub fn total_tokens(ctx: &TaskContext) -> (u64, u64) {
    ctx.node_runs
        .values()
        .fold((0u64, 0u64), |(inp, out), run| match &run.usage {
            Some(usage) => (
                inp + usage.input_tokens.unwrap_or(0),
                out + usage.output_tokens.unwrap_or(0),
            ),
            None => (inp, out),
        })
}

/// Sum every `cost_bearing_stages` entry's last recorded `cost_usd` out of
/// `ctx.nodes`.
#[must_use]
pub fn total_cost_usd(ctx: &TaskContext, cost_bearing_stages: &[&str]) -> f64 {
    cost_bearing_stages
        .iter()
        .filter_map(|stage| {
            ctx.nodes
                .get(*stage)
                .and_then(|value| value.get("cost_usd")?.as_f64())
        })
        .sum()
}

/// Collect `"<stage>:<verdict>"` entries for every `verdict_stages` entry
/// that has run by the time this snapshot is taken (reads
/// `ctx.nodes[stage]["verdict"]`).
#[must_use]
pub fn review_verdicts(ctx: &TaskContext, verdict_stages: &[&str]) -> Vec<String> {
    verdict_stages
        .iter()
        .filter_map(|stage| {
            ctx.nodes
                .get(*stage)
                .and_then(|value| value.get("verdict").and_then(|v| v.as_str()))
                .map(|verdict| format!("{stage}:{verdict}"))
        })
        .collect()
}

/// Harvest a full [`RunTelemetry`] snapshot: deterministic — reads only
/// `ctx`'s already-accumulated state plus the caller-supplied
/// [`RunTelemetryInputs`], spends no model tokens, and stays a pure
/// function of its inputs so tests can pin the clock.
#[must_use]
pub fn harvest(
    ctx: &TaskContext,
    now: DateTime<Utc>,
    inputs: RunTelemetryInputs<'_>,
) -> RunTelemetry {
    let (total_input_tokens, total_output_tokens) = total_tokens(ctx);
    RunTelemetry {
        wall_clock_secs: wall_clock_secs(ctx, inputs.start_node_identity, now),
        total_attempts: inputs.total_attempts,
        total_retries: inputs.total_retries,
        tasks_passed: inputs.tasks_passed,
        tasks_failed: inputs.tasks_failed,
        review_verdicts: review_verdicts(ctx, inputs.verdict_stages),
        total_input_tokens,
        total_output_tokens,
        total_cost_usd: total_cost_usd(ctx, inputs.cost_bearing_stages),
        model_tier_used: inputs.model_tier_used,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::{NodeRun, NodeRunStatus, Usage};
    use std::collections::HashMap;

    fn ctx_with(
        nodes: HashMap<String, serde_json::Value>,
        node_runs: HashMap<String, NodeRun>,
    ) -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes,
            metadata: serde_json::json!({}),
            node_runs,
        }
    }

    #[test]
    fn wall_clock_secs_is_zero_when_start_node_absent() {
        let ctx = ctx_with(HashMap::new(), HashMap::new());
        assert_eq!(wall_clock_secs(&ctx, "SetupWorktreeNode", Utc::now()), 0.0);
    }

    #[test]
    fn wall_clock_secs_measures_elapsed_time_from_start_node() {
        let started = Utc::now() - chrono::Duration::seconds(30);
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "SetupWorktreeNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: Some(started),
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );
        let ctx = ctx_with(HashMap::new(), node_runs);
        let secs = wall_clock_secs(
            &ctx,
            "SetupWorktreeNode",
            started + chrono::Duration::seconds(30),
        );
        assert!((secs - 30.0).abs() < 0.01);
    }

    #[test]
    fn total_tokens_sums_usage_across_every_node_run() {
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "A".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    model: "m".to_string(),
                }),
            },
        );
        node_runs.insert(
            "B".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(200),
                    output_tokens: None,
                    model: "m".to_string(),
                }),
            },
        );
        let ctx = ctx_with(HashMap::new(), node_runs);
        assert_eq!(total_tokens(&ctx), (300, 50));
    }

    #[test]
    fn total_cost_usd_sums_only_named_cost_bearing_stages() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "ImplementTaskNode".to_string(),
            serde_json::json!({ "cost_usd": 1.5 }),
        );
        nodes.insert(
            "OtherNode".to_string(),
            serde_json::json!({ "cost_usd": 99.0 }),
        );
        let ctx = ctx_with(nodes, HashMap::new());
        assert_eq!(total_cost_usd(&ctx, &["ImplementTaskNode"]), 1.5);
    }

    #[test]
    fn review_verdicts_collects_in_declared_stage_order() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "TriageTaskNode".to_string(),
            serde_json::json!({ "verdict": "RETRYABLE" }),
        );
        nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            serde_json::json!({ "verdict": "PASS" }),
        );
        let ctx = ctx_with(nodes, HashMap::new());
        let verdicts = review_verdicts(&ctx, &["TriageTaskNode", "ConsolidatedReviewNode"]);
        assert_eq!(
            verdicts,
            vec![
                "TriageTaskNode:RETRYABLE".to_string(),
                "ConsolidatedReviewNode:PASS".to_string(),
            ]
        );
    }

    #[test]
    fn review_verdicts_skips_stages_that_havent_run() {
        let ctx = ctx_with(HashMap::new(), HashMap::new());
        assert!(review_verdicts(&ctx, &["TriageTaskNode"]).is_empty());
    }

    #[test]
    fn harvest_assembles_full_snapshot_from_ctx_and_inputs() {
        let started = Utc::now() - chrono::Duration::seconds(10);
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "SetupWorktreeNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: Some(started),
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );
        node_runs.insert(
            "ImplementTaskNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    model: "m".to_string(),
                }),
            },
        );
        let mut nodes = HashMap::new();
        nodes.insert(
            "ImplementTaskNode".to_string(),
            serde_json::json!({ "cost_usd": 0.25 }),
        );
        nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            serde_json::json!({ "verdict": "PASS" }),
        );
        let ctx = ctx_with(nodes, node_runs);

        let mut model_tier_used = BTreeMap::new();
        model_tier_used.insert("implement".to_string(), "sonnet".to_string());

        let inputs = RunTelemetryInputs {
            start_node_identity: "SetupWorktreeNode",
            verdict_stages: &["ConsolidatedReviewNode"],
            cost_bearing_stages: &["ImplementTaskNode"],
            total_attempts: 2,
            total_retries: 1,
            tasks_passed: 1,
            tasks_failed: 0,
            model_tier_used: model_tier_used.clone(),
        };

        let telemetry = harvest(&ctx, started + chrono::Duration::seconds(10), inputs);
        assert!((telemetry.wall_clock_secs - 10.0).abs() < 0.01);
        assert_eq!(telemetry.total_attempts, 2);
        assert_eq!(telemetry.total_retries, 1);
        assert_eq!(telemetry.tasks_passed, 1);
        assert_eq!(telemetry.tasks_failed, 0);
        assert_eq!(
            telemetry.review_verdicts,
            vec!["ConsolidatedReviewNode:PASS".to_string()]
        );
        assert_eq!(telemetry.total_input_tokens, 10);
        assert_eq!(telemetry.total_output_tokens, 5);
        assert_eq!(telemetry.total_cost_usd, 0.25);
        assert_eq!(telemetry.model_tier_used, model_tier_used);
    }
}
