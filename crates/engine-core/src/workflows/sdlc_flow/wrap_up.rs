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

use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};

use super::schema::SDLCState;
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
        let state = latest_state(&ctx)?;
        let date = (self.clock)();

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

        put_result(
            &mut ctx,
            "WrapUpNode",
            json!({
                "log_entry": log_entry,
                "report": report,
                "status_suggestion": status_suggestion,
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

    #[test]
    fn default_clock_produces_a_plausible_date_string() {
        let clock = default_clock();
        let date = clock();
        assert_eq!(date.len(), 10);
        assert_eq!(date.chars().nth(4), Some('-'));
        assert_eq!(date.chars().nth(7), Some('-'));
    }
}
