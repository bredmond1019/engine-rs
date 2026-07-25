//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire for `IntakeExtractNode`) / `Workflow` assembly for
//! `DIAGNOSTIC_INTAKE`.
//!
//! Declared graph shape:
//!
//! ```text
//! IntakeExtractNode
//! ```
//!
//! `IntakeExtractNode` is both the start node and the sole (terminal) node —
//! there is no router, unlike `research_agent`'s two-branch shape.

use std::collections::HashMap;
use std::sync::Arc;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::extract::IntakeExtractNode;
use super::policy::{DiagnosticIntakePolicy, ModelTier};

/// The registered workflow type string (mirrors `research_agent::graph` /
/// `sdlc_flow::graph`, both of which hold `WORKFLOW_TYPE` here rather than
/// in `mod.rs`).
pub const WORKFLOW_TYPE: &str = "DIAGNOSTIC_INTAKE";

/// Build the declared `WorkflowSchema` for the `DIAGNOSTIC_INTAKE`
/// workflow: a single node, both start and terminal, with no forward
/// connection.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "IntakeExtractNode".to_string(),
        NodeConfig::new("IntakeExtractNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "IntakeExtractNode", nodes)
}

/// Build a fresh `NodeRegistry` with the single node identity in [`schema`]
/// registered, with its default (real-transport) configuration. Tests build
/// their own registry with a stubbed transport instead of calling this
/// directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(IntakeExtractNode::new()));
    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier `extract` stage's `openai_compat_transport` routes to when
/// its local endpoint is unavailable. Mirrors
/// `sdlc_flow::graph::real_cloud_transport`.
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with `IntakeExtractNode`
/// rewired to route through [`openai_compat_transport_live`] whenever
/// `policy`'s resolved `extract` tier is [`ModelTier::Local`] — the direct
/// analog of `sdlc_flow::graph::registry_for_policy`'s triage/review rewire,
/// but the inverse of `research_agent::graph::registry_for_policy`'s
/// no-rewire guard: here the sole stage *is* Local-eligible (pure
/// extraction suits a local coder model), so the rewire fires for the one
/// and only node in this workflow rather than being permanently absent.
///
/// Any local-endpoint failure at call time falls back to the real `claude`
/// CLI transport for that call — `openai_compat_transport`'s own fail-fast
/// + fallback, not something this function decides.
#[must_use]
pub fn registry_for_policy(policy: &DiagnosticIntakePolicy) -> NodeRegistry {
    let mut registry = NodeRegistry::new();

    if policy.model_tiers.extract == ModelTier::Local {
        registry.register(Box::new(IntakeExtractNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    } else {
        registry.register(Box::new(IntakeExtractNode::new()));
    }

    registry
}

/// Build the runnable `DIAGNOSTIC_INTAKE` `Workflow`: [`registry`] paired
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
        .expect("DIAGNOSTIC_INTAKE declared graph must pass WorkflowValidator::validate")
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
    fn start_node_is_intake_extract_node() {
        assert_eq!(schema().start_node, "IntakeExtractNode");
    }

    #[test]
    fn workflow_type_is_diagnostic_intake() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_the_single_node() {
        let registry = registry();
        assert!(registry.contains("IntakeExtractNode"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&DiagnosticIntakePolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("IntakeExtractNode"));
    }

    #[test]
    fn registry_for_policy_with_local_tier_keeps_same_node_identity() {
        let policy = DiagnosticIntakePolicy {
            model_tiers: super::super::policy::ModelTiers {
                extract: ModelTier::Local,
            },
            ..DiagnosticIntakePolicy::default()
        };

        let registry = registry_for_policy(&policy);

        // Rewiring IntakeExtractNode's transport must not change the
        // registry's node count or identity set — only the transport its
        // composed `ClaudeCodeStep` uses.
        assert_eq!(registry.len(), 1);
        assert!(registry.contains("IntakeExtractNode"));
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
}
