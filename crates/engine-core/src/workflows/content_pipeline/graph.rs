//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy`
//! (Local rewire) / `Workflow` assembly for `CONTENT_PIPELINE`.
//! `WORKFLOW_TYPE` lives here (mirrors `proposal_generator::graph` /
//! `research_agent::graph` / `diagnostic_intake::graph` / `sdlc_flow::graph`).
//!
//! Declared graph shape (source of truth:
//! `planning/EN.5.A-content-pipeline/architecture.md` §5):
//!
//! ```text
//! SourceRouterNode -> { FetchArticleNode | FetchTranscriptNode
//!                      | NormalizeChannelContentNode }
//!   -> SummarizeNode -> SelfCriticNode -> CriticRouterNode
//!   -> { TranslateSkipRouterNode -> { TranslateNode | DigestRenderNode }
//!      | IncrementCriticIterationNode -> ReviseNode -> SelfCriticNode }  // back-edge
//! TranslateNode -> DigestRenderNode -> PersistToBrainNode  // terminal in EN.5.A
//! ```
//!
//! `CriticRouterNode` is a [`Router`]: it reads `SelfCriticNode`'s stored
//! `CriticEvaluation` plus the run's resolved loop bounds (stamped by
//! `SourceRouterNode`) and routes to `TranslateSkipRouterNode` (pass,
//! confidence-threshold met, or iteration cap reached) or
//! `IncrementCriticIterationNode` (revise, back-edge) — it never mutates
//! `ctx` (per the spec's Context Pointers). `IncrementCriticIterationNode`
//! bumps the durable iteration counter and forwards to `ReviseNode`, which
//! re-enters `SelfCriticNode` for another pass, capped by
//! `max_critic_iterations` (architecture.md §4). `TranslateSkipRouterNode`
//! is likewise a [`Router`]: translate-on routes through `TranslateNode`
//! first, translate-off skips straight to `DigestRenderNode`.
//! `PersistToBrainNode` is the sole terminal node — no forward connection
//! (EN.6.A wires an `ActionDispatchNode` after it later).
//!
//! Per `routing.rs`'s D42 declared-acyclic / runtime-cyclic contract, the
//! `ReviseNode -> SelfCriticNode` back-edge is never walked by
//! `WorkflowValidator`'s DFS cycle check because it is reached only through
//! `CriticRouterNode`'s runtime `Router::route` decision, not a declared
//! non-router connection — `CriticRouterNode`'s own declared out-edges are
//! skipped by that check entirely.

use std::collections::HashMap;
use std::sync::Arc;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::critic_router::CriticRouterNode;
use super::digest_render::DigestRenderNode;
use super::fetch_article::FetchArticleNode;
use super::fetch_transcript::FetchTranscriptNode;
use super::increment_critic_iteration::IncrementCriticIterationNode;
use super::normalize_channel_content::NormalizeChannelContentNode;
use super::persist_to_brain::PersistToBrainNode;
use super::policy::{ContentPipelinePolicy, ModelTier};
use super::revise::ReviseNode;
use super::self_critic::SelfCriticNode;
use super::source_router::SourceRouterNode;
use super::summarize::SummarizeNode;
use super::translate::{TranslateNode, TranslateSkipRouterNode};

/// The `CONTENT_PIPELINE` workflow's declared identity/type name, used both
/// to register the workflow (`engine-serve`) and as
/// `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "CONTENT_PIPELINE";

/// Build the declared `WorkflowSchema` for the `CONTENT_PIPELINE` workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "SourceRouterNode".to_string(),
        NodeConfig::new(
            "SourceRouterNode",
            vec![
                "FetchArticleNode".to_string(),
                "FetchTranscriptNode".to_string(),
                "NormalizeChannelContentNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "FetchArticleNode".to_string(),
        NodeConfig::new("FetchArticleNode", vec!["SummarizeNode".to_string()]),
    );
    nodes.insert(
        "FetchTranscriptNode".to_string(),
        NodeConfig::new("FetchTranscriptNode", vec!["SummarizeNode".to_string()]),
    );
    nodes.insert(
        "NormalizeChannelContentNode".to_string(),
        NodeConfig::new(
            "NormalizeChannelContentNode",
            vec!["SummarizeNode".to_string()],
        ),
    );
    nodes.insert(
        "SummarizeNode".to_string(),
        NodeConfig::new("SummarizeNode", vec!["SelfCriticNode".to_string()]),
    );
    nodes.insert(
        "SelfCriticNode".to_string(),
        NodeConfig::new("SelfCriticNode", vec!["CriticRouterNode".to_string()]),
    );
    nodes.insert(
        "CriticRouterNode".to_string(),
        NodeConfig::new(
            "CriticRouterNode",
            vec![
                "TranslateSkipRouterNode".to_string(),
                "IncrementCriticIterationNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "IncrementCriticIterationNode".to_string(),
        NodeConfig::new(
            "IncrementCriticIterationNode",
            vec!["ReviseNode".to_string()],
        ),
    );
    nodes.insert(
        // Back-edge: reachable only through `CriticRouterNode`'s runtime
        // `Router::route`, never walked as a declared non-router cycle
        // (D42) — see module doc comment.
        "ReviseNode".to_string(),
        NodeConfig::new("ReviseNode", vec!["SelfCriticNode".to_string()]),
    );
    nodes.insert(
        "TranslateSkipRouterNode".to_string(),
        NodeConfig::new(
            "TranslateSkipRouterNode",
            vec!["TranslateNode".to_string(), "DigestRenderNode".to_string()],
        ),
    );
    nodes.insert(
        "TranslateNode".to_string(),
        NodeConfig::new("TranslateNode", vec!["DigestRenderNode".to_string()]),
    );
    nodes.insert(
        "DigestRenderNode".to_string(),
        NodeConfig::new("DigestRenderNode", vec!["PersistToBrainNode".to_string()]),
    );
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "SourceRouterNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport/real-`HttpPost`/
/// real-fetch) configuration. Tests build their own registry with stubbed
/// transports/seams instead of calling this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SourceRouterNode));
    registry.register(Box::new(FetchArticleNode::new()));
    registry.register(Box::new(FetchTranscriptNode::new()));
    registry.register(Box::new(NormalizeChannelContentNode));
    registry.register(Box::new(SummarizeNode::new()));
    registry.register(Box::new(SelfCriticNode::new()));
    registry.register(Box::new(CriticRouterNode));
    registry.register(Box::new(IncrementCriticIterationNode));
    registry.register(Box::new(ReviseNode::new()));
    registry.register(Box::new(TranslateSkipRouterNode));
    registry.register(Box::new(TranslateNode::new()));
    registry.register(Box::new(DigestRenderNode));
    registry.register(Box::new(PersistToBrainNode::new()));

    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier stage's `openai_compat_transport` routes to when its local
/// endpoint is unavailable. Mirrors `proposal_generator::graph::real_cloud_transport`
/// / `sdlc_flow::graph::real_cloud_transport`.
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with whichever of the four
/// Local-eligible stages — `summarize` (`SummarizeNode`), `critic`
/// (`SelfCriticNode`), `revise` (`ReviseNode`), `translate`
/// (`TranslateNode`) — `policy` resolves to [`ModelTier::Local`] rewired to
/// route through [`openai_compat_transport_live`] (falling back to the real
/// `claude` CLI transport on any local-endpoint failure). **Never**
/// rewires the fetch/normalize/render/persist stages — they carry no
/// `ModelTier` field and are not model nodes at all (architecture.md §5).
/// This is the four-stage analog of `proposal_generator::graph::registry_for_policy`.
#[must_use]
pub fn registry_for_policy(policy: &ContentPipelinePolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.summarize == ModelTier::Local {
        registry.register(Box::new(SummarizeNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.critic == ModelTier::Local {
        registry.register(Box::new(SelfCriticNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.revise == ModelTier::Local {
        registry.register(Box::new(ReviseNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.translate == ModelTier::Local {
        registry.register(Box::new(TranslateNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    registry
}

/// Build the runnable `CONTENT_PIPELINE` `Workflow`: [`registry`] paired
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
        .expect("CONTENT_PIPELINE declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    const ALL_NODE_IDENTITIES: [&str; 13] = [
        "SourceRouterNode",
        "FetchArticleNode",
        "FetchTranscriptNode",
        "NormalizeChannelContentNode",
        "SummarizeNode",
        "SelfCriticNode",
        "CriticRouterNode",
        "IncrementCriticIterationNode",
        "ReviseNode",
        "TranslateSkipRouterNode",
        "TranslateNode",
        "DigestRenderNode",
        "PersistToBrainNode",
    ];

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_source_router() {
        assert_eq!(schema().start_node, "SourceRouterNode");
    }

    #[test]
    fn workflow_type_is_content_pipeline() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_every_declared_node() {
        let registry = registry();
        for identity in ALL_NODE_IDENTITIES {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), ALL_NODE_IDENTITIES.len());
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
    fn revise_node_back_edge_targets_self_critic() {
        let schema = schema();
        let revise_config = &schema.nodes["ReviseNode"];
        assert_eq!(
            revise_config.connections,
            vec!["SelfCriticNode".to_string()]
        );
    }

    #[test]
    fn persist_to_brain_is_terminal() {
        let schema = schema();
        assert!(schema.nodes["PersistToBrainNode"].connections.is_empty());
    }

    #[test]
    fn critic_router_declares_both_branches() {
        let schema = schema();
        let registry = registry();

        let router_config = &schema.nodes["CriticRouterNode"];
        for branch in ["TranslateSkipRouterNode", "IncrementCriticIterationNode"] {
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
    fn translate_skip_router_declares_both_branches() {
        let schema = schema();
        let registry = registry();

        let router_config = &schema.nodes["TranslateSkipRouterNode"];
        for branch in ["TranslateNode", "DigestRenderNode"] {
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
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&ContentPipelinePolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        for identity in ALL_NODE_IDENTITIES {
            assert!(policy_registry.contains(identity));
        }
    }

    #[test]
    fn registry_for_policy_rewires_exactly_the_four_model_stages_when_all_local() {
        let mut policy = ContentPipelinePolicy::default();
        policy.model_tiers.summarize = ModelTier::Local;
        policy.model_tiers.critic = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;
        policy.model_tiers.translate = ModelTier::Local;

        let registry = registry_for_policy(&policy);

        // Rewiring must not change the registry's node count or identity
        // set — only the transport those nodes' composed `ClaudeCodeStep`
        // uses.
        assert_eq!(registry.len(), super::registry().len());
        for identity in ALL_NODE_IDENTITIES {
            assert!(registry.contains(identity));
        }
    }

    #[test]
    fn registry_for_policy_never_rewires_fetch_normalize_render_persist_stages() {
        // `FetchArticleNode`, `FetchTranscriptNode`,
        // `NormalizeChannelContentNode`, `DigestRenderNode`, and
        // `PersistToBrainNode` have no `ModelTier` field at all — there is
        // no branch in `registry_for_policy` that could rewire them, by
        // construction. This test documents that: a policy with every
        // model-eligible tier set to Local still leaves those five
        // identities registered (i.e. present, not silently dropped) with
        // no policy hook available to touch them.
        let mut policy = ContentPipelinePolicy::default();
        policy.model_tiers.summarize = ModelTier::Local;
        policy.model_tiers.critic = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;
        policy.model_tiers.translate = ModelTier::Local;

        let registry = registry_for_policy(&policy);
        for identity in [
            "FetchArticleNode",
            "FetchTranscriptNode",
            "NormalizeChannelContentNode",
            "DigestRenderNode",
            "PersistToBrainNode",
        ] {
            assert!(registry.contains(identity));
        }
        assert_eq!(registry.len(), super::registry().len());
    }
}
