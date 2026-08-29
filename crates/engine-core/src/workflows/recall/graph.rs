//! The declared `WorkflowSchema` / `NodeRegistry` / `Workflow` assembly for
//! `RECALL` (`EN.12.L` task 2).
//!
//! Declared graph shape (single node, both start and terminal, no router —
//! mirrors `harvest_approve::graph`'s / `opportunity_edit::graph`'s
//! micro-workflow shape verbatim):
//!
//! ```text
//! RecallNode      (RECALL)
//! ```
//!
//! **No policy module, no profiles module, no `harness.json` section.**
//! [`RecallNode`] calls no model — it drives the injectable `HttpGet` seam
//! against Synapse's `GET /recall`, reading its query off `ctx.event` (the
//! unbound default `InputBinding`, so no query plumbing is needed for a
//! dispatch step) — so there is no `ModelTier` to resolve. `limit`/`hybrid`
//! are builder args fixed at registration time, not per-run `Policy` knobs:
//! `brain_client.rs`'s own module doc already records why (they are closer
//! to a call site's fixed shape than a cost/latency/quality trade a run
//! would want overridden — CLAUDE.md standing rule 6's "where feasible"
//! carve-out). `crates/engine-serve/src/workflows.rs`'s registration
//! function for this workflow (`register_recall`) accordingly resolves no
//! policy and seeds no policy stamp, matching `register_terminal_probe` /
//! `register_harvest_approve`.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::nodes::brain_client::{BrainConfig, RecallNode, RECALL_NODE_NAME};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

/// `RECALL`'s registered workflow type string — the wire spelling a
/// dispatch `ChainStep`'s `block_id` names, matching the existing
/// screaming-snake registry keys (`CONTENT_PIPELINE`, `RESEARCH_AGENT`).
pub const RECALL_WORKFLOW_TYPE: &str = "RECALL";

/// Build the declared `WorkflowSchema` for `RECALL`: a single node, both
/// start and terminal, with no forward connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        RECALL_NODE_NAME.to_string(),
        NodeConfig::new(RECALL_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(RECALL_WORKFLOW_TYPE, RECALL_NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` for `RECALL`: one `RecallNode` built from
/// `config`, registered under its default `Node::name()` identity
/// ([`RECALL_NODE_NAME`]) with an unbound query source (reads `ctx.event`)
/// — no `with_input_from` override needed, since a `RECALL` dispatch step
/// is always the query's own source.
#[must_use]
pub fn registry(config: BrainConfig) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(RecallNode::new(config)));
    registry
}

/// Build the runnable `RECALL` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow(config: BrainConfig) -> Workflow {
    Workflow::new_validated(registry(config), schema())
        .expect("RECALL declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    fn test_config() -> BrainConfig {
        BrainConfig::new("http://localhost:8000", None)
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry(test_config());
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, RECALL_WORKFLOW_TYPE);
    }

    #[test]
    fn workflow_type_is_the_recall_wire_spelling() {
        assert_eq!(RECALL_WORKFLOW_TYPE, "RECALL");
    }

    #[test]
    fn start_node_is_its_single_identity_with_no_connections() {
        let schema = schema();
        assert_eq!(schema.start_node, RECALL_NODE_NAME);
        let config = schema
            .nodes
            .get(RECALL_NODE_NAME)
            .expect("start node should be declared");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn registry_contains_exactly_one_node_under_expected_identity() {
        let registry = registry(test_config());
        assert!(registry.contains(RECALL_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow(test_config());
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
