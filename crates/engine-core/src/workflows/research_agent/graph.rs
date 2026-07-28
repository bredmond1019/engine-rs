//! `ResearchModeRouterNode` + the declared `WorkflowSchema` / `NodeRegistry`
//! / `Workflow` assembly — filled in task 7, closed into a terminal write
//! by `EN.7.B` task 4, extended to a terminal contact-merge step by `EN.4.E`
//! task 8.
//!
//! Declared graph shape:
//!
//! ```text
//! ResearchModeRouterNode -> { CompanyResearchNode | ProspectingResearchNode } -> MaterializeDocNode -> MergeContactsNode
//! ```
//!
//! `ResearchModeRouterNode` is the start node and a [`Router`]: it reads
//! `event.mode` off the `RESEARCH_AGENT` event and routes to whichever of
//! the two terminal nodes matches — it never mutates `ctx`, so (per the
//! spec's Context Pointers) policy resolution and telemetry cannot live
//! here; each terminal node resolves its own policy and writes its own
//! `research-agent-state.json` record (tasks 5/6). `CompanyResearchNode`
//! and `ProspectingResearchNode` both converge on a single shared
//! `MaterializeDocNode` instance (`EN.7.B`), configured with an ordered
//! `with_source_nodes` preference (`CompanyResearchNode` then
//! `ProspectingResearchNode`) since exactly one of the two runs per event;
//! its brain root is left unset so it resolves via
//! `crate::brain_root::resolve_brain_root` (`ENGINE_BRAIN_ROOT`, then
//! walk-up) at run time — a served run with no resolvable brain root now
//! fails loudly by design, per the spec's Context Pointers ("Consequence to
//! accept deliberately"). `MaterializeDocNode` now feeds a single shared
//! `MergeContactsNode` instance (`EN.4.E` task 8), which is the graph's only
//! exit point — a run now *ends* by merging whatever `contacts[]` the
//! research node surfaced into the opportunity `MaterializeDocNode` just
//! wrote (or reading, no-op, when the collected list is empty — the normal
//! path for a `contact_enrichment` depth of `off`). Like
//! `MaterializeDocNode`, `MergeContactsNode` is configured with the same
//! ordered `with_source_nodes` preference and an unset brain root (resolves
//! the same way at run time). The graph shape here is INVARIANT across
//! policy depth — no rewire ever branches around `MergeContactsNode`; its
//! own empty-list short-circuit is the only cost control.

use std::collections::HashMap;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::nodes::materialize_doc::MaterializeDocNode;
use crate::nodes::merge_contacts::MergeContactsNode;
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
        NodeConfig::new(
            "CompanyResearchNode",
            vec!["MaterializeDocNode".to_string()],
        ),
    );
    nodes.insert(
        "ProspectingResearchNode".to_string(),
        NodeConfig::new(
            "ProspectingResearchNode",
            vec!["MaterializeDocNode".to_string()],
        ),
    );
    nodes.insert(
        "MaterializeDocNode".to_string(),
        NodeConfig::new("MaterializeDocNode", vec!["MergeContactsNode".to_string()]),
    );
    nodes.insert(
        "MergeContactsNode".to_string(),
        NodeConfig::new("MergeContactsNode", vec![]),
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
    registry.register(Box::new(
        MaterializeDocNode::new("opportunity")
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    registry.register(Box::new(
        MergeContactsNode::new()
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    registry
}

/// Build a `NodeRegistry` like [`registry`], unchanged, for the given
/// resolved `policy`. Both `research` and `prospect` are cloud-only,
/// `WebSearch`-backed stages (per the spec's Context Pointers and Notes) —
/// a local single-shot endpoint cannot serve `WebSearch`/`WebFetch`, so no
/// stage is ever rewired to the `Local` transport here. This is the
/// per-workflow analog of `sdlc_flow::graph::registry_for_policy`'s
/// no-rewire-`implement` guard, but for `RESEARCH_AGENT` it applies to
/// *both* terminal research nodes, not just one — `MaterializeDocNode` is
/// deterministic and carries no `ModelTier` at all, so it is never a
/// rewrite candidate either way.
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
    fn registry_contains_all_five_nodes() {
        let registry = registry();

        let expected = [
            "ResearchModeRouterNode",
            "CompanyResearchNode",
            "ProspectingResearchNode",
            "MaterializeDocNode",
            "MergeContactsNode",
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
    fn merge_contacts_node_is_the_only_identity_declaring_no_connections() {
        let schema = schema();

        let terminal_identities: Vec<&String> = schema
            .nodes
            .iter()
            .filter(|(_, config)| config.connections.is_empty())
            .map(|(identity, _)| identity)
            .collect();

        assert_eq!(
            terminal_identities,
            vec!["MergeContactsNode"],
            "MergeContactsNode should be the only node declaring no forward connection"
        );
    }

    #[test]
    fn materialize_doc_node_connects_only_to_merge_contacts_node() {
        let schema = schema();

        let config = schema
            .nodes
            .get("MaterializeDocNode")
            .expect("schema should declare 'MaterializeDocNode'");
        assert_eq!(
            config.connections,
            vec!["MergeContactsNode".to_string()],
            "MaterializeDocNode should connect only to MergeContactsNode"
        );
    }

    #[test]
    fn both_research_branches_connect_to_materialize_doc_node() {
        let schema = schema();

        for identity in ["CompanyResearchNode", "ProspectingResearchNode"] {
            let config = schema
                .nodes
                .get(identity)
                .unwrap_or_else(|| panic!("schema should declare '{identity}'"));
            assert_eq!(
                config.connections,
                vec!["MaterializeDocNode".to_string()],
                "'{identity}' should connect only to MaterializeDocNode"
            );
        }
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
        assert!(policy_registry.contains("MaterializeDocNode"));
        assert!(policy_registry.contains("MergeContactsNode"));
    }

    #[test]
    fn graph_shape_is_identical_under_a_cheap_fast_off_depth_profile() {
        use super::super::policy::{ContactDepth, ContactEnrichment};

        let off_policy = ResearchAgentPolicy {
            contact_enrichment: ContactEnrichment {
                research: ContactDepth::Off,
                prospect: ContactDepth::Off,
                max_fetches: 0,
            },
            ..ResearchAgentPolicy::default()
        };

        let default_registry = registry();
        let off_registry = registry_for_policy(&off_policy);

        assert_eq!(off_registry.len(), default_registry.len());
        for identity in [
            "ResearchModeRouterNode",
            "CompanyResearchNode",
            "ProspectingResearchNode",
            "MaterializeDocNode",
            "MergeContactsNode",
        ] {
            assert!(
                off_registry.contains(identity),
                "expected off-depth registry to still contain '{identity}'"
            );
        }

        // The declared schema itself carries no policy dependency at all —
        // reassert it still validates against the off-depth registry.
        WorkflowValidator::validate(&off_registry, &schema())
            .expect("declared graph should validate under the off-depth registry too");
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
