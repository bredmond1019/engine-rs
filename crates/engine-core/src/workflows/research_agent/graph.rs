//! `ResearchModeRouterNode` + the declared `WorkflowSchema` / `NodeRegistry`
//! / `Workflow` assembly — filled in task 7.
//!
//! Declared graph shape:
//!
//! ```text
//! ResearchModeRouterNode -> { CompanyResearchNode | ProspectingResearchNode }
//! ```
//!
//! `ResearchModeRouterNode` is the start node and a [`Router`]: it reads
//! `event.mode` off the `RESEARCH_AGENT` event and routes to whichever of
//! the two terminal nodes matches — it never mutates `ctx`, so (per the
//! spec's Context Pointers) policy resolution and telemetry cannot live
//! here; each terminal node resolves its own policy and writes its own
//! `research-agent-state.json` record (tasks 5/6). Both `CompanyResearchNode`
//! and `ProspectingResearchNode` are graph exit points — neither declares a
//! forward connection.

use std::collections::HashMap;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::routing::Router;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::company_research::CompanyResearchNode;
use super::policy::ResearchAgentPolicy;
use super::prospecting::ProspectingResearchNode;
use super::schema::{ResearchAgentEventSchema, ResearchMode};

/// The `RESEARCH_AGENT` workflow's declared identity/type name, used both to
/// register the workflow (`engine-serve`) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "RESEARCH_AGENT";

/// Deterministic router: reads `event.mode` and routes to the matching
/// terminal node. A `Router::route` takes `&TaskContext` and cannot mutate
/// it, so this node does no policy resolution or telemetry work of its
/// own — that lives in the two terminal nodes it routes to.
pub struct ResearchModeRouterNode;

#[async_trait::async_trait]
impl Node for ResearchModeRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ResearchModeRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for ResearchModeRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let event: ResearchAgentEventSchema = serde_json::from_value(ctx.event.clone()).ok()?;
        Some(
            match event.mode {
                ResearchMode::Company => "CompanyResearchNode",
                ResearchMode::Prospecting => "ProspectingResearchNode",
            }
            .to_string(),
        )
    }
}

/// Build the declared `WorkflowSchema` for the `RESEARCH_AGENT` workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "ResearchModeRouterNode".to_string(),
        NodeConfig::new(
            "ResearchModeRouterNode",
            vec![
                "CompanyResearchNode".to_string(),
                "ProspectingResearchNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "CompanyResearchNode".to_string(),
        NodeConfig::new("CompanyResearchNode", vec![]),
    );
    nodes.insert(
        "ProspectingResearchNode".to_string(),
        NodeConfig::new("ProspectingResearchNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "ResearchModeRouterNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport) configuration. Tests
/// build their own registry with stubbed transports instead of calling this
/// directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ResearchModeRouterNode));
    registry.register(Box::new(CompanyResearchNode::new()));
    registry.register(Box::new(ProspectingResearchNode::new()));
    registry
}

/// Build a `NodeRegistry` like [`registry`], unchanged, for the given
/// resolved `policy`. Both `research` and `prospect` are cloud-only,
/// `WebSearch`-backed stages (per the spec's Context Pointers and Notes) —
/// a local single-shot endpoint cannot serve `WebSearch`/`WebFetch`, so no
/// stage is ever rewired to the `Local` transport here. This is the
/// per-workflow analog of `sdlc_flow::graph::registry_for_policy`'s
/// no-rewire-`implement` guard, but for `RESEARCH_AGENT` it applies to
/// *both* terminal nodes, not just one.
#[must_use]
pub fn registry_for_policy(_policy: &ResearchAgentPolicy) -> NodeRegistry {
    registry()
}

/// Build the runnable `RESEARCH_AGENT` `Workflow`: [`registry`] paired with
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
        .expect("RESEARCH_AGENT declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;

    use serde_json::json;

    use super::*;
    use crate::validate::WorkflowValidator;

    fn ctx_for_mode(mode: &str) -> TaskContext {
        TaskContext {
            event: json!({ "mode": mode }),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        }
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_research_mode_router() {
        assert_eq!(schema().start_node, "ResearchModeRouterNode");
    }

    #[test]
    fn workflow_type_is_research_agent() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_all_three_nodes() {
        let registry = registry();

        let expected = [
            "ResearchModeRouterNode",
            "CompanyResearchNode",
            "ProspectingResearchNode",
        ];

        for identity in expected {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), expected.len());
    }

    #[test]
    fn route_returns_company_research_for_company_mode() {
        let router = ResearchModeRouterNode;
        let ctx = ctx_for_mode("company");
        assert_eq!(router.route(&ctx), Some("CompanyResearchNode".to_string()));
    }

    #[test]
    fn route_returns_prospecting_research_for_prospecting_mode() {
        let router = ResearchModeRouterNode;
        let ctx = ctx_for_mode("prospecting");
        assert_eq!(
            router.route(&ctx),
            Some("ProspectingResearchNode".to_string())
        );
    }

    #[test]
    fn route_returns_none_for_invalid_mode() {
        let router = ResearchModeRouterNode;
        let ctx = ctx_for_mode("not-a-real-mode");
        assert_eq!(router.route(&ctx), None);
    }

    #[test]
    fn registry_for_policy_never_changes_node_set_vs_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&ResearchAgentPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("ResearchModeRouterNode"));
        assert!(policy_registry.contains("CompanyResearchNode"));
        assert!(policy_registry.contains("ProspectingResearchNode"));
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
