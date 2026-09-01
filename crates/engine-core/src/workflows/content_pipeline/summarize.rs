//! `SummarizeNode` — the `summarize`-stage model node (EN.5.A task 6).
//!
//! Patterned on `proposal_generator::writer::ProposalWriterNode`: a
//! non-terminal cloud-default model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. read whichever of `FetchArticleNode` / `FetchTranscriptNode` /
//!    `NormalizeChannelContentNode` ran (all three converge on the same
//!    `{title, text, source_ref}` shape — task 5) and the `policy` snapshot
//!    `SourceRouterNode` stamped under its own identity (a channel envelope
//!    has no `RESOLVED_POLICY_IDENTITY` stamp — task 4 stores the resolved
//!    `ContentPipelinePolicy` inline instead);
//! 2. compose a summarization prompt from that content plus
//!    `Config.json_schema` for the `{summary, entities, key_points}` shape;
//! 3. apply `summarize`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 4. await the (injectable) transport and parse its reply via
//!    `parse_structured_or_fenced`;
//! 5. `put_result` the parsed summary and forward to `SelfCriticNode`.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, ModelTransport, TransportSlot,
};

use super::policy::ContentPipelinePolicy;
use super::{fetch_article, fetch_transcript, normalize_channel_content, source_router};

/// The `Node::name()` identity `SummarizeNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto.
pub const NODE_NAME: &str = "SummarizeNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs CONTENT_PIPELINE workflow, \
     summarize stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// What `SummarizeNode` (and, on the back-edge, `ReviseNode`) stores under
/// its own identity — the shape `SelfCriticNode`/`DigestRenderNode` read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryResult {
    pub summary: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub key_points: Vec<String>,
}

/// The `Config.json_schema` shape `SummarizeNode`/`ReviseNode` prompt for.
pub fn summary_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "entities": { "type": "array", "items": { "type": "string" } },
            "key_points": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["summary", "entities", "key_points"],
    })
}

/// The normalized `{title, text, source_ref}` content shape every
/// task-5 node converges on.
struct ConvergedContent {
    title: Option<String>,
    text: String,
    source_ref: String,
}

/// Reads whichever of the three task-5 converge nodes ran (exactly one
/// does, depending on `SourceRouterNode::route`) and parses its stored
/// `{title, text, source_ref}` shape.
fn read_converged_content(ctx: &TaskContext) -> Result<ConvergedContent, NodeError> {
    let stored = get_result(ctx, fetch_article::NODE_NAME)
        .or_else(|| get_result(ctx, fetch_transcript::NODE_NAME))
        .or_else(|| get_result(ctx, normalize_channel_content::NODE_NAME))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no content stored by {}, {}, or {}",
                fetch_article::NODE_NAME,
                fetch_transcript::NODE_NAME,
                normalize_channel_content::NODE_NAME
            ))
        })?;

    let text = stored
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: content missing `text`")))?
        .to_string();
    let source_ref = stored
        .get("source_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: content missing `source_ref`")))?
        .to_string();
    let title = stored
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(ConvergedContent {
        title,
        text,
        source_ref,
    })
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

/// Build the summarization prompt from the converged content.
fn build_prompt(content: &ConvergedContent) -> String {
    let title = content.title.as_deref().unwrap_or("(untitled)");
    format!(
        "You are summarizing content ingested from \"{source_ref}\" for a \
         digest. Title: {title}\n\n\
         Content:\n{text}\n\n\
         Respond with strict JSON matching {{summary, entities, key_points}}: \
         `summary` a concise prose summary, `entities` the named \
         people/organizations/products mentioned, `key_points` the salient \
         takeaways as short bullet strings.",
        source_ref = content.source_ref,
        text = content.text,
    )
}

/// The `summarize`-stage model node. Forwards to `SelfCriticNode`.
pub struct SummarizeNode {
    config: Config,
    transport: TransportSlot,
}

impl SummarizeNode {
    /// Construct with the summary `json_schema` set; `process` overwrites
    /// `model` per the resolved `summarize`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(summary_json_schema()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
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
}

impl Default for SummarizeNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SummarizeNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let content = read_converged_content(&ctx)?;
        let policy = resolved_policy(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.summarize,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&content),
            policy.output_verbosity,
        );

        let step = self
            .transport
            .apply(ClaudeCodeStep::new(NODE_NAME, config, prompt));

        let mut ctx = step.process(ctx).await?;

        let response_content = ctx
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

        let summary: SummaryResult = parse_structured_or_fenced(&ctx, NODE_NAME, &response_content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a SummaryResult from the model's reply: {err}"
                ))
            })?;

        let mut result = serde_json::to_value(&summary)
            .map_err(|err| NodeError::new(format!("failed to serialize SummaryResult: {err}")))?;
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

    use super::super::policy::{ModelTier, ModelTiers, OutputVerbosity};
    use super::*;

    fn stub_summary_json() -> serde_json::Value {
        json!({
            "summary": "A concise summary of the article.",
            "entities": ["Acme Corp"],
            "key_points": ["Point one", "Point two"],
        })
    }

    fn ctx_with_content(node_name: &str, content: serde_json::Value) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, node_name, content);
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

    fn article_content() -> serde_json::Value {
        json!({
            "title": "Example Title",
            "text": "Example body text about Acme Corp.",
            "source_ref": "https://example.com/a",
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_summary_json()).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 100,
                        output_tokens: 50,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 100,
                            output_tokens: 50,
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

    /// `EN.ticket.wire-meta-transport-telemetry` task 3: a
    /// `with_meta_transport` override on `SummarizeNode` must stamp the
    /// *actual* transport tier onto
    /// `ctx.nodes["SummarizeNode"]["transport"]["tier"]` — `"local"` on a
    /// stubbed local success, not a generic `"cloud"` regardless of what
    /// ran (the bug this ticket fixes).
    #[tokio::test]
    async fn summarize_meta_transport_stamps_local_tier_on_stubbed_local_success() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::policy::tier::LocalConfig;

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost = std::sync::Arc::new(|_url, _body| {
            Box::pin(async {
                Ok(json!({
                    "choices": [{ "message": {
                        "content": stub_summary_json().to_string()
                    } }],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                }))
            })
        });
        let cloud_fallback: ModelTransport = std::sync::Arc::new(|_config, _prompt| {
            Box::pin(async { panic!("cloud fallback must not be called when local succeeds") })
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = SummarizeNode::new().with_meta_transport(meta_transport);
        let ctx = ctx_with_content(fetch_article::NODE_NAME, article_content());

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes[NODE_NAME]["transport"]["tier"], "local");
        assert_eq!(
            out.nodes[NODE_NAME]["transport"]["endpoint"],
            "http://localhost:11434"
        );
    }

    /// Same seam, but the local endpoint fails — the resulting telemetry
    /// must show `"cloud"` (what actually ran, via the fallback), not the
    /// `"local"` tier the resolved policy intended.
    #[tokio::test]
    async fn summarize_meta_transport_stamps_cloud_tier_on_local_failure_fallback() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::policy::tier::LocalConfig;

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost = std::sync::Arc::new(|_url, _body| {
            Box::pin(async { Err("connection refused".to_string()) })
        });
        let cloud_fallback: ModelTransport = stub_transport(Some(stub_summary_json()));
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = SummarizeNode::new().with_meta_transport(meta_transport);
        let ctx = ctx_with_content(fetch_article::NODE_NAME, article_content());

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(
            out.nodes[NODE_NAME]["transport"]["tier"], "cloud",
            "a down local endpoint must stamp the cloud fallback's actual tier, \
             not the resolved policy's intended `local` tier"
        );
        assert!(out.nodes[NODE_NAME]["transport"]["endpoint"].is_null());
    }

    #[tokio::test]
    async fn process_populates_summary_from_structured_reply() {
        let node = SummarizeNode::new().with_transport(stub_transport(Some(stub_summary_json())));
        let ctx = ctx_with_content(fetch_article::NODE_NAME, article_content());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let summary: SummaryResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid SummaryResult");
        assert_eq!(summary.summary, "A concise summary of the article.");
        assert_eq!(summary.entities, vec!["Acme Corp".to_string()]);
        assert_eq!(summary.key_points.len(), 2);

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = SummarizeNode::new().with_transport(stub_transport(None));
        let ctx = ctx_with_content(fetch_transcript::NODE_NAME, article_content());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let summary: SummaryResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid SummaryResult");
        assert_eq!(summary.entities.len(), 1);
    }

    #[tokio::test]
    async fn process_reads_normalize_channel_content_when_present() {
        let node = SummarizeNode::new().with_transport(stub_transport(Some(stub_summary_json())));
        let ctx = ctx_with_content(normalize_channel_content::NODE_NAME, article_content());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let summary: SummaryResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid SummaryResult");
        assert_eq!(summary.summary, "A concise summary of the article.");
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let policy = ContentPipelinePolicy {
            output_verbosity: OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: ModelTiers {
                summarize: ModelTier::Opus,
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
                    text: serde_json::to_string(&stub_summary_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(stub_summary_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = SummarizeNode::new().with_transport(transport);
        let mut ctx = ctx_with_content(fetch_article::NODE_NAME, article_content());
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
    async fn process_errors_when_no_upstream_content_is_stored() {
        let node = SummarizeNode::new().with_transport(stub_transport(Some(stub_summary_json())));
        let mut ctx = TaskContext {
            event: json!({}),
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
        assert!(err.message.contains("no content stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_policy_is_stored() {
        let node = SummarizeNode::new().with_transport(stub_transport(Some(stub_summary_json())));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, fetch_article::NODE_NAME, article_content());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no policy stored by"));
    }
}
