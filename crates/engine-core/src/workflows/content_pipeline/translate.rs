//! `TranslateSkipRouterNode` + `TranslateNode` — the optional PT-BR
//! translation branch (EN.5.A task 10), per
//! `planning/EN.5.A-content-pipeline/architecture.md` §1/§3.2.
//!
//! ## `TranslateSkipRouterNode`
//!
//! A deterministic `Node` + `Router` reading `ContentPipelineInput.translate`
//! straight off `ctx.event` (mirrors `CriticRouterNode`'s pattern: `route`
//! takes `&TaskContext` and cannot mutate it, so there is nothing for
//! `process` to stage — the flag is already on the event `SourceRouterNode`
//! parsed at the top of the run). `route` returns `TranslateNode`'s identity
//! when `translate` is `true`, `DigestRenderNode`'s identity (a string
//! literal, not an import — `digest_render.rs` is still a task-11 scaffold
//! at this task's authoring time, mirroring `critic_router.rs`'s
//! `TRANSLATE_SKIP_ROUTER_NODE_NAME` convention) otherwise.
//!
//! ## `TranslateNode`
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`, stage `"translate"`.
//! On `process`:
//! 1. read whichever of `SummarizeNode`/`ReviseNode` most recently stored a
//!    `SummaryResult` (bound `summary_input` [`InputBinding`], falling back
//!    to `SummarizeNode`'s identity when unbound, then `ReviseNode`'s —
//!    mirrors `self_critic::read_summary`'s read-preference precedent);
//! 2. read `target_lang` off `ContentPipelineInput` (default `pt-BR` —
//!    `schema::default_target_lang`);
//! 3. compose a translation prompt from that summary plus `target_lang`,
//!    with `Config.json_schema` set to `translation_json_schema()`;
//! 4. apply `translate`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 5. await the (injectable) transport and parse its reply into a
//!    [`TranslationResult`] via `parse_structured_or_fenced`;
//! 6. `put_result` the `TranslationResult` under `NODE_NAME` — the shape
//!    `DigestRenderNode` reads to populate `translated_markdown` — and
//!    forwards to `DigestRenderNode`.
//!
//! `translate=false` never invokes this node at all (the skip router routes
//! straight to `DigestRenderNode`), so the translate transport is provably
//! never called on that path.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::routing::Router;
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, ModelTransport, TransportSlot,
};

use super::policy::ContentPipelinePolicy;
use super::revise;
use super::schema::ContentPipelineInput;
use super::source_router;
use super::summarize;

/// The `Node::name()` identity `TranslateSkipRouterNode` is registered
/// under.
pub const SKIP_ROUTER_NODE_NAME: &str = "TranslateSkipRouterNode";

/// The `Node::name()` identity `TranslateNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto. Read by `DigestRenderNode`.
pub const NODE_NAME: &str = "TranslateNode";

/// The identity `TranslateSkipRouterNode` routes to when
/// `ContentPipelineInput.translate` is `true`. Not a forward-declared
/// import from this same module's `TranslateNode` for symmetry with
/// [`DIGEST_RENDER_NODE_NAME`]'s doc-comment convention below — both are
/// `Node::name()` string identities, and this one happens to live in this
/// module too.
const TRANSLATE_NODE_NAME: &str = NODE_NAME;

/// The identity `TranslateSkipRouterNode` routes to when
/// `ContentPipelineInput.translate` is `false`, and `TranslateNode` always
/// forwards to on completion. A string literal, not an import —
/// `digest_render.rs` is still a task-11 scaffold at this task's authoring
/// time, mirroring `critic_router.rs`'s `TRANSLATE_SKIP_ROUTER_NODE_NAME`
/// convention (avoids a forward dependency on an unwritten module).
const DIGEST_RENDER_NODE_NAME: &str = "DigestRenderNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs CONTENT_PIPELINE workflow, \
     translate stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// What `TranslateNode` stores under its own identity — the shape
/// `DigestRenderNode` reads to populate `ContentPipelineOutput
/// ::translated_markdown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationResult {
    pub translated_markdown: String,
}

/// The `Config.json_schema` shape `TranslateNode` prompts for.
pub fn translation_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "translated_markdown": { "type": "string" },
        },
        "required": ["translated_markdown"],
    })
}

/// Parse `ContentPipelineInput` off `ctx.event` — the top-of-run event
/// `SourceRouterNode` already validated. Both `TranslateSkipRouterNode`'s
/// `route` and `TranslateNode`'s `process` need `translate`/`target_lang`
/// off it independently (a `Router::route` cannot read anything `process`
/// would otherwise stage, since `route` takes `&TaskContext` and cannot
/// mutate it — `routing.rs`'s contract), so both re-parse it directly
/// rather than depending on a stored intermediate.
fn parsed_input(ctx: &TaskContext) -> Option<ContentPipelineInput> {
    serde_json::from_value(ctx.event.clone()).ok()
}

/// The optional-translation branch's guard: deterministic `Node` (a pure
/// pass-through — there is nothing to stage) + `Router` reading
/// `ContentPipelineInput.translate` off `ctx.event`.
pub struct TranslateSkipRouterNode;

#[async_trait::async_trait]
impl Node for TranslateSkipRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        SKIP_ROUTER_NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TranslateSkipRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let input = parsed_input(ctx)?;
        Some(if input.translate {
            TRANSLATE_NODE_NAME.to_string()
        } else {
            DIGEST_RENDER_NODE_NAME.to_string()
        })
    }
}

/// Reads whichever of `SummarizeNode`/`ReviseNode` most recently stored a
/// `SummaryResult` (tries `ReviseNode` first, since it overwrites
/// `SummarizeNode`'s output on the loop's back-edge, falling back to the
/// bound `summary_input` identity — `SummarizeNode`'s by default when
/// unbound — when no revise pass ran) and returns its `summary` text for
/// the translation prompt.
fn read_summary(ctx: &TaskContext, summary_input: &InputBinding) -> Result<String, NodeError> {
    let bound = summary_input.resolve(summarize::NODE_NAME);
    let stored = get_result(ctx, revise::NODE_NAME)
        .or_else(|| get_result(ctx, bound))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no summary stored by {bound} or {}",
                revise::NODE_NAME
            ))
        })?;
    stored
        .get("summary")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: stored summary missing `summary`")))
}

/// Reads the `ContentPipelinePolicy` `SourceRouterNode` resolved and stored
/// inline under its own identity (task 4) — a channel envelope has no
/// worktree to resolve `RESOLVED_POLICY_IDENTITY` against, so this stage
/// does not use `crate::policy::resolved_policy_strict`.
fn resolved_policy(ctx: &TaskContext) -> Result<ContentPipelinePolicy, NodeError> {
    let stored = get_result(ctx, source_router::NODE_NAME).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: no policy stored by {}",
            source_router::NODE_NAME
        ))
    })?;
    let policy = stored
        .get("policy")
        .cloned()
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: policy field missing")))?;
    serde_json::from_value(policy)
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid stored policy: {err}")))
}

/// Read `target_lang` off `ctx.event`'s `ContentPipelineInput`, defaulting
/// to `pt-BR` when the event fails to parse (should not happen this far
/// into the run — `SourceRouterNode` already validated it — but `process`
/// fails closed to a safe default rather than erroring the whole pipeline
/// over a re-parse hiccup on an already-validated event).
fn target_lang(ctx: &TaskContext) -> String {
    parsed_input(ctx)
        .map(|input| input.target_lang)
        .unwrap_or_else(|| "pt-BR".to_string())
}

/// Build the translation prompt from the current summary and target
/// language.
fn build_prompt(summary: &str, target_lang: &str) -> String {
    format!(
        "You are translating a content digest summary into {target_lang}. \
         Preserve meaning, named entities, and markdown structure. Respond \
         with strict JSON matching {{\"translated_markdown\": string}} — \
         the full translation as markdown.\n\n\
         Summary:\n{summary}"
    )
}

/// The `translate`-stage model node. Forwards to `DigestRenderNode`.
pub struct TranslateNode {
    config: Config,
    transport: TransportSlot,
    summary_input: InputBinding,
}

impl TranslateNode {
    /// Construct with the translation `json_schema` set; `process`
    /// overwrites `model` per the resolved `translate`-stage tier. The
    /// upstream binding starts unbound — `EN.5.E` task 1's `InputBinding`
    /// default, which falls back to today's identity (`SummarizeNode`,
    /// then `ReviseNode`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(translation_json_schema()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            summary_input: InputBinding::default(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware [`MetaTransport`] that
    /// reports the [`crate::nodes::claude_code_step::TransportInfo`] of
    /// whichever call actually executed (e.g. local vs. cloud fallback),
    /// taking precedence over a plain transport set via
    /// [`Self::with_transport`].
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }

    /// Bind the identity this node reads its current summary from. Unbound
    /// falls back to `SummarizeNode`'s identity, then `ReviseNode`'s
    /// (today's default).
    #[must_use]
    pub fn with_summary_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.summary_input = InputBinding::bound(upstream);
        self
    }
}

impl Default for TranslateNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for TranslateNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let summary = read_summary(&ctx, &self.summary_input)?;
        let policy = resolved_policy(&ctx)?;
        let target_lang = target_lang(&ctx);

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.translate,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&summary, &target_lang),
            policy.output_verbosity,
        );

        let step = self
            .transport
            .apply(ClaudeCodeStep::new(NODE_NAME, config, prompt));

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        // `put_result` below replaces this node's whole `ctx.nodes` entry,
        // which would otherwise silently drop the `"transport"` stamp
        // `ClaudeCodeStep::process` just wrote — the exact tier-telemetry
        // `RunTelemetry`/`observed_model_tiers` (`policy/telemetry.rs`)
        // reads back out by this same node name.
        let transport_stamp = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("transport"))
            .cloned();

        let translation: TranslationResult = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a TranslationResult from the model's reply: \
                     {err}"
                ))
            })?;

        let mut result = serde_json::to_value(&translation).map_err(|err| {
            NodeError::new(format!("failed to serialize TranslationResult: {err}"))
        })?;
        if let Some(transport) = transport_stamp {
            result["transport"] = transport;
        }
        put_result(&mut ctx, NODE_NAME, result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
    use claude_code_rs::Outcome;
    use futures::FutureExt;
    use serde_json::json;

    use super::super::policy::{ContentPipelineModelTiers, ModelTier, OutputVerbosity};
    use super::*;

    fn translated_json() -> serde_json::Value {
        json!({ "translated_markdown": "# Resumo traduzido\n\nConteudo em portugues." })
    }

    fn event_with_translate(translate: bool) -> serde_json::Value {
        json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" }
            },
            "translate": translate,
        })
    }

    fn ctx_with_summary(event: serde_json::Value) -> TaskContext {
        let mut ctx = TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({
                "summary": "A summary in English.",
                "entities": [],
                "key_points": [],
            }),
        );
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": ContentPipelinePolicy::default(),
            }),
        );
        ctx
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&translated_json()).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 80,
                        output_tokens: 30,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 80,
                            output_tokens: 30,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    session_id: None,
                    structured_output: structured,
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    // --- TranslateSkipRouterNode -------------------------------------

    #[test]
    fn route_goes_to_translate_node_when_translate_is_true() {
        let router = TranslateSkipRouterNode;
        let ctx = ctx_with_summary(event_with_translate(true));

        assert_eq!(router.route(&ctx), Some("TranslateNode".to_string()));
    }

    #[test]
    fn route_goes_to_digest_render_node_when_translate_is_false() {
        let router = TranslateSkipRouterNode;
        let ctx = ctx_with_summary(event_with_translate(false));

        assert_eq!(router.route(&ctx), Some("DigestRenderNode".to_string()));
    }

    #[test]
    fn route_defaults_to_digest_render_node_when_translate_is_absent() {
        let router = TranslateSkipRouterNode;
        let mut event = event_with_translate(false);
        event.as_object_mut().unwrap().remove("translate");
        let ctx = ctx_with_summary(event);

        assert_eq!(router.route(&ctx), Some("DigestRenderNode".to_string()));
    }

    #[tokio::test]
    async fn skip_router_process_is_a_pure_passthrough() {
        let router = TranslateSkipRouterNode;
        let ctx = ctx_with_summary(event_with_translate(true));
        let before = ctx.nodes.clone();

        let after = router.process(ctx).await.expect("process should succeed");

        assert_eq!(after.nodes, before);
    }

    #[test]
    fn skip_router_as_router_is_some() {
        let router = TranslateSkipRouterNode;
        assert!(router.as_router().is_some());
    }

    // --- TranslateNode -------------------------------------------------

    #[tokio::test]
    async fn process_populates_translated_markdown_from_structured_reply() {
        let node = TranslateNode::new().with_transport(stub_transport(Some(translated_json())));
        let ctx = ctx_with_summary(event_with_translate(true));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let translation: TranslationResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid TranslationResult");
        assert!(translation.translated_markdown.contains("traduzido"));

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(80));
        assert_eq!(usage.output_tokens, Some(30));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = TranslateNode::new().with_transport(stub_transport(None));
        let ctx = ctx_with_summary(event_with_translate(true));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let translation: TranslationResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid TranslationResult");
        assert!(translation.translated_markdown.contains("Resumo"));
    }

    #[tokio::test]
    async fn process_defaults_target_lang_to_pt_br_and_carries_it_in_the_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&translated_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(translated_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = TranslateNode::new().with_transport(transport);
        let ctx = ctx_with_summary(event_with_translate(true));

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("pt-BR"));
    }

    #[tokio::test]
    async fn process_reads_bound_summary_input_when_set() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&translated_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(translated_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = TranslateNode::new()
            .with_transport(transport)
            .with_summary_input_from("CustomSummaryNode");
        let mut ctx = ctx_with_summary(event_with_translate(true));
        put_result(
            &mut ctx,
            "CustomSummaryNode",
            json!({ "summary": "Bound summary text.", "entities": [], "key_points": [] }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Bound summary text."));
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let policy = ContentPipelinePolicy {
            output_verbosity: OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: ContentPipelineModelTiers {
                translate: ModelTier::Opus,
                ..ContentPipelinePolicy::default().model_tiers
            },
            ..ContentPipelinePolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&translated_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(translated_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = TranslateNode::new().with_transport(transport);
        let mut ctx = ctx_with_summary(event_with_translate(true));
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": policy,
            }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));
        assert!(prompt.contains("Be terse"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_summary_is_stored() {
        let node = TranslateNode::new().with_transport(stub_transport(Some(translated_json())));
        let mut ctx = TaskContext {
            event: event_with_translate(true),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": ContentPipelinePolicy::default(),
            }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no summary stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_policy_is_stored() {
        let node = TranslateNode::new().with_transport(stub_transport(Some(translated_json())));
        let mut ctx = TaskContext {
            event: event_with_translate(true),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({ "summary": "A summary.", "entities": [], "key_points": [] }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no policy stored by"));
    }

    #[tokio::test]
    async fn process_never_invokes_translate_transport_when_translate_is_false() {
        // Simulates the graph-level guarantee: `TranslateSkipRouterNode`
        // routes straight to `DigestRenderNode` when `translate=false`, so
        // `TranslateNode::process` is never called at all on that path.
        // This test asserts the routing half of that guarantee directly;
        // the transport-never-called half is structural (no call site
        // exists on the false branch), covered by the graph wiring test in
        // `graph.rs` once task 12 lands.
        let router = TranslateSkipRouterNode;
        let ctx = ctx_with_summary(event_with_translate(false));

        let route = router.route(&ctx);

        assert_eq!(route, Some("DigestRenderNode".to_string()));
    }
}
