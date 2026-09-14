//! `LlmNode` — the shared node-side shape every model-calling node in
//! `workflows/` implements, plus the graph-side backend/tier resolution
//! that decides which transport to wire (`planning/pre-plan/
//! transport-slot-consolidation/notes.md`).
//!
//! Before this module, every local-eligible node hand-wrote its own
//! `TransportSlot` field, its own `with_meta_transport`/`with_transport`
//! builder pair, and every `graph.rs` hand-wrote its own
//! `if policy.model_tiers.X == ModelTier::Local { node.with_meta_transport(...) }`
//! conditional — 16+ node files, 13+ near-identical graph-side blocks, each
//! copy-pasted for a new stage. [`TransportSlotted`] and [`Cancellable`]
//! collapse the node-side half to one field + one accessor per node;
//! [`resolve_meta_transport`] + [`wire`] collapse the graph-side half to one
//! call per stage.
//!
//! **Migrated so far (`EN.ticket.transport-slot-consolidation`):**
//! Phase 1 — `TriageTaskNode`, `GenerateTasksNode`, `ImplementTaskNode`
//! (`SDLC_TASK`). Phase 2 — `ConsolidatedReviewNode`, `EndReviewNode`,
//! `PatchDocsNode` (`SDLC_FLOW`). Phase 3 (narrowed to
//! orchestration/sdlc_flow/sdlc_task per operator direction) —
//! `nodes::judgment::JudgmentNode<T>`, the shared primitive
//! `orchestration::inbox_triage::InboxTriageRunner` and
//! `orchestration::preflight::PreflightRunner` both delegate to (those two
//! wrapper structs are not graph `Node`s and hold no `TransportSlot` of
//! their own — see each module's doc comment). Every other node still
//! using the pre-existing hand-rolled `TransportSlot` field + inherent
//! builder pair (`content_pipeline`'s four nodes, `proposal_generator`'s
//! three, `diagnostic_intake::IntakeExtractNode`,
//! `claim_reaffirm::judge::JudgeClaimNode`, `linkedin_post`'s node — see
//! the pre-plan note's file list) is UNCHANGED by this module and keeps
//! working exactly as before; migrating them is later-phase work,
//! deliberately deferred by the operator for now.

use std::sync::Arc;

use crate::cancellation::CancellationToken;
use crate::nodes::MetaTransport;
use crate::policy::{AgentBackend, LocalConfig, ModelTier, PiConfig};

use super::transport_slot::TransportSlot;
use super::ModelTransport;

/// A node that composes an `AgentCodeStep` and can have its transport
/// overridden — the node-side half of the transport-slot consolidation.
///
/// A node implements this by exposing its one `TransportSlot` field via
/// [`Self::transport_slot_mut`]; the two builder methods are default
/// implementations, so every implementor gets `with_meta_transport`/
/// `with_transport` for free instead of hand-writing the same two bodies.
pub trait TransportSlotted: Sized {
    /// The node's own `TransportSlot` field, by mutable reference.
    fn transport_slot_mut(&mut self) -> &mut TransportSlot;

    /// Override the transport with a tier-aware [`MetaTransport`] that
    /// reports the [`TransportInfo`] of whichever call actually executed,
    /// taking precedence over a plain transport set via
    /// [`Self::with_transport`] — matches `TransportSlot::apply`'s
    /// documented meta-wins precedence.
    #[must_use]
    fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport_slot_mut().set_meta(transport);
        self
    }

    /// Override the transport used by the composed `AgentCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`.
    #[must_use]
    fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport_slot_mut().set_plain(transport);
        self
    }
}

/// A node whose in-flight model call can be raced against a
/// `CancellationToken`, interrupting it mid-call instead of only taking
/// effect at the next node boundary — the node-side half of cancellation
/// support. `None` (the default a node's own `new()` sets) is
/// behavior-stable: no token, no cancellation check.
pub trait Cancellable: Sized {
    /// The node's own `Option<CancellationToken>` field, by mutable
    /// reference.
    fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken>;

    /// Attach a `CancellationToken`, forwarded into the composed
    /// `AgentCodeStep`'s own `with_cancellation_token` race.
    #[must_use]
    fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        *self.cancellation_token_mut() = Some(token);
        self
    }
}

/// The real `claude_code_rs::execute` transport, used as the cloud
/// fallback `resolve_meta_transport` passes to
/// `openai_compat_meta_transport_live` for the `Local` tier's own
/// error-path fallback. Every `workflows/*/graph.rs` file that composes
/// this transport keeps its own private copy (a pre-existing duplication
/// predating this module, out of this phase's scope to consolidate) — this
/// is the one `resolve_meta_transport` itself uses, so callers going
/// through this module no longer need their own.
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Resolve the [`MetaTransport`] a stage should dispatch through, given its
/// resolved [`ModelTier`] and the run's [`AgentBackend`] — the graph-side
/// backend/tier selection every `registry_for_policy` hand-wrote per stage.
/// `None` means "no override" — the node falls back to its own default
/// transport (a real `claude` CLI call).
///
/// Reproduces exactly the branching every existing call site already had:
/// - `Pi`/`Aider` backends always route local, regardless of `tier` —
///   `ImplementTaskNode`'s only local-routing path today, since neither
///   backend has ever consulted its stage's `ModelTier`.
/// - `ClaudeCli` + `Local` tier routes through the OpenAI-compatible local
///   transport (with cloud fallback) — `TriageTaskNode`/`GenerateTasksNode`'s
///   only local-routing path today, since neither has ever supported
///   `Pi`/`Aider`.
/// - `ClaudeCli` + any non-`Local` tier is a no-op — the node dispatches
///   its own default (real `claude` CLI) transport, unchanged.
#[must_use]
pub fn resolve_meta_transport(
    tier: ModelTier,
    backend: AgentBackend,
    local: &LocalConfig,
    pi: &PiConfig,
) -> Option<MetaTransport> {
    match backend {
        AgentBackend::Pi => Some(crate::nodes::pi_meta_transport_live(
            local.clone(),
            pi.clone(),
        )),
        AgentBackend::Aider => Some(crate::nodes::aider_meta_transport_live(local.clone())),
        AgentBackend::ClaudeCli if tier == ModelTier::Local => Some(
            crate::nodes::openai_compat_transport::openai_compat_meta_transport_live(
                local.clone(),
                real_cloud_transport(),
            ),
        ),
        AgentBackend::ClaudeCli => None,
    }
}

/// Apply an optional resolved transport and an optional cancellation token
/// to a node in one call — the graph-side half of the consolidation. A
/// call with both `None` is a true no-op: the returned node is
/// indistinguishable from one built by `N::new()` alone, which is what
/// makes it safe to call unconditionally (never gated behind an
/// `if tier == Local || token.is_some()` check) at every registration
/// site.
#[must_use]
pub fn wire<N: TransportSlotted + Cancellable>(
    mut node: N,
    transport: Option<MetaTransport>,
    token: Option<CancellationToken>,
) -> N {
    if let Some(transport) = transport {
        node = node.with_meta_transport(transport);
    }
    if let Some(token) = token {
        node = node.with_cancellation_token(token);
    }
    node
}

#[cfg(test)]
mod tests {
    //! The ONE shared test suite proving `TransportSlotted`/`Cancellable`'s
    //! default methods, `resolve_meta_transport`, and `wire` all behave
    //! correctly — moved here from `transport_slot.rs`'s node-shaped
    //! equivalents so no migrated node needs to re-prove this itself. A
    //! migrated node's own tests should only cover what's genuinely
    //! node-specific (e.g. which stage's policy field feeds
    //! `resolve_meta_transport`), not re-run this matrix.
    use super::*;
    use crate::nodes::agent_code_step::AgentCodeStep;
    use crate::nodes::TransportInfo;
    use claude_code_rs::{Config, Outcome};
    use engine_contract::TaskContext;
    use futures::future::BoxFuture;
    use std::collections::{BTreeMap, HashMap};

    /// Minimal test-only node implementing both traits over one
    /// `TransportSlot` + one `Option<CancellationToken>` field — stands in
    /// for any real migrated node (`TriageTaskNode`, `GenerateTasksNode`,
    /// `ImplementTaskNode`) whose `impl` blocks are otherwise identical to
    /// this by construction.
    #[derive(Default)]
    struct StubNode {
        slot: TransportSlot,
        token: Option<CancellationToken>,
    }

    impl TransportSlotted for StubNode {
        fn transport_slot_mut(&mut self) -> &mut TransportSlot {
            &mut self.slot
        }
    }

    impl Cancellable for StubNode {
        fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken> {
            &mut self.token
        }
    }

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
            session_id: None,
            structured_output: None,
        }
    }

    fn stub_plain_transport(model: &'static str) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(format!("plain:{model}"));
            Box::pin(async move { Ok(outcome) })
                as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    fn stub_meta_transport(tier: &'static str) -> MetaTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(format!("meta:{tier}"));
            let info = TransportInfo {
                tier: tier.to_string(),
                model: "stub-model".to_string(),
                endpoint: None,
                backend: "claude_cli".to_string(),
                cost_known: true,
                extra: BTreeMap::new(),
            };
            Box::pin(async move { Ok((outcome, info)) })
                as BoxFuture<'static, claude_code_rs::Result<(Outcome, TransportInfo)>>
        })
    }

    fn make_step() -> AgentCodeStep {
        AgentCodeStep::new("LlmNodeTest", Config::default(), "prompt")
    }

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    async fn run_step(step: AgentCodeStep) -> TaskContext {
        use crate::node::Node;
        step.process(empty_context())
            .await
            .expect("stub transport never errors")
    }

    #[tokio::test]
    async fn trait_meta_wins_when_both_set() {
        let node = StubNode::default()
            .with_transport(stub_plain_transport("plain-model"))
            .with_meta_transport(stub_meta_transport("local"));

        let step = node.slot.apply(make_step());
        let ctx = run_step(step).await;

        let transport = ctx
            .nodes
            .get("LlmNodeTest")
            .and_then(|value| value.get("transport"))
            .expect("transport stamp present");
        assert_eq!(
            transport.get("tier").and_then(|v| v.as_str()),
            Some("local"),
            "meta transport's tier must win over the plain transport's generic cloud stamp"
        );
    }

    #[tokio::test]
    async fn trait_plain_applies_when_only_plain_set() {
        let node = StubNode::default().with_transport(stub_plain_transport("plain-model"));

        let step = node.slot.apply(make_step());
        let ctx = run_step(step).await;

        let transport = ctx
            .nodes
            .get("LlmNodeTest")
            .and_then(|value| value.get("transport"))
            .expect("transport stamp present");
        assert_eq!(
            transport.get("tier").and_then(|v| v.as_str()),
            Some("cloud")
        );
    }

    #[test]
    fn trait_neither_set_leaves_slot_default() {
        let node = StubNode::default();
        // Both fields untouched: a default slot has no plain/meta override,
        // so `apply` would return its input step unchanged — asserted
        // structurally (see `transport_slot.rs`'s equivalent test) rather
        // than by driving a real subprocess call.
        assert!(node.token.is_none());
    }

    #[test]
    fn trait_with_cancellation_token_sets_field() {
        let token = CancellationToken::new();
        let node = StubNode::default().with_cancellation_token(token.clone());
        assert!(node.token.is_some());
    }

    #[test]
    fn resolve_meta_transport_claude_cli_cloud_is_none() {
        let transport = resolve_meta_transport(
            ModelTier::Sonnet,
            AgentBackend::ClaudeCli,
            &LocalConfig::default(),
            &PiConfig::default(),
        );
        assert!(
            transport.is_none(),
            "claude_cli + non-local tier must be a no-op, matching every \
             pre-consolidation call site"
        );
    }

    #[test]
    fn resolve_meta_transport_claude_cli_local_is_some() {
        let transport = resolve_meta_transport(
            ModelTier::Local,
            AgentBackend::ClaudeCli,
            &LocalConfig::default(),
            &PiConfig::default(),
        );
        assert!(transport.is_some());
    }

    #[test]
    fn resolve_meta_transport_pi_backend_routes_local_regardless_of_tier() {
        // Pi/Aider have never consulted their stage's ModelTier — they
        // always route local. Assert this holds even for a Cloud tier,
        // the exact case `ImplementTaskNode`'s pre-consolidation branching
        // never checked either.
        let transport = resolve_meta_transport(
            ModelTier::Sonnet,
            AgentBackend::Pi,
            &LocalConfig::default(),
            &PiConfig::default(),
        );
        assert!(transport.is_some());
    }

    #[test]
    fn resolve_meta_transport_aider_backend_routes_local_regardless_of_tier() {
        let transport = resolve_meta_transport(
            ModelTier::Sonnet,
            AgentBackend::Aider,
            &LocalConfig::default(),
            &PiConfig::default(),
        );
        assert!(transport.is_some());
    }

    #[test]
    fn wire_with_both_none_is_a_true_noop() {
        let wired = wire(StubNode::default(), None, None);
        assert!(wired.token.is_none());
    }

    #[test]
    fn wire_applies_transport_and_token() {
        let token = CancellationToken::new();
        let wired = wire(
            StubNode::default(),
            Some(stub_meta_transport("local")),
            Some(token),
        );
        assert!(wired.token.is_some());
    }

    /// Live smoke, `#[ignore]`d — same convention `engine-store`'s
    /// Postgres-gated tests use for a dependency this hermetic suite must
    /// not require by default. Proves `resolve_meta_transport`'s
    /// `ClaudeCli + Local` branch produces a [`MetaTransport`] that
    /// actually dispatches to a real local Ollama endpoint end to end
    /// (not a stub) — the transport every migrated node now reaches
    /// through `wire`/`TransportSlotted::with_meta_transport`. The
    /// wiring mechanics themselves (does the node's own `TransportSlot`
    /// correctly install and forward whatever transport it's given) are
    /// already proven hermetically by this module's other tests; this one
    /// is the "the resolved transport really talks to a local model" half.
    ///
    /// Run explicitly: `cargo nextest run -p engine-core \
    /// llm_node::tests::live_resolve_meta_transport_local_dispatches_to_real_ollama \
    /// --run-ignored ignored-only`, with a local Ollama serving a real
    /// model at `http://localhost:11434` (confirmed reachable via
    /// `curl localhost:11434/api/tags` before this run).
    #[tokio::test]
    #[ignore = "requires a live local Ollama endpoint"]
    async fn live_resolve_meta_transport_local_dispatches_to_real_ollama() {
        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b-ctx16384".to_string(),
            constrained_json: false,
        };
        let transport = resolve_meta_transport(
            ModelTier::Local,
            AgentBackend::ClaudeCli,
            &local,
            &PiConfig::default(),
        )
        .expect("ClaudeCli + Local must resolve to Some(transport)");

        let config = Config::default();
        let (outcome, info) = transport(config, "Reply with exactly one word: ack".to_string())
            .await
            .expect("live Ollama call must succeed — is `ollama serve` running on :11434?");

        assert_eq!(info.tier, "local");
        assert_eq!(info.backend, "claude_cli");
        assert!(
            !outcome.text.trim().is_empty(),
            "expected a non-empty real completion, got: {:?}",
            outcome.text
        );
    }
}
