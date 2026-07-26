//! `CriticRouterNode` — the bounded self-critic loop's guard (EN.5.A task
//! 8), per `planning/EN.5.A-content-pipeline/architecture.md` §4.
//!
//! `Router::route` takes `&TaskContext` and cannot mutate it (`routing.rs`
//! contract), so this node's `process` is a pure pass-through: every input
//! `route` needs was stored by an earlier `process()` call —
//! `SelfCriticNode`'s `CriticEvaluation`, `SourceRouterNode`'s resolved
//! `ContentPipelinePolicy` (loop bounds), and
//! `IncrementCriticIterationNode`'s running counter (0 when absent, i.e.
//! before the first revise pass).
//!
//! ## Exit condition
//!
//! `route` exits to `TranslateSkipRouterNode` when any of:
//! - the critic's verdict is `Pass`;
//! - the critic's `confidence` meets or exceeds the resolved
//!   `critic_confidence_threshold`;
//! - the cap is reached — see "Iteration semantics" below.
//!
//! Otherwise it routes to `IncrementCriticIterationNode` (the back-edge:
//! bump -> `ReviseNode` -> `SelfCriticNode` -> back here).
//!
//! ## Iteration semantics (2026-07-25 tasks.md review — pinned)
//!
//! `CriticEvaluation.iteration` is 0-based and counts revisions completed
//! *before* the pass it is stamped on, so a critic pass's 1-based ordinal
//! is `iteration + 1`. `max_critic_iterations = N` must yield **exactly N**
//! critic passes when the cap (not a verdict/confidence exit) governs: the
//! cap fires — routes to `TranslateSkipRouterNode` instead of continuing —
//! once the pass just evaluated is the Nth one, i.e. `iteration + 1 >= N`.
//! Checking `iteration >= N` directly (an easy off-by-one) would let the
//! loop run `N + 1` passes before capping, so this uses the
//! `saturating_add(1)` form explicitly.
//!
//! Example, `max_critic_iterations = 2`: pass 1 has `iteration = 0`
//! (`0 + 1 = 1 < 2`, continue); the back-edge bumps the counter to 1; pass 2
//! has `iteration = 1` (`1 + 1 = 2 >= 2`, cap fires) — exactly 2 critic
//! passes, and the loop cannot re-enter.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::routing::Router;
use crate::workflows::get_result;

use super::increment_critic_iteration;
use super::policy::ContentPipelinePolicy;
use super::schema::{CriticEvaluation, CriticVerdict};
use super::self_critic;
use super::source_router;

/// The `Node::name()` identity `CriticRouterNode` is registered under.
pub const NODE_NAME: &str = "CriticRouterNode";

/// The identity the loop exits to once `route` decides `done`. Task 10 (
/// `translate.rs`) is still a scaffold at this task's authoring time, so
/// this is a string literal — not an import — mirroring `self_critic.rs`'s
/// `REVISE_NODE_NAME` convention (avoids a forward dependency on an
/// unwritten module).
const TRANSLATE_SKIP_ROUTER_NODE_NAME: &str = "TranslateSkipRouterNode";

/// Reads the `ContentPipelinePolicy` `SourceRouterNode` resolved and stored
/// inline under its own identity (task 4). Mirrors
/// `self_critic::resolved_policy` — a channel envelope has no worktree to
/// resolve `RESOLVED_POLICY_IDENTITY` against, so this stage does not use
/// `crate::policy::resolved_policy_strict`.
fn resolved_policy(ctx: &TaskContext) -> Option<ContentPipelinePolicy> {
    let stored = get_result(ctx, source_router::NODE_NAME)?;
    let policy = stored.get("policy")?.clone();
    serde_json::from_value(policy).ok()
}

/// Read the current loop pass from `IncrementCriticIterationNode`'s stored
/// counter, defaulting to 0 when absent (no revise pass has happened yet).
fn read_iteration(ctx: &TaskContext) -> u32 {
    get_result(ctx, increment_critic_iteration::NODE_NAME)
        .and_then(|value| value.get("iteration"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32
}

/// The bounded self-critic loop's guard. Forwards to
/// `IncrementCriticIterationNode` (continue) or `TranslateSkipRouterNode`
/// (exit).
pub struct CriticRouterNode;

#[async_trait::async_trait]
impl Node for CriticRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for CriticRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let stored = get_result(ctx, self_critic::NODE_NAME)?;
        let evaluation: CriticEvaluation = serde_json::from_value(stored.clone()).ok()?;
        let policy = resolved_policy(ctx)?;
        let iteration = read_iteration(ctx);

        // This pass's 1-based ordinal — see the "Iteration semantics" doc
        // comment above for why this is `iteration + 1`, not `iteration`.
        let passes_so_far = iteration.saturating_add(1);

        let done = matches!(evaluation.verdict, CriticVerdict::Pass)
            || evaluation.confidence >= policy.critic_confidence_threshold
            || passes_so_far >= policy.max_critic_iterations;

        Some(if done {
            TRANSLATE_SKIP_ROUTER_NODE_NAME.to_string()
        } else {
            increment_critic_iteration::NODE_NAME.to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn ctx_with(
        verdict: &str,
        confidence: f64,
        stored_iteration: Option<u32>,
        policy: ContentPipelinePolicy,
    ) -> TaskContext {
        let mut nodes = HashMap::new();
        nodes.insert(
            self_critic::NODE_NAME.to_string(),
            json!({
                "verdict": verdict,
                "confidence": confidence,
                "issues": [],
                "iteration": stored_iteration.unwrap_or(0),
            }),
        );
        nodes.insert(
            source_router::NODE_NAME.to_string(),
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": policy,
            }),
        );
        if let Some(iteration) = stored_iteration {
            nodes.insert(
                increment_critic_iteration::NODE_NAME.to_string(),
                json!({ "iteration": iteration }),
            );
        }
        TaskContext {
            event: json!({}),
            nodes,
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn route_exits_on_pass_verdict_even_at_iteration_zero() {
        let router = CriticRouterNode;
        let ctx = ctx_with("pass", 0.5, None, ContentPipelinePolicy::default());

        assert_eq!(
            router.route(&ctx),
            Some("TranslateSkipRouterNode".to_string())
        );
    }

    #[test]
    fn route_exits_when_confidence_meets_the_threshold_despite_revise_verdict() {
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: 0.8,
            ..ContentPipelinePolicy::default()
        };
        let router = CriticRouterNode;
        let ctx = ctx_with("revise", 0.8, None, policy);

        assert_eq!(
            router.route(&ctx),
            Some("TranslateSkipRouterNode".to_string())
        );
    }

    #[test]
    fn route_continues_to_increment_when_revise_and_below_both_gates() {
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: 0.8,
            max_critic_iterations: 3,
            ..ContentPipelinePolicy::default()
        };
        let router = CriticRouterNode;
        let ctx = ctx_with("revise", 0.4, None, policy);

        assert_eq!(
            router.route(&ctx),
            Some("IncrementCriticIterationNode".to_string())
        );
    }

    #[test]
    fn route_to_revise_is_impossible_once_the_cap_is_reached() {
        // max_critic_iterations = 2: pass 2 (iteration = 1) is the cap.
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: 0.99,
            max_critic_iterations: 2,
            ..ContentPipelinePolicy::default()
        };
        let router = CriticRouterNode;
        let ctx = ctx_with("revise", 0.1, Some(1), policy);

        assert_eq!(
            router.route(&ctx),
            Some("TranslateSkipRouterNode".to_string())
        );
    }

    #[test]
    fn cap_fires_at_exactly_n_critic_passes_not_n_plus_one() {
        // max_critic_iterations = 2, confidence never meets the threshold.
        // Simulates the loop's first two visits to the router directly off
        // the increment node's counter (0, then 1).
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: 0.99,
            max_critic_iterations: 2,
            ..ContentPipelinePolicy::default()
        };
        let router = CriticRouterNode;

        // Pass 1: iteration = 0 -> 1 pass so far, below the cap -> continue.
        let pass_1 = ctx_with("revise", 0.1, Some(0), policy.clone());
        assert_eq!(
            router.route(&pass_1),
            Some("IncrementCriticIterationNode".to_string())
        );

        // Pass 2: iteration = 1 (after one back-edge bump) -> 2 passes so
        // far, cap reached -> exit. Exactly 2 total critic passes.
        let pass_2 = ctx_with("revise", 0.1, Some(1), policy);
        assert_eq!(
            router.route(&pass_2),
            Some("TranslateSkipRouterNode".to_string())
        );
    }

    #[test]
    fn an_early_pass_verdict_exits_without_ever_revising() {
        let router = CriticRouterNode;
        let ctx = ctx_with("pass", 0.5, Some(0), ContentPipelinePolicy::default());

        assert_eq!(
            router.route(&ctx),
            Some("TranslateSkipRouterNode".to_string())
        );
    }

    #[test]
    fn route_returns_none_when_self_critic_has_not_run() {
        let router = CriticRouterNode;
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            source_router::NODE_NAME.to_string(),
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": ContentPipelinePolicy::default(),
            }),
        );

        assert_eq!(router.route(&ctx), None);
    }

    #[test]
    fn route_returns_none_when_no_policy_is_stored() {
        let router = CriticRouterNode;
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            self_critic::NODE_NAME.to_string(),
            json!({ "verdict": "pass", "confidence": 0.9, "issues": [], "iteration": 0 }),
        );

        assert_eq!(router.route(&ctx), None);
    }

    #[tokio::test]
    async fn process_is_a_pure_passthrough() {
        let router = CriticRouterNode;
        let ctx = ctx_with("pass", 0.9, None, ContentPipelinePolicy::default());
        let before = ctx.nodes.clone();

        let after = router.process(ctx).await.expect("process should succeed");

        assert_eq!(after.nodes, before);
    }

    #[test]
    fn route_never_mutates_ctx() {
        // `Router::route` takes `&TaskContext` — the compiler already
        // forbids mutation through this signature; this asserts the
        // observable contract by comparing before/after snapshots across
        // several `route` calls.
        let router = CriticRouterNode;
        let ctx = ctx_with("revise", 0.2, Some(0), ContentPipelinePolicy::default());
        let before = ctx.clone();

        let _ = router.route(&ctx);
        let _ = router.route(&ctx);

        assert_eq!(ctx.nodes, before.nodes);
        assert_eq!(ctx.metadata, before.metadata);
        assert_eq!(ctx.event, before.event);
    }

    #[test]
    fn as_router_is_some() {
        let router = CriticRouterNode;
        assert!(router.as_router().is_some());
    }
}
