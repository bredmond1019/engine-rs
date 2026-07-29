//! The declared `WorkflowSchema` / `NodeRegistry` / `Workflow` assembly for
//! `HARVEST_APPROVE` (`EN.7.C` task 6).
//!
//! Declared graph shape (single node, both start and terminal, no router —
//! mirrors `opportunity_edit::graph`'s micro-workflow shape verbatim):
//!
//! ```text
//! HarvestApproveNode      (HARVEST_APPROVE)
//! ```
//!
//! **No policy module, no profiles module, no `harness.json` section.**
//! `HarvestApproveNode` calls no model — it drives the injectable
//! `HttpPost` seam over a pending-harvest record it reads off `ctx.event` —
//! so there is no `ModelTier` to resolve and nothing for a policy layer to
//! override. `crates/engine-serve/src/workflows.rs`'s registration function
//! for this workflow (task 7) accordingly resolves no policy and seeds no
//! policy stamp, matching `register_opportunity_set_stage` /
//! `register_opportunity_add_action`.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::nodes::harvest_approve::HarvestApproveNode;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

/// The identity `HARVEST_APPROVE`'s single node is registered under.
pub const HARVEST_APPROVE_NODE_NAME: &str = "HarvestApproveNode";

/// `HARVEST_APPROVE`'s registered workflow type string.
pub const HARVEST_APPROVE_WORKFLOW_TYPE: &str = "HARVEST_APPROVE";

/// Build the declared `WorkflowSchema` for `HARVEST_APPROVE`: a single node,
/// both start and terminal, with no forward connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        HARVEST_APPROVE_NODE_NAME.to_string(),
        NodeConfig::new(HARVEST_APPROVE_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(
        HARVEST_APPROVE_WORKFLOW_TYPE,
        HARVEST_APPROVE_NODE_NAME,
        nodes,
    )
}

/// Build a fresh `NodeRegistry` for `HARVEST_APPROVE`: one
/// `HarvestApproveNode`, registered under [`HARVEST_APPROVE_NODE_NAME`]
/// (its default `Node::name()` identity — no `with_identity` override
/// needed, unlike `opportunity_edit`'s two distinctly-configured
/// instances).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(HarvestApproveNode::new()));
    registry
}

/// Build the runnable `HARVEST_APPROVE` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("HARVEST_APPROVE declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, HARVEST_APPROVE_WORKFLOW_TYPE);
    }

    #[test]
    fn start_node_is_its_single_identity_with_no_connections() {
        let schema = schema();
        assert_eq!(schema.start_node, HARVEST_APPROVE_NODE_NAME);
        let config = schema
            .nodes
            .get(HARVEST_APPROVE_NODE_NAME)
            .expect("start node should be declared");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn registry_contains_exactly_one_node_under_expected_identity() {
        let registry = registry();
        assert!(registry.contains(HARVEST_APPROVE_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn schema_declares_exactly_one_node_that_is_both_start_and_terminal() {
        let schema = schema();
        assert_eq!(schema.nodes.len(), 1);
        let config = schema
            .nodes
            .get(&schema.start_node)
            .expect("start node should be declared");
        assert!(
            config.connections.is_empty(),
            "the single node should have no forward connections (terminal)"
        );
    }
}
