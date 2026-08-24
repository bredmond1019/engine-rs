//! The declared `WorkflowSchema` / `NodeRegistry` / `registry_for_policy` /
//! `Workflow` assembly for `LINKEDIN_POST` (`EN.5.G` task 6). `WORKFLOW_TYPE`
//! lives here, mirroring `content_pipeline::graph` / `proposal_generator::graph`.
//!
//! Declared graph shape (source of truth: `planning/EN.5.G/tasks.md` +
//! `tasks.json` task 6):
//!
//! ```text
//! WorkSourceNode -> PostDraftNode -> PostCandidateSelectNode -> BrandCriticNode
//!   -> CriticRouterNode -> { TranslateNode (exit)
//!      | IncrementCriticIterationNode -> ReviseNode -> BrandCriticNode }  // back-edge
//! ```
//!
//! ## Two adapters this task adds, and why
//!
//! Tasks 1-5 are already committed on this branch and this task does not
//! revise them — but two of their shapes do not fit together without a
//! seam this task supplies:
//!
//! 1. **`PostCandidateSelectNode`.** `PostDraftNode` (task 4) stores
//!    `{"candidates": [...], "unsupported_claims": [...]}` — a whole array
//!    of `PostCandidate`s. `BrandCriticNode`/`ReviseNode` (task 5) each
//!    critique/revise **one** draft at a time: their default upstream read
//!    (`draft::NODE_NAME`, unbound) looks for a top-level `"draft"` string,
//!    which `PostDraftNode`'s stored shape does not have. This node bridges
//!    the two: it selects the first (primary) candidate and stores
//!    `{"draft", "sources"}` under its own identity, and `BrandCriticNode`/
//!    `ReviseNode` are both wired with `.with_draft_input_from(..)` pointed
//!    at it instead of their unbound `PostDraftNode` default. Critiquing
//!    only the primary candidate (not fanning the loop out over all of
//!    them) is a deliberate task-6 scoping choice — tasks.json's declared
//!    shape is a single linear `BrandCriticNode` instance, and multi-
//!    candidate fan-out is a larger change than "graph assembly +
//!    registration" covers.
//! 2. **`TranslateGateNode`.** tasks.md's Context Pointers say `TranslateNode`
//!    is "reused from `content_pipeline` as the PT starting point," and
//!    `policy.rs`'s own doc comment says `translate_enabled: false` "routes
//!    `TranslateNode` to its no-op path." But
//!    `content_pipeline::translate::TranslateNode::process` reads
//!    `content_pipeline::source_router::NODE_NAME` for its policy and
//!    `ContentPipelineInput` off `ctx.event` for `target_lang` — both
//!    `CONTENT_PIPELINE`-specific keys `LINKEDIN_POST` never populates, so
//!    the literal type would error every real run (`"no policy stored by
//!    SourceRouterNode"`) rather than silently no-op. This module defines
//!    its own `TranslateGateNode` under the same `"TranslateNode"` identity
//!    instead: it resolves `LinkedInPostPolicy` via
//!    `crate::policy::resolved_policy_strict` (the shared, workflow-generic
//!    seam every other node in this module already uses) and takes the
//!    documented no-op path when `translate_enabled` is `false` — never
//!    removed from the declared graph, never skipped by a router, per
//!    CLAUDE.md standing rule 6's shape-invariance discipline.
//!
//! ## The critic loop's back-edge — a local router, not `loop_combinator`
//!
//! `crate::loop_combinator::build_loop` (as `proposal_generator/graph.rs`
//! uses it) was the sketched approach in tasks.json, but its generic
//! increment node stores its counter as `{"iterations": N}` under an
//! identity derived from `LoopSpec::identity_prefix` — neither of which
//! matches what `brand_critic.rs`'s `read_iteration` (task 5, already
//! committed) actually reads by default: `content_pipeline::
//! increment_critic_iteration::NODE_NAME` ("IncrementCriticIterationNode"),
//! key `"iteration"` (singular). Using `build_loop` here would silently
//! desync `BrandCriticNode`'s own iteration-cap bookkeeping (its `capped`
//! marker) from the loop's actual guard. So this graph instead follows
//! `content_pipeline::critic_router`'s established, working precedent
//! directly: [`CriticRouterNode`] (a local `Router`, defined below) reads
//! `BrandCriticNode`'s stored `{"verdict", "capped"}` — both already
//! computed by `BrandCriticNode::process` itself, so this router
//! re-derives no policy/threshold logic of its own — and routes to
//! `TranslateNode` (pass, or the iteration cap already fired) or
//! `content_pipeline::increment_critic_iteration::IncrementCriticIterationNode`
//! (continue), reused unmodified: it is policy-agnostic and keyed only by
//! its own identity, so it needs no linkedin_post-specific copy (exactly
//! task 5's own doc comment says). `IncrementCriticIterationNode` forwards
//! to `ReviseNode` (linkedin_post's own, task 5) via a plain declared
//! connection, and `ReviseNode` forwards back to `BrandCriticNode` — a
//! declared connection reachable only through `CriticRouterNode`'s runtime
//! `Router::route` decision, never a declared non-router cycle, so
//! `WorkflowValidator`'s DFS cycle check skips it exactly as it does for
//! `content_pipeline`'s `ReviseNode -> SelfCriticNode` back-edge (D42).
//!
//! ## Node-set invariance across profiles (standing rule 6)
//!
//! None of the three composed model nodes (`PostDraftNode`, `BrandCriticNode`,
//! `ReviseNode`, `TranslateGateNode`) expose a `with_meta_transport` hook
//! onto `openai_compat_meta_transport_live` — unlike `CONTENT_PIPELINE`/
//! `PROPOSAL_GENERATOR`, a resolved `ModelTier::Local` here changes only
//! the `model` string `crate::policy::apply_model_tier` sets inside each
//! node's own `process`, not the transport route. So [`registry_for_policy`]
//! has no rewiring to perform and returns exactly [`registry`] regardless
//! of the policy passed in — this is deliberate, not a stub, and it is
//! what makes the declared node set trivially byte-identical across every
//! profile (translate on or off included): no branch ever adds, removes,
//! or swaps a registered identity.

use std::collections::HashMap;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::Value;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::nodes::ClaudeCodeStep;
use crate::routing::Router;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::content_pipeline::increment_critic_iteration::{
    self, IncrementCriticIterationNode,
};
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::brand_critic::{self, BrandCriticNode};
use super::draft::{self, PostDraftNode};
use super::policy::LinkedInPostPolicy;
use super::revise::{self, ReviseNode};
use super::schema::PostCandidate;
use super::work_source::WorkSourceNode;

/// The `LINKEDIN_POST` workflow's declared identity/type name, used both
/// to register the workflow (`engine-serve`) and as
/// `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "LINKEDIN_POST";

/// The `Node::name()` identity [`PostCandidateSelectNode`] is registered
/// under and stamps its result onto.
const CANDIDATE_SELECT_NODE_NAME: &str = "PostCandidateSelectNode";

/// The `Node::name()` identity [`CriticRouterNode`] is registered under.
const CRITIC_ROUTER_NODE_NAME: &str = "CriticRouterNode";

/// The `Node::name()` identity [`TranslateGateNode`] is registered under
/// and stamps its result onto — matches `content_pipeline::translate::
/// TranslateNode::NODE_NAME`'s string by convention (it is a distinct
/// type in a distinct workflow's registry, so there is no collision).
const TRANSLATE_NODE_NAME: &str = "TranslateNode";

/// Target language `TranslateGateNode` translates into when enabled. Not a
/// policy knob: `brand.md`'s PT-BR convention is a fixed target for this
/// workflow's translate pass, mirroring `content_pipeline::translate`'s own
/// default `target_lang`.
const TRANSLATE_TARGET_LANG: &str = "pt-BR";

/// Bridges `PostDraftNode`'s `{candidates, unsupported_claims}` array into
/// the single `{draft, sources}` shape `BrandCriticNode`/`ReviseNode`
/// expect — see the module doc comment's "Two adapters" section, item 1.
/// A pure deterministic `Node` (not a `Router`): it never chooses between
/// alternatives, so it declares exactly one outgoing connection.
struct PostCandidateSelectNode;

#[async_trait::async_trait]
impl Node for PostCandidateSelectNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let candidates: Vec<PostCandidate> = get_result(&ctx, draft::NODE_NAME)
            .and_then(|value| value.get("candidates").cloned())
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();

        let primary = candidates.into_iter().next().ok_or_else(|| {
            NodeError::new(format!(
                "{CANDIDATE_SELECT_NODE_NAME}: no traceable candidates stored by {}",
                draft::NODE_NAME
            ))
        })?;

        put_result(
            &mut ctx,
            CANDIDATE_SELECT_NODE_NAME,
            serde_json::json!({ "draft": primary.draft, "sources": primary.sources }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        CANDIDATE_SELECT_NODE_NAME
    }
}

/// The bounded brand-critic loop's guard — see the module doc comment's
/// "critic loop's back-edge" section for why this is a local router
/// rather than a reuse of `content_pipeline::critic_router::CriticRouterNode`
/// (which is hardcoded to `self_critic`/`source_router`'s identities) or
/// `crate::loop_combinator::build_loop` (whose generic counter shape does
/// not match what `brand_critic.rs` already reads).
///
/// `process` is a pure pass-through — `Router::route` takes `&TaskContext`
/// and cannot mutate it, so there is nothing for `process` to stage; every
/// input `route` needs was already stored by `BrandCriticNode`.
struct CriticRouterNode;

#[async_trait::async_trait]
impl Node for CriticRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        CRITIC_ROUTER_NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for CriticRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let stored = get_result(ctx, brand_critic::NODE_NAME)?;
        let verdict_is_pass = stored.get("verdict").and_then(Value::as_str) == Some("pass");
        let capped = stored
            .get("capped")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Some(if verdict_is_pass || capped {
            TRANSLATE_NODE_NAME.to_string()
        } else {
            increment_critic_iteration::NODE_NAME.to_string()
        })
    }
}

/// The model's reply shape for [`TranslateGateNode`]'s enabled path.
#[derive(Debug, Clone, serde::Deserialize)]
struct TranslateGateReply {
    translated_markdown: String,
}

/// JSON schema matching [`TranslateGateReply`].
fn translation_json_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "translated_markdown": { "type": "string" },
        },
        "required": ["translated_markdown"],
    })
}

/// The terminal `translate`-stage node — see the module doc comment's "Two
/// adapters" section, item 2, for why this is a local reimplementation
/// rather than a reuse of `content_pipeline::translate::TranslateNode`.
struct TranslateGateNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl TranslateGateNode {
    /// Construct with the translation `json_schema` set; `process`
    /// overwrites `model` per the resolved `translate`-stage tier.
    #[must_use]
    fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(translation_json_schema()),
                ..Config::default()
            },
            transport: None,
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`. Only reached when
    /// `translate_enabled` is `true` — the no-op path never calls the
    /// transport at all. Not called by [`registry`] (the default,
    /// real-transport registration) — only by this module's own tests —
    /// so it is unused outside `#[cfg(test)]` builds; `TranslateGateNode`
    /// is a private, unexported type, so `pub` visibility alone does not
    /// exempt it from the dead-code lint the way the sibling
    /// `content_pipeline`/`draft`/`revise` nodes' public `with_transport`
    /// is.
    #[allow(dead_code)]
    #[must_use]
    fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }
}

/// Read the draft text this node translates: prefers `ReviseNode`'s
/// revised output (a revise pass happened), falling back to
/// `PostCandidateSelectNode`'s pre-revision draft (no revise pass ran) —
/// the same read-preference precedent `BrandCriticNode`/`ReviseNode`
/// already use for `revise::NODE_NAME` ahead of the bound draft identity.
fn read_final_draft(ctx: &TaskContext) -> Result<String, NodeError> {
    get_result(ctx, revise::NODE_NAME)
        .or_else(|| get_result(ctx, CANDIDATE_SELECT_NODE_NAME))
        .and_then(|value| {
            value
                .get("draft")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| {
            NodeError::new(format!(
                "{TRANSLATE_NODE_NAME}: no draft stored by {} or {CANDIDATE_SELECT_NODE_NAME}",
                revise::NODE_NAME
            ))
        })
}

fn build_translate_prompt(draft: &str) -> String {
    format!(
        "Translate the following LinkedIn post draft into {TRANSLATE_TARGET_LANG}, preserving \
         meaning, voice, and markdown structure. Respond with strict JSON matching \
         {{\"translated_markdown\": string}}.\n\n\
         Draft:\n{draft}"
    )
}

#[async_trait::async_trait]
impl Node for TranslateGateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let policy: LinkedInPostPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        // The documented no-op path (`policy.rs`'s own doc comment on
        // `translate_enabled`): the node stays in the declared graph and
        // still runs, it simply never calls the model.
        if !policy.translate_enabled {
            put_result(
                &mut ctx,
                TRANSLATE_NODE_NAME,
                serde_json::json!({ "translated": false }),
            );
            return Ok(ctx);
        }

        let draft = read_final_draft(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.translate,
            &policy.local.model,
        );
        let prompt = build_translate_prompt(&draft);

        let mut step = ClaudeCodeStep::new(TRANSLATE_NODE_NAME, config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(TRANSLATE_NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let parsed: TranslateGateReply =
            parse_structured_or_fenced(&ctx, TRANSLATE_NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                "{TRANSLATE_NODE_NAME}: failed to parse a translation from the model's reply: {err}"
            ))
            })?;

        put_result(
            &mut ctx,
            TRANSLATE_NODE_NAME,
            serde_json::json!({
                "translated": true,
                "translated_markdown": parsed.translated_markdown,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        TRANSLATE_NODE_NAME
    }
}

/// Build the declared `WorkflowSchema` for the `LINKEDIN_POST` workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        work_source_node_name().to_string(),
        NodeConfig::new(work_source_node_name(), vec![draft::NODE_NAME.to_string()]),
    );
    nodes.insert(
        draft::NODE_NAME.to_string(),
        NodeConfig::new(
            draft::NODE_NAME,
            vec![CANDIDATE_SELECT_NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        CANDIDATE_SELECT_NODE_NAME.to_string(),
        NodeConfig::new(
            CANDIDATE_SELECT_NODE_NAME,
            vec![brand_critic::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        brand_critic::NODE_NAME.to_string(),
        NodeConfig::new(
            brand_critic::NODE_NAME,
            vec![CRITIC_ROUTER_NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        CRITIC_ROUTER_NODE_NAME.to_string(),
        NodeConfig::new(
            CRITIC_ROUTER_NODE_NAME,
            vec![
                TRANSLATE_NODE_NAME.to_string(),
                increment_critic_iteration::NODE_NAME.to_string(),
            ],
        ),
    );
    nodes.insert(
        increment_critic_iteration::NODE_NAME.to_string(),
        NodeConfig::new(
            increment_critic_iteration::NODE_NAME,
            vec![revise::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        // Back-edge: reachable only through `CriticRouterNode`'s runtime
        // `Router::route`, never walked as a declared non-router cycle
        // (D42) — see module doc comment.
        revise::NODE_NAME.to_string(),
        NodeConfig::new(revise::NODE_NAME, vec![brand_critic::NODE_NAME.to_string()]),
    );
    nodes.insert(
        TRANSLATE_NODE_NAME.to_string(),
        NodeConfig::new(TRANSLATE_NODE_NAME, vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, work_source_node_name(), nodes)
}

/// `WorkSourceNode`'s registered identity — read via `Node::name()` on a
/// fresh instance rather than a hardcoded literal, so this module never
/// drifts from `work_source.rs`'s own `NODE_NAME` constant.
fn work_source_node_name() -> &'static str {
    super::work_source::NODE_NAME
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport) configuration. Tests
/// build their own registry with stubbed transports instead of calling
/// this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(WorkSourceNode::new()));
    registry.register(Box::new(PostDraftNode::new()));
    registry.register(Box::new(PostCandidateSelectNode));
    registry.register(Box::new(
        BrandCriticNode::new().with_draft_input_from(CANDIDATE_SELECT_NODE_NAME),
    ));
    registry.register(Box::new(CriticRouterNode));
    registry.register(Box::new(IncrementCriticIterationNode));
    registry.register(Box::new(
        ReviseNode::new().with_draft_input_from(CANDIDATE_SELECT_NODE_NAME),
    ));
    registry.register(Box::new(TranslateGateNode::new()));

    registry
}

/// Build a `NodeRegistry` like [`registry`] for a resolved `policy` — see
/// the module doc comment's "Node-set invariance across profiles" section
/// for why this performs no rewiring and simply returns [`registry`]: none
/// of this workflow's composed model nodes expose a `with_meta_transport`
/// hook, so there is nothing a `ModelTier::Local` resolution could rewire
/// here (unlike `CONTENT_PIPELINE`/`PROPOSAL_GENERATOR`). The parameter is
/// still taken (and still named, not `_`) so this signature matches every
/// sibling `registry_for_policy` and the `engine-serve` registration below
/// compiles unchanged if that ever stops being true.
#[must_use]
pub fn registry_for_policy(policy: &LinkedInPostPolicy) -> NodeRegistry {
    let _ = policy;
    registry()
}

/// Build the runnable `LINKEDIN_POST` `Workflow`: [`registry`] paired with
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
        .expect("LINKEDIN_POST declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap as StdHashMap;

    use serde_json::json;

    use super::*;
    use crate::validate::WorkflowValidator;

    const ALL_NODE_IDENTITIES: [&str; 8] = [
        "WorkSourceNode",
        "PostDraftNode",
        "PostCandidateSelectNode",
        "BrandCriticNode",
        "CriticRouterNode",
        "IncrementCriticIterationNode",
        "ReviseNode",
        "TranslateNode",
    ];

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_work_source() {
        assert_eq!(schema().start_node, "WorkSourceNode");
    }

    #[test]
    fn workflow_type_is_linkedin_post() {
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
    fn critic_router_declares_both_branches() {
        let schema = schema();
        let registry = registry();

        let router_config = &schema.nodes["CriticRouterNode"];
        for branch in ["TranslateNode", "IncrementCriticIterationNode"] {
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
    fn revise_node_back_edge_targets_brand_critic() {
        let schema = schema();
        let revise_config = &schema.nodes["ReviseNode"];
        assert_eq!(
            revise_config.connections,
            vec!["BrandCriticNode".to_string()]
        );
    }

    #[test]
    fn translate_node_is_terminal() {
        let schema = schema();
        assert!(schema.nodes["TranslateNode"].connections.is_empty());
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn registry_for_policy_matches_plain_registry_under_default_policy() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&LinkedInPostPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        for identity in ALL_NODE_IDENTITIES {
            assert!(policy_registry.contains(identity));
        }
    }

    #[test]
    fn declared_node_set_is_byte_identical_across_all_three_profiles() {
        let identities = |registry: &NodeRegistry| {
            let mut found: Vec<&str> = ALL_NODE_IDENTITIES
                .iter()
                .copied()
                .filter(|identity| registry.contains(identity))
                .collect();
            found.sort_unstable();
            found
        };

        let baseline = identities(&registry_for_policy(&LinkedInPostPolicy::default()));

        let cheap_fast = super::super::profiles::cheap_fast();
        let thorough = super::super::profiles::thorough();
        for partial in [&cheap_fast, &thorough] {
            let policy = <LinkedInPostPolicy as crate::policy::Policy>::apply(
                LinkedInPostPolicy::default(),
                partial,
            );
            let registry = registry_for_policy(&policy);
            assert_eq!(
                identities(&registry),
                baseline,
                "the resolved policy must change node CONFIGURATION, never the node set"
            );
            assert_eq!(registry.len(), ALL_NODE_IDENTITIES.len());
        }
    }

    #[test]
    fn declared_node_set_is_identical_with_translate_off() {
        let identities = |registry: &NodeRegistry| {
            let mut found: Vec<&str> = ALL_NODE_IDENTITIES
                .iter()
                .copied()
                .filter(|identity| registry.contains(identity))
                .collect();
            found.sort_unstable();
            found
        };

        let baseline = identities(&registry_for_policy(&LinkedInPostPolicy::default()));

        let translate_off = LinkedInPostPolicy {
            translate_enabled: false,
            ..LinkedInPostPolicy::default()
        };
        let registry = registry_for_policy(&translate_off);

        assert_eq!(identities(&registry), baseline);
        assert!(registry.contains("TranslateNode"));
    }

    #[test]
    fn declared_graph_still_validates_with_translate_off() {
        let translate_off = LinkedInPostPolicy {
            translate_enabled: false,
            ..LinkedInPostPolicy::default()
        };

        WorkflowValidator::validate(&registry_for_policy(&translate_off), &schema())
            .expect("declared graph should validate with translate off");
    }

    // --- CriticRouterNode::route ------------------------------------------

    fn ctx_with_brand_critic(verdict: &str, capped: bool) -> TaskContext {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            brand_critic::NODE_NAME.to_string(),
            json!({
                "verdict": verdict,
                "confidence": 0.5,
                "issues": [],
                "iteration": 0,
                "capped": capped,
            }),
        );
        TaskContext {
            event: json!({}),
            nodes,
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        }
    }

    #[test]
    fn critic_router_exits_to_translate_on_pass() {
        let router = CriticRouterNode;
        let ctx = ctx_with_brand_critic("pass", false);
        assert_eq!(router.route(&ctx), Some("TranslateNode".to_string()));
    }

    #[test]
    fn critic_router_exits_to_translate_when_capped_even_on_revise() {
        let router = CriticRouterNode;
        let ctx = ctx_with_brand_critic("revise", true);
        assert_eq!(router.route(&ctx), Some("TranslateNode".to_string()));
    }

    #[test]
    fn critic_router_continues_to_increment_on_uncapped_revise() {
        let router = CriticRouterNode;
        let ctx = ctx_with_brand_critic("revise", false);
        assert_eq!(
            router.route(&ctx),
            Some(increment_critic_iteration::NODE_NAME.to_string())
        );
    }

    #[test]
    fn critic_router_returns_none_when_brand_critic_has_not_run() {
        let router = CriticRouterNode;
        let ctx = TaskContext {
            event: json!({}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        assert_eq!(router.route(&ctx), None);
    }

    #[tokio::test]
    async fn critic_router_process_is_a_pure_passthrough() {
        let router = CriticRouterNode;
        let ctx = ctx_with_brand_critic("pass", false);
        let before = ctx.nodes.clone();

        let after = router.process(ctx).await.expect("process should succeed");

        assert_eq!(after.nodes, before);
    }

    #[test]
    fn critic_router_as_router_is_some() {
        let router = CriticRouterNode;
        assert!(router.as_router().is_some());
    }

    // --- PostCandidateSelectNode -------------------------------------------

    fn ctx_with_candidates(candidates: serde_json::Value) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        put_result(
            &mut ctx,
            draft::NODE_NAME,
            json!({ "candidates": candidates, "unsupported_claims": [] }),
        );
        ctx
    }

    fn fixture_candidate(draft: &str) -> serde_json::Value {
        json!({
            "angle": "shipped a thing",
            "draft": draft,
            "sources": [{ "kind": "commit", "id": "abc123", "summary": "did a thing" }],
        })
    }

    #[tokio::test]
    async fn candidate_select_stores_the_first_candidates_draft_and_sources() {
        let node = PostCandidateSelectNode;
        let ctx = ctx_with_candidates(json!([
            fixture_candidate("first draft"),
            fixture_candidate("second draft"),
        ]));

        let result = node.process(ctx).await.expect("process should succeed");
        let stored = result
            .nodes
            .get(CANDIDATE_SELECT_NODE_NAME)
            .expect("stored");

        assert_eq!(stored["draft"], json!("first draft"));
        assert_eq!(stored["sources"][0]["id"], json!("abc123"));
    }

    #[tokio::test]
    async fn candidate_select_errors_on_no_candidates() {
        let node = PostCandidateSelectNode;
        let ctx = ctx_with_candidates(json!([]));

        let err = node.process(ctx).await.expect_err("should error");
        assert!(err.to_string().contains(draft::NODE_NAME));
    }

    // --- TranslateGateNode ---------------------------------------------------

    fn ctx_with_final_draft(policy: LinkedInPostPolicy) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        put_result(
            &mut ctx,
            CANDIDATE_SELECT_NODE_NAME,
            json!({ "draft": "a clean draft", "sources": [] }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        ctx
    }

    #[tokio::test]
    async fn translate_gate_takes_the_no_op_path_when_disabled() {
        let node = TranslateGateNode::new();
        let policy = LinkedInPostPolicy {
            translate_enabled: false,
            ..LinkedInPostPolicy::default()
        };
        let ctx = ctx_with_final_draft(policy);

        let result = node.process(ctx).await.expect("process should succeed");
        let stored = result.nodes.get(TRANSLATE_NODE_NAME).expect("stored");

        assert_eq!(stored["translated"], json!(false));
    }

    #[tokio::test]
    async fn translate_gate_calls_the_transport_and_stores_translation_when_enabled() {
        use std::collections::BTreeMap;

        use claude_code_rs::parse::Usage as SdkUsage;
        use claude_code_rs::Outcome;
        use futures::FutureExt;

        let transport: ModelTransport = std::sync::Arc::new(|_config, _prompt| {
            async move {
                let structured = json!({ "translated_markdown": "um rascunho limpo" });
                Ok(Outcome {
                    text: structured.to_string(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(structured),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = TranslateGateNode::new().with_transport(transport);
        let policy = LinkedInPostPolicy {
            translate_enabled: true,
            ..LinkedInPostPolicy::default()
        };
        let ctx = ctx_with_final_draft(policy);

        let result = node.process(ctx).await.expect("process should succeed");
        let stored = result.nodes.get(TRANSLATE_NODE_NAME).expect("stored");

        assert_eq!(stored["translated"], json!(true));
        assert_eq!(stored["translated_markdown"], json!("um rascunho limpo"));
    }

    #[tokio::test]
    async fn translate_gate_prefers_the_revised_draft_over_the_selected_one() {
        let mut ctx = ctx_with_final_draft(LinkedInPostPolicy {
            translate_enabled: false,
            ..LinkedInPostPolicy::default()
        });
        put_result(
            &mut ctx,
            revise::NODE_NAME,
            json!({ "draft": "a revised draft", "sources": [] }),
        );

        let draft = read_final_draft(&ctx).expect("should read a draft");
        assert_eq!(draft, "a revised draft");
    }
}
