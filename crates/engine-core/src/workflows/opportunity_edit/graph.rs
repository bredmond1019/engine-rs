//! The declared `WorkflowSchema` / `NodeRegistry` / `Workflow` assembly for
//! both opportunity-edit micro-workflows.
//!
//! Declared graph shape (identical for both — a single node, both start and
//! terminal, no router, mirroring `diagnostic_intake::graph`):
//!
//! ```text
//! SetOpportunityStageNode      (OPPORTUNITY_SET_STAGE)
//! AddOpportunityActionNode     (OPPORTUNITY_ADD_ACTION)
//! ```
//!
//! Each identity wraps a distinctly-configured `OpportunityEditNode`
//! (`crate::nodes::opportunity_edit`) via `NodeExt::with_identity`, so the
//! two never collide in a shared `NodeRegistry`/dispatcher and the declared
//! start node reads self-describingly.
//!
//! **No policy module, no profiles module, no `harness.json` section.**
//! `OpportunityEditNode` calls no model — it drives the injectable
//! `DocMaterializer` seam over mev's deterministic `plan_set_stage` /
//! `plan_add_action` — so there is no `ModelTier` to resolve and nothing
//! for a policy layer to override. `crates/engine-serve/src/workflows.rs`'s
//! registration functions for these two types accordingly resolve no
//! policy and seed no policy stamp (`EN.7.B` task 6) — the first
//! `register_builtin_workflows` entries with that shape.

use std::collections::HashMap;

use crate::node::{NodeExt, NodeRegistry};
use crate::nodes::opportunity_edit::{OpportunityEditNode, OpportunityEditOp};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

/// The identity `OPPORTUNITY_SET_STAGE`'s single node is registered under.
pub const SET_STAGE_NODE_NAME: &str = "SetOpportunityStageNode";
/// The identity `OPPORTUNITY_ADD_ACTION`'s single node is registered under.
pub const ADD_ACTION_NODE_NAME: &str = "AddOpportunityActionNode";

/// `OPPORTUNITY_SET_STAGE`'s registered workflow type string.
pub const SET_STAGE_WORKFLOW_TYPE: &str = "OPPORTUNITY_SET_STAGE";
/// `OPPORTUNITY_ADD_ACTION`'s registered workflow type string.
pub const ADD_ACTION_WORKFLOW_TYPE: &str = "OPPORTUNITY_ADD_ACTION";

/// Build the declared `WorkflowSchema` for `OPPORTUNITY_SET_STAGE`: a
/// single node, both start and terminal, with no forward connection.
#[must_use]
pub fn set_stage_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        SET_STAGE_NODE_NAME.to_string(),
        NodeConfig::new(SET_STAGE_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(SET_STAGE_WORKFLOW_TYPE, SET_STAGE_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `OPPORTUNITY_SET_STAGE`: one
/// `OpportunityEditNode` configured for `OpportunityEditOp::SetStage`,
/// registered under [`SET_STAGE_NODE_NAME`].
#[must_use]
pub fn set_stage_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        OpportunityEditNode::new(OpportunityEditOp::SetStage).with_identity(SET_STAGE_NODE_NAME),
    ));
    registry
}

/// Build the runnable `OPPORTUNITY_SET_STAGE` `Workflow`: [`set_stage_registry`]
/// paired with [`set_stage_schema`], constructed via `Workflow::new_validated`
/// so assembly fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn set_stage_workflow() -> Workflow {
    Workflow::new_validated(set_stage_registry(), set_stage_schema())
        .expect("OPPORTUNITY_SET_STAGE declared graph must pass WorkflowValidator::validate")
}

/// Build the declared `WorkflowSchema` for `OPPORTUNITY_ADD_ACTION`: a
/// single node, both start and terminal, with no forward connection.
#[must_use]
pub fn add_action_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        ADD_ACTION_NODE_NAME.to_string(),
        NodeConfig::new(ADD_ACTION_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(ADD_ACTION_WORKFLOW_TYPE, ADD_ACTION_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `OPPORTUNITY_ADD_ACTION`: one
/// `OpportunityEditNode` configured for `OpportunityEditOp::AddAction`,
/// registered under [`ADD_ACTION_NODE_NAME`].
#[must_use]
pub fn add_action_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        OpportunityEditNode::new(OpportunityEditOp::AddAction).with_identity(ADD_ACTION_NODE_NAME),
    ));
    registry
}

/// Build the runnable `OPPORTUNITY_ADD_ACTION` `Workflow`: [`add_action_registry`]
/// paired with [`add_action_schema`], constructed via `Workflow::new_validated`
/// so assembly fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn add_action_workflow() -> Workflow {
    Workflow::new_validated(add_action_registry(), add_action_schema())
        .expect("OPPORTUNITY_ADD_ACTION declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    #[test]
    fn set_stage_schema_passes_validation() {
        let schema = set_stage_schema();
        let registry = set_stage_registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn add_action_schema_passes_validation() {
        let schema = add_action_schema();
        let registry = add_action_registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn set_stage_workflow_type_matches_schema() {
        assert_eq!(set_stage_schema().workflow_type, SET_STAGE_WORKFLOW_TYPE);
    }

    #[test]
    fn add_action_workflow_type_matches_schema() {
        assert_eq!(add_action_schema().workflow_type, ADD_ACTION_WORKFLOW_TYPE);
    }

    #[test]
    fn set_stage_start_node_is_its_single_identity_with_no_connections() {
        let schema = set_stage_schema();
        assert_eq!(schema.start_node, SET_STAGE_NODE_NAME);
        let config = schema
            .nodes
            .get(SET_STAGE_NODE_NAME)
            .expect("start node should be declared");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn add_action_start_node_is_its_single_identity_with_no_connections() {
        let schema = add_action_schema();
        assert_eq!(schema.start_node, ADD_ACTION_NODE_NAME);
        let config = schema
            .nodes
            .get(ADD_ACTION_NODE_NAME)
            .expect("start node should be declared");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn set_stage_registry_contains_exactly_one_node_under_expected_identity() {
        let registry = set_stage_registry();
        assert!(registry.contains(SET_STAGE_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn add_action_registry_contains_exactly_one_node_under_expected_identity() {
        let registry = add_action_registry();
        assert!(registry.contains(ADD_ACTION_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn set_stage_workflow_builds_without_panicking() {
        let _workflow = set_stage_workflow();
    }

    #[test]
    fn add_action_workflow_builds_without_panicking() {
        let _workflow = add_action_workflow();
    }

    #[test]
    fn the_two_node_identities_are_distinct() {
        assert_ne!(SET_STAGE_NODE_NAME, ADD_ACTION_NODE_NAME);
    }
}
