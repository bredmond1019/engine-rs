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
//! TranslateNode -> DigestRenderNode -> LearningArtifactPayloadNode
//!   -> MaterializeDocNode -> PersistToBrainNode
//!   -> ActionDispatchNode  // terminal (EN.6.A egress)
//! ```
//!
//! `LearningArtifactPayloadNode` + `MaterializeDocNode` are `EN.7.D`: the
//! finished digest is materialized into the Brain corpus as a source `.md`
//! (D53's fourth boundary-test channel — mev writes the document, Synapse
//! still owns the derived index) before `PersistToBrainNode`'s Synapse
//! push. That ORDER is deliberate: `PersistToBrainNode` halts the run on a
//! non-2xx, so materializing first means an unreachable Synapse can no
//! longer cost the run its written document. `MaterializeDocNode` is the
//! same generic node `RESEARCH_AGENT` uses for opportunities — only the
//! model string (`"learning-artifact"`) and its source-node differ, which
//! is precisely the generality claim this block exists to prove.
//!
//! `PersistToBrainNode` is `EN.7.C`'s harvest gate execution site: what
//! varies under its resolved `harvest` knob is whether the push happens now
//! (`in_process`), never (`off` — the default; the freshness reindex
//! indexes the `.md` `MaterializeDocNode` just wrote), or is deferred as a
//! pending record for later `HARVEST_APPROVE` approval (`approval`). The
//! graph shape above is unchanged in every case — only the node's own
//! configuration varies. The materialize-before-push ORDER matters even
//! more under `off`: the write is the durable artifact and the index
//! follows it, whether via the freshness reindex or a later approval.
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
//! `PersistToBrainNode` forwards to `ActionDispatchNode` (`EN.6.A`), the
//! sole terminal node — the outbound egress seam that replies to the
//! originating channel (or dispatches nothing for fire-and-forget runs)
//! and/or chains a follow-on workflow trigger.
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
use crate::nodes::harvest_gate::HarvestGate;
use crate::nodes::materialize_doc::MaterializeDocNode;
use crate::nodes::openai_compat_transport::openai_compat_meta_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::ModelTransport;

use super::action_dispatch::ActionDispatchNode;
use super::critic_router::CriticRouterNode;
use super::digest_render::DigestRenderNode;
use super::fetch_article::FetchArticleNode;
use super::fetch_transcript::FetchTranscriptNode;
use super::increment_critic_iteration::IncrementCriticIterationNode;
use super::learning_artifact::{self, LearningArtifactPayloadNode};
use super::normalize_channel_content::NormalizeChannelContentNode;
use super::persist_to_brain::PersistToBrainNode;
use super::policy::{ContentPipelinePolicy, HarvestConfig, MaterializeConfig, ModelTier};
use super::revise::ReviseNode;
use super::self_critic::SelfCriticNode;
use super::source_router::SourceRouterNode;
use super::summarize::SummarizeNode;
use super::translate::{TranslateNode, TranslateSkipRouterNode};

/// The `CONTENT_PIPELINE` workflow's declared identity/type name, used both
/// to register the workflow (`engine-serve`) and as
/// `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "CONTENT_PIPELINE";

/// The `BrainDocModel` this workflow's `MaterializeDocNode` instance writes
/// (`EN.7.D`). Not a policy knob: the doc kind a pipeline emits is fixed by
/// what the pipeline produces, not a per-run cost/quality trade.
const LEARNING_ARTIFACT_MODEL: &str = "learning-artifact";

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
        NodeConfig::new(
            "DigestRenderNode",
            vec!["LearningArtifactPayloadNode".to_string()],
        ),
    );
    nodes.insert(
        "LearningArtifactPayloadNode".to_string(),
        NodeConfig::new(
            "LearningArtifactPayloadNode",
            vec!["MaterializeDocNode".to_string()],
        ),
    );
    nodes.insert(
        "MaterializeDocNode".to_string(),
        NodeConfig::new("MaterializeDocNode", vec!["PersistToBrainNode".to_string()]),
    );
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec!["ActionDispatchNode".to_string()]),
    );
    nodes.insert(
        "ActionDispatchNode".to_string(),
        NodeConfig::new("ActionDispatchNode", vec![]),
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
    registry.register(Box::new(LearningArtifactPayloadNode));
    registry.register(Box::new(materialize_doc_node(
        &ContentPipelinePolicy::default().materialize,
    )));
    registry.register(Box::new(persist_to_brain_node(
        &ContentPipelinePolicy::default().harvest,
    )));
    registry.register(Box::new(ActionDispatchNode::new()));

    registry
}

/// Build the `MaterializeDocNode` instance this workflow registers, from
/// the resolved `materialize` knob (`EN.7.D`). One construction site for
/// both [`registry`] (built-in defaults) and [`registry_for_policy`] (the
/// run's resolved policy), so the two can never drift.
///
/// The node identity, its position in the graph, and its model
/// (`"learning-artifact"`) are invariant across every setting — only the
/// node's own configuration varies, per CLAUDE.md rule 6. `corpus_root`
/// left unset means the node resolves the brain root at run time via
/// `crate::brain_root::resolve_brain_root` (`ENGINE_BRAIN_ROOT`, then
/// walk-up), exactly as `RESEARCH_AGENT`'s opportunity instance does.
fn materialize_doc_node(materialize: &MaterializeConfig) -> MaterializeDocNode {
    let node = MaterializeDocNode::new(LEARNING_ARTIFACT_MODEL)
        .with_source_node(learning_artifact::NODE_NAME)
        .with_enabled(materialize.enabled)
        .with_write(materialize.write);

    match &materialize.corpus_root {
        Some(root) => node.with_brain_root(root),
        None => node,
    }
}

/// Build the `PersistToBrainNode` instance this workflow registers, from
/// the resolved `harvest` knob (`EN.7.C`). One construction site for both
/// [`registry`] (built-in defaults) and [`registry_for_policy`] (the run's
/// resolved policy), so the two can never drift — same pattern as
/// [`materialize_doc_node`].
///
/// The node identity, its position in the graph, and its target URL are
/// invariant across every setting; only whether/when the push happens
/// varies, per CLAUDE.md rule 6.
fn persist_to_brain_node(harvest: &HarvestConfig) -> PersistToBrainNode {
    PersistToBrainNode::new().with_harvest(HarvestGate::new(harvest.mode))
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
/// route through [`openai_compat_meta_transport_live`] (falling back to the real
/// `claude` CLI transport on any local-endpoint failure). **Never**
/// rewires the fetch/normalize/render/persist/dispatch stages — they carry
/// no `ModelTier` field and are not model nodes at all (architecture.md
/// §5); `ActionDispatchNode`'s `dispatch` stage is deterministic egress and
/// is never Local-eligible (`policy.rs`'s `dispatch_verbosity` is
/// telemetry/verbosity config only, not a `ModelTier`).
/// This is the four-stage analog of `proposal_generator::graph::registry_for_policy`.
///
/// It also applies `EN.7.D`'s `materialize` knob to the `MaterializeDocNode`
/// instance (enabled / write / corpus root), and `EN.7.C`'s `harvest` knob
/// to the `PersistToBrainNode` instance (off / in_process / approval). Both
/// are *configuration* changes only: the node identity set this returns is
/// identical for every policy, so one declared graph, validated once,
/// describes every run.
#[must_use]
pub fn registry_for_policy(policy: &ContentPipelinePolicy) -> NodeRegistry {
    let mut registry = registry();

    // EN.7.D: re-register the same identity with the run's resolved
    // `materialize` knob. This replaces a node's configuration, never the
    // node SET — `registry()` already registered this identity, so the
    // declared graph is identical for every setting.
    registry.register(Box::new(materialize_doc_node(&policy.materialize)));

    // EN.7.C: re-register the same identity with the run's resolved
    // `harvest` knob. Configuration, never the node SET — `registry()`
    // already registered this identity, so the declared graph is identical
    // for every harvest mode.
    registry.register(Box::new(persist_to_brain_node(&policy.harvest)));

    if policy.model_tiers.summarize == ModelTier::Local {
        registry.register(Box::new(SummarizeNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.critic == ModelTier::Local {
        registry.register(Box::new(SelfCriticNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.revise == ModelTier::Local {
        registry.register(Box::new(ReviseNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.translate == ModelTier::Local {
        registry.register(Box::new(TranslateNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
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
    use crate::node::Node;
    use crate::validate::WorkflowValidator;

    const ALL_NODE_IDENTITIES: [&str; 16] = [
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
        "LearningArtifactPayloadNode",
        "MaterializeDocNode",
        "PersistToBrainNode",
        "ActionDispatchNode",
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
    fn persist_to_brain_forwards_to_action_dispatch() {
        let schema = schema();
        assert_eq!(
            schema.nodes["PersistToBrainNode"].connections,
            vec!["ActionDispatchNode".to_string()]
        );
    }

    #[test]
    fn action_dispatch_is_terminal() {
        let schema = schema();
        assert!(schema.nodes["ActionDispatchNode"].connections.is_empty());
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
        // `NormalizeChannelContentNode`, `DigestRenderNode`,
        // `PersistToBrainNode`, and `ActionDispatchNode` have no
        // `ModelTier` field at all — there is no branch in
        // `registry_for_policy` that could rewire them, by construction.
        // This test documents that: a policy with every model-eligible
        // tier set to Local still leaves those six identities registered
        // (i.e. present, not silently dropped) with no policy hook
        // available to touch them.
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
            "ActionDispatchNode",
        ] {
            assert!(registry.contains(identity));
        }
        assert_eq!(registry.len(), super::registry().len());
    }

    #[test]
    fn registry_for_policy_leaves_action_dispatch_untouched_under_local_profile() {
        // Local profile (all four model-eligible stages set to Local)
        // must not add, remove, or otherwise disturb the `dispatch` stage
        // — `ActionDispatchNode` carries no `ModelTier` and is not part of
        // the Local rewire at all.
        let mut policy = ContentPipelinePolicy::default();
        policy.model_tiers.summarize = ModelTier::Local;
        policy.model_tiers.critic = ModelTier::Local;
        policy.model_tiers.revise = ModelTier::Local;
        policy.model_tiers.translate = ModelTier::Local;

        let registry = registry_for_policy(&policy);
        assert!(registry.contains("ActionDispatchNode"));
        assert_eq!(registry.len(), super::registry().len());
    }

    // EN.7.D task 6 — the materialize tail.

    #[test]
    fn digest_render_forwards_to_the_learning_artifact_payload_node() {
        let schema = schema();
        assert_eq!(
            schema.nodes["DigestRenderNode"].connections,
            vec!["LearningArtifactPayloadNode".to_string()]
        );
    }

    #[test]
    fn materialize_tail_runs_before_the_synapse_push() {
        // The block's ordering claim: the source `.md` is written before
        // `PersistToBrainNode` can halt the run on a non-2xx.
        let schema = schema();
        assert_eq!(
            schema.nodes["LearningArtifactPayloadNode"].connections,
            vec!["MaterializeDocNode".to_string()]
        );
        assert_eq!(
            schema.nodes["MaterializeDocNode"].connections,
            vec!["PersistToBrainNode".to_string()]
        );
    }

    #[test]
    fn registry_for_policy_node_set_is_identical_across_every_materialize_setting() {
        let identities = |registry: &NodeRegistry| {
            let mut found: Vec<&str> = ALL_NODE_IDENTITIES
                .iter()
                .copied()
                .filter(|identity| registry.contains(identity))
                .collect();
            found.sort_unstable();
            found
        };

        let baseline = registry_for_policy(&ContentPipelinePolicy::default());

        let mut disabled = ContentPipelinePolicy::default();
        disabled.materialize.enabled = false;

        let mut dry_run = ContentPipelinePolicy::default();
        dry_run.materialize.write = false;

        let mut pinned_root = ContentPipelinePolicy::default();
        pinned_root.materialize.corpus_root = Some("/tmp/some-corpus".to_string());

        for policy in [&disabled, &dry_run, &pinned_root] {
            let registry = registry_for_policy(policy);
            assert_eq!(
                identities(&registry),
                identities(&baseline),
                "the materialize knob must change node CONFIGURATION, never the node set"
            );
            assert_eq!(registry.len(), baseline.len());
        }
    }

    #[test]
    fn declared_graph_still_validates_with_materialization_disabled() {
        // Shape invariance is only meaningful if the disabled-knob registry
        // still satisfies the one declared schema.
        let mut policy = ContentPipelinePolicy::default();
        policy.materialize.enabled = false;

        WorkflowValidator::validate(&registry_for_policy(&policy), &schema())
            .expect("declared graph should validate with materialization off");
    }

    #[test]
    fn materialize_doc_node_is_built_for_the_learning_artifact_model() {
        // The generality claim, asserted at the construction site: this
        // workflow differs from `RESEARCH_AGENT`'s opportunity instance
        // only by the model string and the source node.
        assert_eq!(LEARNING_ARTIFACT_MODEL, "learning-artifact");

        let node = materialize_doc_node(&MaterializeConfig::default());
        assert_eq!(node.name(), "MaterializeDocNode");
    }

    // EN.7.C task 5 — the harvest gate wired into the graph.

    use crate::nodes::harvest_gate::HarvestMode;

    fn all_identities(registry: &NodeRegistry) -> Vec<&'static str> {
        let mut found: Vec<&str> = ALL_NODE_IDENTITIES
            .iter()
            .copied()
            .filter(|identity| registry.contains(identity))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn registry_and_registry_for_policy_register_the_same_identities() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&ContentPipelinePolicy::default());

        assert_eq!(
            all_identities(&policy_registry),
            all_identities(&default_registry)
        );
    }

    #[test]
    fn identity_set_is_identical_under_all_three_harvest_modes() {
        let baseline = registry_for_policy(&ContentPipelinePolicy::default());

        for mode in [
            HarvestMode::Off,
            HarvestMode::InProcess,
            HarvestMode::Approval,
        ] {
            let mut policy = ContentPipelinePolicy::default();
            policy.harvest.mode = mode;

            let registry = registry_for_policy(&policy);
            assert_eq!(
                all_identities(&registry),
                all_identities(&baseline),
                "the harvest knob must change node CONFIGURATION, never the node set"
            );
            assert_eq!(registry.len(), baseline.len());
        }
    }

    #[test]
    fn schema_is_unchanged_by_the_harvest_knob() {
        // `persist_to_brain_forwards_to_action_dispatch` above already
        // asserts this connection; re-asserted here as the harvest-knob
        // acceptance criterion so it reads as a Step 5 guarantee, not an
        // incidental consequence of the materialize tests.
        let schema = schema();
        assert_eq!(
            schema.nodes["PersistToBrainNode"].connections,
            vec!["ActionDispatchNode".to_string()]
        );
    }

    #[test]
    fn declared_graph_validates_under_every_harvest_mode() {
        for mode in [
            HarvestMode::Off,
            HarvestMode::InProcess,
            HarvestMode::Approval,
        ] {
            let mut policy = ContentPipelinePolicy::default();
            policy.harvest.mode = mode;

            WorkflowValidator::validate(&registry_for_policy(&policy), &schema()).unwrap_or_else(
                |err| panic!("declared graph should validate under harvest mode {mode:?}: {err}"),
            );
        }
    }

    #[test]
    fn persist_to_brain_node_is_built_with_the_resolved_harvest_mode() {
        let node = persist_to_brain_node(&HarvestConfig {
            mode: HarvestMode::InProcess,
        });
        assert_eq!(node.name(), "PersistToBrainNode");
    }
}
