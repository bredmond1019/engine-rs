//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire) / `Workflow` assembly for `PROPOSAL_GENERATOR`.
//! `WORKFLOW_TYPE` lives here (mirrors `research_agent::graph` /
//! `diagnostic_intake::graph` / `sdlc_flow::graph`).
//!
//! Declared graph shape:
//!
//! ```text
//! ProposalCompanyResearchNode -> OpportunityIdentifierNode -> ProposalWriterNode
//!   -> ProposalReviewNode -> ProposalReviewRouterNode
//!   -> { PersistToBrainNode | ProposalReviseNode -> PersistToBrainNode }
//! ```
//!
//! `ProposalReviewRouterNode` is a [`Router`]: it reads `ProposalReviewNode`'s
//! stored verdict off `ctx.nodes` and routes to whichever of
//! `PersistToBrainNode` (pass) / `ProposalReviseNode` (revise) matches — it
//! never mutates `ctx`, so (per the spec's Context Pointers) policy
//! resolution/telemetry live in the model nodes it routes between, not in
//! the router itself. `PersistToBrainNode` is the sole terminal node — no
//! forward connection.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::company_research::ProposalCompanyResearchNode;
use super::opportunity_identifier::OpportunityIdentifierNode;
use super::persist_to_brain::PersistToBrainNode;
use super::policy::{ModelTier, ProposalGeneratorPolicy};
use super::review::ProposalReviewNode;
use super::review_router::ProposalReviewRouterNode;
use super::revise::ProposalReviseNode;
use super::writer::ProposalWriterNode;

/// The `PROPOSAL_GENERATOR` workflow's declared identity/type name, used
/// both to register the workflow (`engine-serve`) and as
/// `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "PROPOSAL_GENERATOR";

/// Build the declared `WorkflowSchema` for the `PROPOSAL_GENERATOR`
/// workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "ProposalCompanyResearchNode".to_string(),
        NodeConfig::new(
            "ProposalCompanyResearchNode",
            vec!["OpportunityIdentifierNode".to_string()],
        ),
    );
    nodes.insert(
        "OpportunityIdentifierNode".to_string(),
        NodeConfig::new(
            "OpportunityIdentifierNode",
            vec!["ProposalWriterNode".to_string()],
        ),
    );
    nodes.insert(
        "ProposalWriterNode".to_string(),
        NodeConfig::new("ProposalWriterNode", vec!["ProposalReviewNode".to_string()]),
    );
    nodes.insert(
        "ProposalReviewNode".to_string(),
        NodeConfig::new(
            "ProposalReviewNode",
            vec!["ProposalReviewRouterNode".to_string()],
        ),
    );
    nodes.insert(
        "ProposalReviewRouterNode".to_string(),
        NodeConfig::new(
            "ProposalReviewRouterNode",
            vec![
                "PersistToBrainNode".to_string(),
                "ProposalReviseNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "ProposalReviseNode".to_string(),
        NodeConfig::new("ProposalReviseNode", vec!["PersistToBrainNode".to_string()]),
    );
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "ProposalCompanyResearchNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport/real-`HttpPost`)
/// configuration. Tests build their own registry with stubbed transports
/// instead of calling this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ProposalCompanyResearchNode::new()));
    registry.register(Box::new(OpportunityIdentifierNode::new()));
    registry.register(Box::new(ProposalWriterNode::new()));
    registry.register(Box::new(ProposalReviewNode::new()));
    registry.register(Box::new(ProposalReviewRouterNode));
    registry.register(Box::new(ProposalReviseNode::new()));
    registry.register(Box::new(PersistToBrainNode::new()));
    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier stage's `openai_compat_transport` routes to when its local
/// endpoint is unavailable. Mirrors `sdlc_flow::graph::real_cloud_transport`.
fn real_cloud_transport() -> ModelTransport {
    std::sync::Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with whichever of the three
/// Local-eligible stages — `opportunity` (`OpportunityIdentifierNode`),
/// `review` (`ProposalReviewNode`), `revise` (`ProposalReviseNode`) —
/// `policy` resolves to [`ModelTier::Local`] rewired to route through
/// [`openai_compat_transport_live`] (falling back to the real `claude` CLI
/// transport on any local-endpoint failure). **Never** rewires `research`
/// (`ProposalCompanyResearchNode`) — it wraps `ClaudeCodeStep` with
/// WebSearch/WebFetch tools granted, which a local single-shot endpoint
/// cannot serve (spec Context Pointers). `writer` also never rewires — it
/// is cloud-default per the policy module's docs and carries no `Local`
/// dispatch here. This is the multi-stage analog of
/// `sdlc_flow::graph::registry_for_policy`.
#[must_use]
pub fn registry_for_policy(policy: &ProposalGeneratorPolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.opportunity == ModelTier::Local {
        registry.register(Box::new(OpportunityIdentifierNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.review == ModelTier::Local {
        registry.register(Box::new(ProposalReviewNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.revise == ModelTier::Local {
        registry.register(Box::new(ProposalReviseNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    registry
}

/// Build the runnable `PROPOSAL_GENERATOR` `Workflow`: [`registry`] paired
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
        .expect("PROPOSAL_GENERATOR declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;

    use engine_contract::TaskContext;
    use serde_json::json;

    use super::*;
    use crate::routing::Router;
    use crate::validate::WorkflowValidator;
    use crate::workflows::put_result;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_company_research() {
        assert_eq!(schema().start_node, "ProposalCompanyResearchNode");
    }

    #[test]
    fn workflow_type_is_proposal_generator() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_all_seven_nodes() {
        let registry = registry();

        let expected = [
            "ProposalCompanyResearchNode",
            "OpportunityIdentifierNode",
            "ProposalWriterNode",
            "ProposalReviewNode",
            "ProposalReviewRouterNode",
            "ProposalReviseNode",
            "PersistToBrainNode",
        ];

        for identity in expected {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), expected.len());
    }

    fn ctx_with_verdict(verdict: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        put_result(
            &mut ctx,
            "ProposalReviewNode",
            json!({ "verdict": verdict }),
        );
        ctx
    }

    #[test]
    fn router_pass_branch_routes_to_persist_to_brain() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("pass");
        assert_eq!(router.route(&ctx), Some("PersistToBrainNode".to_string()));
    }

    #[test]
    fn router_revise_branch_routes_to_revise_node() {
        let router = ProposalReviewRouterNode;
        let ctx = ctx_with_verdict("revise");
        assert_eq!(router.route(&ctx), Some("ProposalReviseNode".to_string()));
    }

    #[test]
    fn both_router_branches_are_declared_and_registered() {
        let schema = schema();
        let registry = registry();

        let router_config = &schema.nodes["ProposalReviewRouterNode"];
        for branch in ["PersistToBrainNode", "ProposalReviseNode"] {
            assert!(
                router_config.connections.contains(&branch.to_string()),
                "router should declare a connection to '{branch}'"
            );
            assert!(
                registry.contains(branch),
                "branch target '{branch}' should be registered"
            );
        }
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&ProposalGeneratorPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("OpportunityIdentifierNode"));
        assert!(policy_registry.contains("ProposalReviewNode"));
        assert!(policy_registry.contains("ProposalReviseNode"));
    }

    #[test]
    fn registry_for_policy_with_local_tiers_rewires_judgment_stages_but_never_research() {
        let mut policy = ProposalGeneratorPolicy::default();
        policy.model_tiers.opportunity = ModelTier::Local;
        policy.model_tiers.review = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;
        // research is deliberately left at its default (Sonnet) tier — the
        // policy layer never resolves it to Local (spec Context Pointers),
        // and even if it were set to Local here, registry_for_policy has no
        // branch that would rewire ProposalCompanyResearchNode.

        let registry = registry_for_policy(&policy);

        // Rewiring must not change the registry's node count or identity
        // set — only the transport those nodes' composed `ClaudeCodeStep`
        // uses.
        assert_eq!(registry.len(), super::registry().len());
        assert!(registry.contains("OpportunityIdentifierNode"));
        assert!(registry.contains("ProposalReviewNode"));
        assert!(registry.contains("ProposalReviseNode"));
        assert!(registry.contains("ProposalCompanyResearchNode"));
    }

    #[test]
    fn registry_for_policy_never_rewires_research_even_when_every_tier_is_local() {
        // ProposalCompanyResearchNode has no `model_tiers.research == Local`
        // branch in `registry_for_policy` at all — WebSearch/WebFetch tools
        // cannot be served by a local single-shot endpoint (spec Context
        // Pointers). This test documents that by construction: a policy
        // with every tier set to Local, including research, still leaves
        // registry_for_policy with only the three judgment-stage branches
        // to rewire.
        let mut policy = ProposalGeneratorPolicy::default();
        policy.model_tiers.research = ModelTier::Local;
        policy.model_tiers.opportunity = ModelTier::Local;
        policy.model_tiers.review = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;

        let registry = registry_for_policy(&policy);
        assert!(registry.contains("ProposalCompanyResearchNode"));
        assert_eq!(registry.len(), super::registry().len());
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
