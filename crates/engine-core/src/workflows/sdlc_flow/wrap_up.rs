//! `WrapUpNode` — deterministic wrap-up template render (bottom-half, EN.3.B).
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/wrap_up_node.py`:
//! a deterministic `Node` (no model call) that reads the latest durable
//! `SDLCState`, computes a `PASS`/`PARTIAL/FAIL` outcome from its telemetry,
//! and renders three text artifacts — `log_entry`, `report`,
//! `status_suggestion` — from Rust string templates. This node is a
//! MAJOR_BAIL / structural-fail terminal target of `TriageRouterNode` and
//! `ReviewRouterNode` as well as the natural end of a fully-passing run, so
//! it must tolerate being reached with any telemetry shape.
//!
//! Writes no files itself (the templates are handed back for a later
//! step/human) — mirrors the Python node exactly.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::policy::SdlcPolicy;
use super::schema::{RunOutcomes, SDLCState};
use super::setup::RESOLVED_POLICY_IDENTITY;
use super::{get_result, put_result};

/// Injectable "today" clock seam so the rendered date is deterministic
/// under test. Defaults to the real current UTC date
/// (`YYYY-MM-DD`); tests substitute a fixed date.
pub type ClockFn = Arc<dyn Fn() -> String + Send + Sync>;

/// The default [`ClockFn`]: today's real date in `YYYY-MM-DD` (UTC),
/// computed without pulling in a chrono dependency — days-since-epoch civil
/// conversion (Howard Hinnant's algorithm).
#[must_use]
pub fn default_clock() -> ClockFn {
    Arc::new(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let days = now.as_secs() as i64 / 86_400;
        civil_from_days(days)
    })
}

/// Convert a days-since-1970-01-01 count into a `YYYY-MM-DD` string.
/// Standard proleptic-Gregorian civil-from-days algorithm.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Resolve the most recently mutated `SDLCState`: `UpdateTaskStatusNode`'s
/// output if the loop has run at least once, else `LoadTaskStateNode`'s
/// initial load. Mirrors the equivalent helper in `task_loop.rs` (kept as a
/// local copy since it is not part of the hoisted `mod.rs` seams and this
/// module must not touch `mod.rs`).
fn latest_state(ctx: &TaskContext) -> Result<SDLCState, NodeError> {
    let value = get_result(ctx, "UpdateTaskStatusNode")
        .or_else(|| get_result(ctx, "LoadTaskStateNode"))
        .ok_or_else(|| {
            NodeError::new(
                "no SDLCState found: neither UpdateTaskStatusNode nor LoadTaskStateNode has run",
            )
        })?;
    serde_json::from_value(value.clone())
        .map_err(|err| NodeError::new(format!("failed to parse SDLCState: {err}")))
}

/// Read the resolved `SdlcPolicy` stamped by `SetupWorktreeNode`
/// (`setup::RESOLVED_POLICY_IDENTITY`). Falls back to the built-in default
/// when absent or unparsable — the same defensive fallback `task_loop.rs`
/// uses (kept as a local copy per this module's no-`mod.rs`-touch rule).
fn resolved_policy(ctx: &TaskContext) -> SdlcPolicy {
    get_result(ctx, RESOLVED_POLICY_IDENTITY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// Wall-clock seconds from `SetupWorktreeNode`'s framework-stamped
/// `started_at` (contract §6) to `now`. `0.0` if the run never went through
/// `SetupWorktreeNode` (e.g. a unit test driving `WrapUpNode` directly).
fn wall_clock_secs(ctx: &TaskContext, now: chrono::DateTime<Utc>) -> f64 {
    ctx.node_runs
        .get("SetupWorktreeNode")
        .and_then(|run| run.started_at)
        .map(|started| (now - started).num_milliseconds().max(0) as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// Sum of every task's attempts beyond its first — total retries triggered
/// by `RETRYABLE`/minor-`FAIL` back-edges.
fn total_retries(state: &SDLCState) -> u32 {
    state
        .tasks
        .iter()
        .map(|task| task.attempt_count.saturating_sub(1))
        .sum()
}

/// The verdict-bearing model-judgment stages this snapshot inspects, in
/// declared run order.
const VERDICT_STAGES: [&str; 2] = ["TriageTaskNode", "ConsolidatedReviewNode"];

/// Collect `"<stage>:<verdict>"` entries for every verdict-bearing stage
/// that has run by the time this snapshot is taken.
fn review_verdicts(ctx: &TaskContext) -> Vec<String> {
    VERDICT_STAGES
        .iter()
        .filter_map(|stage| {
            get_result(ctx, stage)
                .and_then(|value| value.get("verdict").and_then(|v| v.as_str()))
                .map(|verdict| format!("{stage}:{verdict}"))
        })
        .collect()
}

/// Sum the input/output tokens last recorded in `ctx.node_runs` (contract
/// §6 `Usage`) across every node that ran a model this run.
fn total_tokens(ctx: &TaskContext) -> (u64, u64) {
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

/// The model-node identities whose `ctx.nodes` output may carry a
/// `"cost_usd"` field (`ClaudeCodeStep`'s output shape).
const COST_BEARING_STAGES: [&str; 4] = [
    "ImplementTaskNode",
    "TriageTaskNode",
    "ConsolidatedReviewNode",
    "GenerateTasksNode",
];

/// Sum every model-bearing stage's last recorded `cost_usd`.
fn total_cost_usd(ctx: &TaskContext) -> f64 {
    COST_BEARING_STAGES
        .iter()
        .filter_map(|stage| {
            get_result(ctx, stage).and_then(|value| value.get("cost_usd")?.as_f64())
        })
        .sum()
}

/// The resolved policy's per-stage tier, keyed by `ModelTiers` field name —
/// "actually used" for this run (`registry_for_policy` wires the stage's
/// transport from exactly this assignment; a local-endpoint failure falls
/// back to cloud per-call without changing the resolved policy snapshot).
fn model_tier_used(policy: &SdlcPolicy) -> BTreeMap<String, String> {
    let tiers = &policy.model_tiers;
    let tier_str = |tier: super::policy::ModelTier| {
        serde_json::to_value(tier)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    BTreeMap::from([
        ("implement".to_string(), tier_str(tiers.implement)),
        (
            "implement_simple".to_string(),
            tier_str(tiers.implement_simple),
        ),
        ("triage".to_string(), tier_str(tiers.triage)),
        ("review".to_string(), tier_str(tiers.review)),
        ("generate".to_string(), tier_str(tiers.generate)),
    ])
}

/// Finalize the resolved-policy snapshot + outcome-metrics block for a
/// completed (or bailed) run (EN.3.C task 6): deterministic — reads only
/// `ctx`'s already-accumulated `SDLCState`/`node_runs`/`nodes`, spends no
/// model tokens, and stays a pure function of `ctx` + `now` so tests can
/// pin the clock.
fn finalize_outcomes(
    ctx: &TaskContext,
    state: &SDLCState,
    policy: &SdlcPolicy,
    now: chrono::DateTime<Utc>,
) -> RunOutcomes {
    let (total_input_tokens, total_output_tokens) = total_tokens(ctx);
    RunOutcomes {
        wall_clock_secs: wall_clock_secs(ctx, now),
        total_attempts: state.telemetry.total_attempts,
        total_retries: total_retries(state),
        tasks_passed: state.telemetry.tasks_passed,
        tasks_failed: state.telemetry.tasks_failed,
        review_verdicts: review_verdicts(ctx),
        total_input_tokens,
        total_output_tokens,
        total_cost_usd: total_cost_usd(ctx),
        model_tier_used: model_tier_used(policy),
    }
}

/// Deterministic node: renders the wrap-up artifacts for a completed (or
/// bailed) SDLC flow run. No model call, no file writes.
pub struct WrapUpNode {
    clock: ClockFn,
}

impl WrapUpNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: default_clock(),
        }
    }

    /// Override the "today" clock. Tests use this to render against a fixed
    /// date.
    #[must_use]
    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }
}

impl Default for WrapUpNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for WrapUpNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut state = latest_state(&ctx)?;
        let date = (self.clock)();

        let policy = resolved_policy(&ctx);
        let outcomes = finalize_outcomes(&ctx, &state, &policy, Utc::now());
        state.policy = Some(policy);
        state.outcomes = Some(outcomes);

        let spec_slug = &state.spec_slug;
        let tasks_passed = state.telemetry.tasks_passed;
        let tasks_failed = state.telemetry.tasks_failed;
        let total_attempts = state.telemetry.total_attempts;
        let outcome = if tasks_failed == 0 {
            "PASS"
        } else {
            "PARTIAL/FAIL"
        };

        let log_entry = format!(
            "## {date} — {spec_slug}\n\n\
             Outcome: {outcome}. {tasks_passed} task(s) passed, {tasks_failed} failed, \
             {total_attempts} total implement/test attempt(s)."
        );

        let report = format!(
            "# SDLC Flow Report — {spec_slug}\n\n\
             - Date: {date}\n\
             - Outcome: {outcome}\n\
             - Tasks passed: {tasks_passed}\n\
             - Tasks failed: {tasks_failed}\n\
             - Total attempts: {total_attempts}\n"
        );

        let status_suggestion = if outcome == "PASS" {
            format!(
                "{spec_slug} completed successfully on {date} \
                 ({tasks_passed} task(s) passed, {total_attempts} total attempt(s)). \
                 Ready for review/merge."
            )
        } else {
            format!(
                "{spec_slug} did not complete cleanly on {date} \
                 ({tasks_passed} passed / {tasks_failed} failed, {total_attempts} total \
                 attempt(s)). Needs follow-up before merge."
            )
        };

        let state_value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;

        put_result(
            &mut ctx,
            "WrapUpNode",
            json!({
                "log_entry": log_entry,
                "report": report,
                "status_suggestion": status_suggestion,
                "state": state_value,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "WrapUpNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::schema::{SDLCState, SDLCTask, SDLCTaskStatus};
    use std::collections::HashMap;

    fn fixed_clock(date: &'static str) -> ClockFn {
        Arc::new(move || date.to_string())
    }

    fn ctx_with_state(state: &SDLCState) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({ "spec_slug": state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(state).unwrap(),
        );
        ctx
    }

    #[tokio::test]
    async fn wrap_up_renders_pass_outcome() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 3;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 4;

        let ctx = ctx_with_state(&state);
        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let log_entry = result["log_entry"].as_str().unwrap();
        let report = result["report"].as_str().unwrap();
        let status_suggestion = result["status_suggestion"].as_str().unwrap();

        assert!(log_entry.contains("2026-07-18"));
        assert!(log_entry.contains("EN.3.B-sdlc-flow-docs-wrapup-pr"));
        assert!(log_entry.contains("PASS"));
        assert!(log_entry.contains("3 task(s) passed"));
        assert!(log_entry.contains("0 failed"));
        assert!(log_entry.contains("4 total"));

        assert!(report.contains("Outcome: PASS"));
        assert!(report.contains("Tasks passed: 3"));
        assert!(report.contains("Tasks failed: 0"));
        assert!(report.contains("Total attempts: 4"));

        assert!(status_suggestion.contains("completed successfully"));
        assert!(status_suggestion.contains("2026-07-18"));
    }

    #[tokio::test]
    async fn wrap_up_renders_partial_fail_outcome() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        state.telemetry.tasks_passed = 2;
        state.telemetry.tasks_failed = 1;
        state.telemetry.total_attempts = 5;

        let ctx = ctx_with_state(&state);
        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-19"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let log_entry = result["log_entry"].as_str().unwrap();
        let report = result["report"].as_str().unwrap();
        let status_suggestion = result["status_suggestion"].as_str().unwrap();

        assert!(log_entry.contains("PARTIAL/FAIL"));
        assert!(report.contains("Outcome: PARTIAL/FAIL"));
        assert!(report.contains("Tasks passed: 2"));
        assert!(report.contains("Tasks failed: 1"));
        assert!(report.contains("Total attempts: 5"));
        assert!(status_suggestion.contains("did not complete cleanly"));
        assert!(status_suggestion.contains("Needs follow-up"));
    }

    #[tokio::test]
    async fn wrap_up_falls_back_to_load_task_state_node() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        state.telemetry.tasks_passed = 0;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 0;

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&state).unwrap(),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-01-01"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        assert!(result["log_entry"].as_str().unwrap().contains("PASS"));
    }

    #[tokio::test]
    async fn wrap_up_stamps_resolved_policy_and_outcomes_into_state() {
        use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, SdlcPolicy};

        let mut state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        task.attempt_count = 2;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 2;

        let mut ctx = ctx_with_state(&state);
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                triage: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).unwrap(),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "PASS", "summary": "s", "issues": [] }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone())
            .expect("WrapUpNode output carries a parseable SDLCState");

        let stamped_policy = stamped_state.policy.expect("policy block populated");
        assert_eq!(stamped_policy, policy);

        let outcomes = stamped_state.outcomes.expect("outcomes block populated");
        assert_eq!(outcomes.total_attempts, 2);
        assert_eq!(outcomes.tasks_passed, 1);
        assert_eq!(outcomes.tasks_failed, 0);
        assert_eq!(outcomes.total_retries, 1);
        assert_eq!(
            outcomes.review_verdicts,
            vec!["ConsolidatedReviewNode:PASS".to_string()]
        );
        assert_eq!(outcomes.model_tier_used["triage"], "haiku");
        assert_eq!(outcomes.model_tier_used["implement"], "sonnet");
    }

    #[tokio::test]
    async fn wrap_up_falls_back_to_default_policy_when_none_stamped() {
        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let ctx = ctx_with_state(&state);

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone()).unwrap();
        assert_eq!(
            stamped_state.policy,
            Some(crate::workflows::sdlc_flow::policy::SdlcPolicy::default())
        );
        assert_eq!(stamped_state.outcomes.unwrap().wall_clock_secs, 0.0);
    }

    #[tokio::test]
    async fn wrap_up_sums_tokens_and_cost_across_model_nodes() {
        use engine_contract::{NodeRun, NodeRunStatus, Usage};

        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let mut ctx = ctx_with_state(&state);

        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "content": "x", "cost_usd": 0.01, "model": "claude-haiku-4-5" }),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({
                "content": "x",
                "cost_usd": 0.02,
                "model": "claude-sonnet-4-5",
                "verdict": "PASS",
            }),
        );
        ctx.node_runs.insert(
            "TriageTaskNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    model: "claude-haiku-4-5".to_string(),
                }),
            },
        );
        ctx.node_runs.insert(
            "ConsolidatedReviewNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(20),
                    output_tokens: Some(8),
                    model: "claude-sonnet-4-5".to_string(),
                }),
            },
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone()).unwrap();
        let outcomes = stamped_state.outcomes.unwrap();

        assert_eq!(outcomes.total_input_tokens, 30);
        assert_eq!(outcomes.total_output_tokens, 13);
        assert!((outcomes.total_cost_usd - 0.03).abs() < 1e-9);
    }

    #[tokio::test]
    async fn two_different_policies_yield_different_recorded_tiers() {
        use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, SdlcPolicy};

        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");

        let mut ctx_a = ctx_with_state(&state);
        ctx_a.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).unwrap(),
        );

        let mut ctx_b = ctx_with_state(&state);
        let policy_b = SdlcPolicy {
            model_tiers: ModelTiers {
                review: ModelTier::Local,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        ctx_b.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy_b).unwrap(),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out_a = node
            .process(ctx_a)
            .await
            .expect("process should succeed")
            .nodes["WrapUpNode"]
            .clone();
        let out_b = WrapUpNode::new()
            .with_clock(fixed_clock("2026-07-18"))
            .process(ctx_b)
            .await
            .expect("process should succeed")
            .nodes["WrapUpNode"]
            .clone();

        let state_a: SDLCState = serde_json::from_value(out_a["state"].clone()).unwrap();
        let state_b: SDLCState = serde_json::from_value(out_b["state"].clone()).unwrap();

        assert_eq!(
            state_a.outcomes.unwrap().model_tier_used["review"],
            "sonnet"
        );
        assert_eq!(state_b.outcomes.unwrap().model_tier_used["review"], "local");
    }

    #[test]
    fn default_clock_produces_a_plausible_date_string() {
        let clock = default_clock();
        let date = clock();
        assert_eq!(date.len(), 10);
        assert_eq!(date.chars().nth(4), Some('-'));
        assert_eq!(date.chars().nth(7), Some('-'));
    }
}
