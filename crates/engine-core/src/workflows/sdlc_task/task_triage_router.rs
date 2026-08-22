//! `TaskTriageRouterNode` — the SDLC_TASK triage fork (port design T6).
//!
//! **This is a genuinely new router, not a policy pin on
//! `sdlc_flow::task_loop::TriageRouterNode`.** `sdlc_flow`'s router's
//! `PASS` arm routes to `ConsolidatedReviewNode` under BOTH
//! `ReviewMode::PerTask` and `ReviewMode::TrivialSkip` (its non-trivial
//! branch) — SDLC_TASK never registers a `ConsolidatedReviewNode` (it ships
//! no per-task review at all, see `sdlc-task-ships-no-docs-stage`), so an
//! operator selecting a profile that resolved `review_mode: per_task` would
//! route to an unregistered node and the walk would halt with no terminal
//! state. A policy value must never be able to name an unregistered node —
//! the same argument `EN.10.C` made for the closed `EngineKind`. This
//! router therefore has exactly three arms and reads nothing from
//! `SdlcPolicy::review_mode` at all.
//!
//! Verdict extraction mirrors `sdlc_flow::task_loop::TriageRouterNode::route`
//! verbatim (`get_result(ctx, "TriageTaskNode")` -> `"verdict"` -> `&str`).
//! The budget-exhausted `RETRYABLE` re-check in [`budget_exhausted`] mirrors
//! `TriageTaskNode::process`'s own `attempt_count >= max_attempts` bail gate
//! (`sdlc_flow::task_loop`, the `if attempt_count >= max_attempts` arm) —
//! same live-state source (`latest_state`, reused as-is rather than
//! re-derived), same comparison. `TriageTaskNode` already converts an
//! over-budget failure to `MAJOR_BAIL` before this router ever sees it, so
//! the re-check here is defensive: it exists so a `RETRYABLE` verdict that
//! somehow reaches this router already over budget still fails closed to
//! `LeanBookkeepNode` rather than looping through `IncrementAttemptNode`
//! forever.
//!
//! Three arms, and only three:
//! - `PASS` -> `"UpdateTaskStatusNode"`
//! - `RETRYABLE`, under the attempt budget -> `"IncrementAttemptNode"`
//! - `MAJOR_BAIL`, budget-exhausted `RETRYABLE`, and any unknown/unparseable
//!   verdict (including a missing `TriageTaskNode` result) ->
//!   `"LeanBookkeepNode"` — fail-closed, `Router::route` here never returns
//!   `None`.
//!
//! The bail arm stamps nothing new: `TriageTaskNode` already wrote the
//! `{"verdict": "MAJOR_BAIL", "reason": ...}` shape `sdlc_flow::wrap_up`'s
//! `derive_terminal_signal` reads to build a `TerminalSignal::MajorBail`,
//! which `derive_committed_status` maps to `"blocked"` — this router simply
//! routes to the node that will read it (`LeanBookkeepNode`, added by a
//! later task in this spec) without touching the stamp's shape.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::routing::Router;
use crate::workflows::sdlc_flow::task_loop::latest_state;

use super::get_result;

/// Deterministic router: branches on `TriageTaskNode`'s stored verdict into
/// SDLC_TASK's lean three-arm fork. See the module doc for the full
/// rationale and the fork it prevents.
pub struct TaskTriageRouterNode;

#[async_trait::async_trait]
impl Node for TaskTriageRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TaskTriageRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TaskTriageRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        // Mirrors `sdlc_flow::task_loop::TriageRouterNode::route`'s verdict
        // extraction verbatim, except a missing/unparseable result folds
        // into the `_` arm below rather than returning `None` — this
        // router must never end the walk with no terminal state.
        let verdict = get_result(ctx, "TriageTaskNode")
            .and_then(|triage| triage.get("verdict"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        match verdict {
            "PASS" => Some("UpdateTaskStatusNode".to_string()),
            "RETRYABLE" => {
                if budget_exhausted(ctx) {
                    Some("LeanBookkeepNode".to_string())
                } else {
                    Some("IncrementAttemptNode".to_string())
                }
            }
            // `MAJOR_BAIL`, and any unknown/unparseable verdict (including
            // no upstream `TriageTaskNode` result at all) fail closed to
            // `LeanBookkeepNode` rather than ever returning `None`.
            _ => Some("LeanBookkeepNode".to_string()),
        }
    }
}

/// Mirrors `TriageTaskNode::process`'s own attempt_count/max_attempts bail
/// gate reading (`sdlc_flow::task_loop`, the `if attempt_count >=
/// max_attempts` arm): same live-state source (`latest_state`, reused
/// as-is), same `current_task_id` resolution off `TaskQueueRouterNode`'s
/// stamp, same `>=` comparison. Do not re-derive the gate with different
/// rounding or a different state source.
///
/// `true` (fail-closed to the bookkeep arm) whenever the current task
/// cannot even be resolved — a missing `TaskQueueRouterNode` stamp, an
/// unparseable/absent durable state, or no matching task id — since
/// `Router::route` can never return `None` here.
fn budget_exhausted(ctx: &TaskContext) -> bool {
    let Some(current_task_id) = get_result(ctx, "TaskQueueRouterNode")
        .and_then(|value| value.get("current_task_id"))
        .and_then(serde_json::Value::as_u64)
    else {
        return true;
    };

    let Ok(state) = latest_state(ctx) else {
        return true;
    };

    let Some(task) = state
        .tasks
        .iter()
        .find(|task| u64::from(task.task_id) == current_task_id)
    else {
        return true;
    };

    u64::from(task.attempt_count) >= u64::from(task.max_attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::schema::{
        derive_committed_status, SDLCState, SDLCTask, TerminalSignal,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn ctx_with_triage(verdict: &str) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": verdict, "reason": "r" }),
        );
        ctx
    }

    fn ctx_with_current_task(task: &SDLCTask) -> TaskContext {
        let mut state = SDLCState::new("my-spec");
        state.tasks = vec![task.clone()];
        let mut ctx = empty_context(json!({ "spec_slug": "my-spec" }));
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&state).unwrap(),
        );
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({
                "current_task_id": task.task_id,
                "title": task.title,
                "description": task.description,
                "attempt_count": task.attempt_count,
                "max_attempts": task.max_attempts,
            }),
        );
        ctx
    }

    // --- the three arms -----------------------------------------------

    #[test]
    fn pass_routes_to_update_task_status() {
        let ctx = ctx_with_triage("PASS");
        let router = TaskTriageRouterNode;
        assert_eq!(router.route(&ctx), Some("UpdateTaskStatusNode".to_string()));
    }

    #[test]
    fn retryable_under_budget_routes_to_increment_attempt() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 0;
        task.max_attempts = 3;
        let mut ctx = ctx_with_current_task(&task);
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "RETRYABLE", "reason": "retry" }),
        );
        let router = TaskTriageRouterNode;
        assert_eq!(router.route(&ctx), Some("IncrementAttemptNode".to_string()));
    }

    #[test]
    fn major_bail_routes_to_lean_bookkeep() {
        let ctx = ctx_with_triage("MAJOR_BAIL");
        let router = TaskTriageRouterNode;
        assert_eq!(router.route(&ctx), Some("LeanBookkeepNode".to_string()));
    }

    // --- fail-closed: every verdict, plus unknown, is one of the three ---

    #[test]
    fn every_verdict_and_an_unknown_string_return_one_of_the_three_identities_never_none() {
        let router = TaskTriageRouterNode;
        let allowed = [
            "UpdateTaskStatusNode",
            "IncrementAttemptNode",
            "LeanBookkeepNode",
        ];
        for verdict in ["PASS", "RETRYABLE", "MAJOR_BAIL", "WAT", ""] {
            let mut task = SDLCTask::new(1, "One", "d1");
            task.attempt_count = 0;
            task.max_attempts = 3;
            let mut ctx = ctx_with_current_task(&task);
            ctx.nodes.insert(
                "TriageTaskNode".to_string(),
                json!({ "verdict": verdict, "reason": "r" }),
            );
            let routed = router.route(&ctx);
            assert!(
                routed.is_some(),
                "verdict {verdict:?} must never route to None"
            );
            let routed = routed.unwrap();
            assert!(
                allowed.contains(&routed.as_str()),
                "verdict {verdict:?} routed to unexpected identity {routed:?}"
            );
        }
    }

    #[test]
    fn missing_upstream_triage_result_still_fails_closed_to_lean_bookkeep() {
        let ctx = empty_context(json!({}));
        let router = TaskTriageRouterNode;
        assert_eq!(router.route(&ctx), Some("LeanBookkeepNode".to_string()));
    }

    // --- the router never names the review nodes SDLC_TASK doesn't have --

    #[test]
    fn router_never_returns_consolidated_review_or_review_router_for_any_input() {
        let router = TaskTriageRouterNode;
        for verdict in ["PASS", "RETRYABLE", "MAJOR_BAIL", "WAT"] {
            let mut task = SDLCTask::new(1, "One", "d1");
            task.attempt_count = 0;
            task.max_attempts = 3;
            let mut ctx = ctx_with_current_task(&task);
            ctx.nodes.insert(
                "TriageTaskNode".to_string(),
                json!({ "verdict": verdict, "reason": "r" }),
            );
            let routed = router.route(&ctx).unwrap();
            assert_ne!(routed, "ConsolidatedReviewNode");
            assert_ne!(routed, "ReviewRouterNode");
        }
    }

    // --- budget-exhausted RETRYABLE ------------------------------------

    #[test]
    fn budget_exhausted_retryable_routes_to_lean_bookkeep_not_increment_attempt() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 3;
        task.max_attempts = 3;
        let mut ctx = ctx_with_current_task(&task);
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "RETRYABLE", "reason": "retry" }),
        );
        let router = TaskTriageRouterNode;
        assert_eq!(router.route(&ctx), Some("LeanBookkeepNode".to_string()));
    }

    #[test]
    fn budget_exhausted_helper_reads_the_same_live_state_source_as_triage_task_node() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 2;
        task.max_attempts = 3;
        let ctx = ctx_with_current_task(&task);
        assert!(!budget_exhausted(&ctx));

        let mut exhausted_task = SDLCTask::new(1, "One", "d1");
        exhausted_task.attempt_count = 3;
        exhausted_task.max_attempts = 3;
        let exhausted_ctx = ctx_with_current_task(&exhausted_task);
        assert!(budget_exhausted(&exhausted_ctx));
    }

    // --- the bail-arm stamp shape the router relies on ------------------

    /// The router never mutates `TriageTaskNode`'s stamp; it just routes to
    /// the node (`LeanBookkeepNode`, added by a later task) that reads it.
    /// This pins the invariant it relies on: the exact
    /// `{"verdict": "MAJOR_BAIL", "reason": ...}` shape `TriageTaskNode`
    /// already stamps is what `TerminalSignal::MajorBail` carries, and
    /// `derive_committed_status` maps that signal to `"blocked"`.
    #[test]
    fn major_bail_terminal_signal_maps_to_blocked_status() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 3;
        task.max_attempts = 3;
        let mut state = SDLCState::new("my-spec");
        state.tasks = vec![task];

        let signal = TerminalSignal::MajorBail("Max attempts (3) reached.".to_string());
        assert_eq!(derive_committed_status(&state, Some(&signal)), "blocked");
    }

    #[test]
    fn sdlc_flows_triage_router_node_is_a_distinct_unmodified_type() {
        // Compile-time check that this module does not alias or shadow
        // `sdlc_flow`'s router — a different name, in a different module,
        // reused nowhere in this file.
        use crate::workflows::sdlc_flow::task_loop::TriageRouterNode;
        let _ = TriageRouterNode;
        let _ = TaskTriageRouterNode;
    }
}
