//! `IncrementCriticIterationNode` — the counter bump on the bounded
//! self-critic loop's back-edge (EN.5.A task 8).
//!
//! Deterministic node: bumps the loop's `iteration` counter every time
//! `CriticRouterNode` routes here — i.e. once per revise pass.
//! `Router::route` takes `&TaskContext` and cannot mutate it (`routing.rs`
//! contract), so the bump cannot live in `CriticRouterNode`'s routing logic
//! itself — it must be a real `Node` sitting on the back-edge, between
//! `CriticRouterNode` and `ReviseNode` (mirrors
//! `sdlc_flow::task_loop::IncrementAttemptNode`). Unlike that node, this
//! one's forward hop to `ReviseNode` is a plain declared graph connection
//! (`graph.rs` / architecture.md §5), not a runtime `Router` edge — this
//! node's own successor is fixed statically.
//!
//! Stores `{"iteration": <u32>}` under its own identity — `SelfCriticNode`
//! and `CriticRouterNode` both read this back (0 when absent, i.e. before
//! the first revise pass has happened).

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

/// The `Node::name()` identity `IncrementCriticIterationNode` is registered
/// under, and the `ctx.nodes` key its bumped counter is stamped onto. Read
/// by `SelfCriticNode` and `CriticRouterNode`.
pub const NODE_NAME: &str = "IncrementCriticIterationNode";

/// `ctx.nodes[NODE_NAME]` key the running iteration count is stored under.
const ITERATION_KEY: &str = "iteration";

/// Deterministic counter-bump node on the self-critic loop's back-edge.
pub struct IncrementCriticIterationNode;

#[async_trait::async_trait]
impl Node for IncrementCriticIterationNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current = get_result(&ctx, NODE_NAME)
            .and_then(|value| value.get(ITERATION_KEY))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let bumped = current + 1;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({ ITERATION_KEY: bumped }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn process_starts_the_counter_at_one_when_absent() {
        let node = IncrementCriticIterationNode;
        let ctx = node
            .process(empty_ctx())
            .await
            .expect("process should succeed");

        assert_eq!(ctx.nodes[NODE_NAME], json!({ "iteration": 1 }));
    }

    #[tokio::test]
    async fn process_increments_an_existing_counter() {
        let node = IncrementCriticIterationNode;
        let mut ctx = empty_ctx();
        put_result(&mut ctx, NODE_NAME, json!({ "iteration": 1 }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(ctx.nodes[NODE_NAME], json!({ "iteration": 2 }));
    }

    #[tokio::test]
    async fn process_is_monotonic_across_repeated_back_edge_visits() {
        let node = IncrementCriticIterationNode;
        let mut ctx = empty_ctx();

        let mut counts = Vec::new();
        for _ in 0..4 {
            ctx = node.process(ctx).await.expect("process should succeed");
            counts.push(
                ctx.nodes[NODE_NAME]["iteration"]
                    .as_u64()
                    .expect("iteration is a u64"),
            );
        }

        assert_eq!(counts, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn process_ignores_unrelated_stored_results() {
        let node = IncrementCriticIterationNode;
        let mut ctx = empty_ctx();
        put_result(&mut ctx, "SelfCriticNode", json!({ "verdict": "revise" }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(ctx.nodes[NODE_NAME], json!({ "iteration": 1 }));
        assert!(ctx.nodes.contains_key("SelfCriticNode"));
    }

    #[test]
    fn name_is_increment_critic_iteration_node() {
        let node = IncrementCriticIterationNode;
        assert_eq!(node.name(), "IncrementCriticIterationNode");
    }

    #[test]
    fn is_not_a_router_forward_hop_is_a_declared_connection() {
        let node = IncrementCriticIterationNode;
        assert!(node.as_router().is_none());
    }
}
