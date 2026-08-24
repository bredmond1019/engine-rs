//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy` /
//! `Workflow` assembly for `DELIVERABLE_RENDER` (`EN.4.D` task 5) —
//! mirrors `proposal_generator/graph.rs`'s shape, scaled down to this
//! workflow's two-node straight line.
//!
//! Declared graph shape:
//!
//! ```text
//! RenderDeliverableNode -> RenderPdfNode
//! ```
//!
//! `RenderPdfNode` is the sole terminal node (no forward connection).
//! Neither node currently carries a policy-selected transport seam — the
//! one real knob on this workflow's `Policy` surface, the optional
//! model-polish pass over the rendered markdown (`policy.rs`), has no
//! wiring into `RenderDeliverableNode` yet, so [`registry_for_policy`]
//! builds the same registry [`registry`] does regardless of the resolved
//! policy. That keeps the declared node set identical across all three
//! named profiles (`CLAUDE.md` standing rule 6): when the polish pass is
//! wired, it must land as an in-place no-op inside `RenderDeliverableNode`,
//! never as a conditional rewire of this graph.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::policy::DeliverableRenderPolicy;
use super::render_markdown::{RenderDeliverableNode, NODE_NAME as RENDER_DELIVERABLE_NODE_NAME};
use super::render_pdf::{RenderPdfNode, NODE_NAME as RENDER_PDF_NODE_NAME};

/// The registered workflow type string (mirrors `proposal_generator::graph`
/// / `diagnostic_intake::graph`, both of which hold `WORKFLOW_TYPE` here
/// rather than in `mod.rs`).
pub const WORKFLOW_TYPE: &str = "DELIVERABLE_RENDER";

/// Build the declared `WorkflowSchema` for `DELIVERABLE_RENDER`: the
/// straight line `RenderDeliverableNode -> RenderPdfNode`, with
/// `RenderPdfNode` terminal.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        RENDER_DELIVERABLE_NODE_NAME.to_string(),
        NodeConfig::new(
            RENDER_DELIVERABLE_NODE_NAME,
            vec![RENDER_PDF_NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        RENDER_PDF_NODE_NAME.to_string(),
        NodeConfig::new(RENDER_PDF_NODE_NAME, vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, RENDER_DELIVERABLE_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` with both declared node identities
/// registered under their default (real `CommandRunner`) configuration.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(RenderDeliverableNode::new()));
    registry.register(Box::new(RenderPdfNode::new()));
    registry
}

/// Build a `NodeRegistry` like [`registry`], but explicitly policy-aware —
/// the analog of `diagnostic_intake::graph::registry_for_policy` /
/// `research_agent::graph::registry_for_policy`'s no-rewire guard. Neither
/// `RenderDeliverableNode` nor `RenderPdfNode` currently exposes a
/// policy-selected seam (the polish pass is not yet wired into
/// `RenderDeliverableNode`), so every field of `policy` is presently unused
/// here; the parameter exists so callers resolve and thread policy exactly
/// the way every other workflow's `engine-serve` registration does, and so
/// wiring the polish pass later is a change inside this function's body,
/// not a change to its signature or callers.
#[must_use]
pub fn registry_for_policy(_policy: &DeliverableRenderPolicy) -> NodeRegistry {
    registry()
}

/// Build the runnable `DELIVERABLE_RENDER` `Workflow`: [`registry`] paired
/// with [`schema`], constructed via `Workflow::new_validated` so assembly
/// fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("DELIVERABLE_RENDER declared graph must pass WorkflowValidator::validate")
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
    fn workflow_type_is_deliverable_render() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn start_node_is_render_deliverable_node() {
        assert_eq!(schema().start_node, RENDER_DELIVERABLE_NODE_NAME);
    }

    #[test]
    fn render_deliverable_node_connects_to_render_pdf_node() {
        let schema = schema();
        let config = schema
            .nodes
            .get(RENDER_DELIVERABLE_NODE_NAME)
            .expect("start node should be declared");
        assert_eq!(config.connections, vec![RENDER_PDF_NODE_NAME.to_string()]);
    }

    #[test]
    fn render_pdf_node_is_terminal() {
        let schema = schema();
        let config = schema
            .nodes
            .get(RENDER_PDF_NODE_NAME)
            .expect("terminal node should be declared");
        assert!(
            config.connections.is_empty(),
            "RenderPdfNode should have no forward connections"
        );
    }

    #[test]
    fn registry_contains_both_nodes() {
        let registry = registry();
        assert!(registry.contains(RENDER_DELIVERABLE_NODE_NAME));
        assert!(registry.contains(RENDER_PDF_NODE_NAME));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn declared_graph_has_no_dangling_or_unregistered_identity() {
        let schema = schema();
        let registry = registry();

        for (identity, config) in &schema.nodes {
            assert!(
                registry.contains(identity),
                "declared node '{identity}' is not registered"
            );
            for connection in &config.connections {
                assert!(
                    schema.nodes.contains_key(connection),
                    "'{identity}' declares a connection to unregistered/undeclared '{connection}'"
                );
            }
        }
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&DeliverableRenderPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains(RENDER_DELIVERABLE_NODE_NAME));
        assert!(policy_registry.contains(RENDER_PDF_NODE_NAME));
    }

    #[test]
    fn registry_for_policy_with_polish_enabled_keeps_the_same_node_set() {
        let policy = DeliverableRenderPolicy {
            polish_enabled: true,
            ..DeliverableRenderPolicy::default()
        };

        let registry = registry_for_policy(&policy);

        // Toggling the one real knob must not change the registered node
        // identity set — the declared shape stays invariant across policy
        // (CLAUDE.md standing rule 6), even though the knob is not yet
        // wired into RenderDeliverableNode's behavior.
        assert_eq!(registry.len(), 2);
        assert!(registry.contains(RENDER_DELIVERABLE_NODE_NAME));
        assert!(registry.contains(RENDER_PDF_NODE_NAME));
    }
}
