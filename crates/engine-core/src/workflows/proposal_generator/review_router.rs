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
//!
//! ## `EN.5.E` task 4 — bound, not imported
//!
//! `ProposalReviewRouterNode` must stay a zero-field unit struct: the
//! hermetic `proposal_generator_e2e.rs` constructs it as the bare value
//! `ProposalReviewRouterNode` (no `::new()`/braces), and that file is not
//! edited by this block. So its upstream identity and its two downstream
//! target identities cannot live on `self` as `InputBinding` fields the
//! way `ProposalReviseNode`'s do (`EN.5.E` task 1's per-struct-field
//! convention). Instead they are held as [`InputBinding`] values built by
//! small module-private functions (`review_input`/`persist_target`/
//! `revise_target`) — not a cross-module `NODE_NAME` import, and not a
//! `pub const` another module could couple to — and `route` reads them
//! through [`InputBinding::resolve`] rather than writing the identity
//! strings inline. The verdict -> branch semantics are unchanged: `pass`
//! routes to the persist target, `revise` routes to the revise target,
//! anything else routes to `None`.

use engine_contract::TaskContext;

use crate::node::{InputBinding, Node, NodeError};
use crate::routing::Router;
use crate::workflows::get_result;

/// The `Node::name()` identity `ProposalReviewRouterNode` is registered
/// under.
pub const NODE_NAME: &str = "ProposalReviewRouterNode";

/// The upstream identity this router reads its verdict from — bound via
/// [`InputBinding`] rather than an imported `super::review::NODE_NAME`
/// const (see module docs).
fn review_input() -> InputBinding {
    InputBinding::bound("ProposalReviewNode")
}

/// The downstream identity a `pass` verdict routes to — bound via
/// [`InputBinding`] rather than a literal written inline in `route`.
fn persist_target() -> InputBinding {
    InputBinding::bound("PersistToBrainNode")
}

/// The downstream identity a `revise` verdict routes to — bound via
/// [`InputBinding`] rather than a literal written inline in `route`.
fn revise_target() -> InputBinding {
    InputBinding::bound("ProposalReviseNode")
}

/// Resolve a bound identity. The fallback is never observed here — every
/// binding above is constructed via [`InputBinding::bound`] — it exists
/// only to satisfy [`InputBinding::resolve`]'s signature.
fn resolve_bound(binding: &InputBinding) -> &str {
    binding.resolve("")
}

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
        let review_binding = review_input();
        let review = get_result(ctx, resolve_bound(&review_binding))?;
        let verdict = review.get("verdict")?.as_str()?;
        let target = match verdict {
            "pass" => persist_target(),
            "revise" => revise_target(),
            _ => return None,
        };
        Some(resolve_bound(&target).to_string())
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
            "ProposalReviewNode".to_string(),
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

    // -- EN.5.E task 4: bound, not hardcoded, targets ---------------------

    #[test]
    fn review_input_and_targets_are_bound_input_bindings() {
        assert!(review_input().is_bound());
        assert!(persist_target().is_bound());
        assert!(revise_target().is_bound());
    }

    #[test]
    fn resolve_bound_reads_through_the_binding_not_a_literal_default() {
        // The fallback passed to `resolve` is never observed for a bound
        // binding — proving the identity comes from the binding itself.
        assert_eq!(
            InputBinding::bound("PersistToBrainNode").resolve("nonsense-fallback"),
            "PersistToBrainNode"
        );
    }
}
