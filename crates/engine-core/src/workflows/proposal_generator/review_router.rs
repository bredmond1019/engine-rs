//! `ProposalReviewRouterNode` — impls `Node` + `Router`, branching on the
//! review verdict to `PersistToBrainNode` (pass) or `ProposalReviseNode`
//! (revise).
//!
//! Deterministic router: reads `ctx.nodes["ProposalReviewNode"]["verdict"]`
//! and routes to the matching terminal identity. A `Router::route` takes
//! `&TaskContext` and cannot mutate it, so this node does no policy
//! resolution or telemetry work of its own — that lives in
//! `ProposalReviewNode` and `ProposalReviseNode` (mirrors
//! `research_agent::graph::ResearchModeRouterNode`).

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::routing::Router;
use crate::workflows::get_result;

use super::review::NODE_NAME as REVIEW_NODE_NAME;

/// The `Node::name()` identity `ProposalReviewRouterNode` is registered
/// under.
pub const NODE_NAME: &str = "ProposalReviewRouterNode";

/// Deterministic router: reads `ProposalReviewNode`'s stored verdict and
/// routes to `PersistToBrainNode` (pass) or `ProposalReviseNode` (revise).
pub struct ProposalReviewRouterNode;

#[async_trait::async_trait]
impl Node for ProposalReviewRouterNode {
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

impl Router for ProposalReviewRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let review = get_result(ctx, REVIEW_NODE_NAME)?;
        let verdict = review.get("verdict")?.as_str()?;
        Some(
            match verdict {
                "pass" => "PersistToBrainNode",
                "revise" => "ProposalReviseNode",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn ctx_with_verdict(verdict: &str) -> TaskContext {
        let mut nodes = HashMap::new();
        nodes.insert(
            REVIEW_NODE_NAME.to_string(),
            json!({ "verdict": verdict, "notes": "" }),
        );
        TaskContext {
            event: json!({ "company_name": "Loja da Ana" }),
            nodes,
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn route_returns_persist_to_brain_for_pass_verdict() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("pass");
        assert_eq!(router.route(&ctx), Some("PersistToBrainNode".to_string()));
    }

    #[test]
    fn route_returns_revise_for_revise_verdict() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("revise");
        assert_eq!(router.route(&ctx), Some("ProposalReviseNode".to_string()));
    }

    #[test]
    fn route_returns_none_when_review_node_has_not_run() {
        let router = ProposalReviewRouterNode;
        let ctx = TaskContext {
            event: json!({ "company_name": "Loja da Ana" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        assert_eq!(router.route(&ctx), None);
    }

    #[test]
    fn route_returns_none_for_unknown_verdict() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("maybe");
        assert_eq!(router.route(&ctx), None);
    }

    #[test]
    fn as_router_is_some() {
        let router = ProposalReviewRouterNode;
        assert!(router.as_router().is_some());
    }
}
