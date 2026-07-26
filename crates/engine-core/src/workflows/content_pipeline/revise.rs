//! `ReviseNode` — the `revise`-stage correction node, emits a revised
//! `SummaryResult` for the critic loop's back-edge (EN.5.A task 9).
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. read the current summary (bound `summary_input` [`InputBinding`],
//!    falling back to `SummarizeNode`'s identity when unbound — `EN.5.E`
//!    `with_input_from`, mirroring `SelfCriticNode`'s read-preference
//!    precedent) and `SelfCriticNode`'s stored `CriticEvaluation.issues`
//!    (bound `critic_input`, falling back to `SelfCriticNode`'s identity);
//! 2. compose a revision prompt from that summary plus the critic's
//!    issues, with `Config.json_schema` set to
//!    `summarize::summary_json_schema()` so the revised reply parses into
//!    the same `SummaryResult` shape `SummarizeNode` stores;
//! 3. apply `revise`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 4. await the (injectable) transport and parse its reply via
//!    `parse_structured_or_fenced`;
//! 5. `put_result` the revised `SummaryResult` under `NODE_NAME` — this
//!    node's own identity, which `SelfCriticNode`/`DigestRenderNode` check
//!    as their read-preference fallback after their bound summary
//!    identity (`summarize.rs`'s doc comment), and forwards to
//!    `SelfCriticNode` — the loop's back-edge re-entry.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::Value;

use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::ContentPipelinePolicy;
use super::self_critic;
use super::source_router;
use super::summarize::{self, summary_json_schema, SummaryResult};

/// The `Node::name()` identity `ReviseNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto. Read by `SelfCriticNode` (and, on the
/// terminal pass, `DigestRenderNode`) as the read-preference fallback
/// after their bound summary identity.
pub const NODE_NAME: &str = "ReviseNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs CONTENT_PIPELINE workflow, \
     revise stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// Reads whichever identity the bound `summary_input` resolves to (falling
/// back to `SummarizeNode`'s identity when unbound) and returns its
/// `summary` text for the revision prompt.
fn read_summary(ctx: &TaskContext, summary_input: &InputBinding) -> Result<String, NodeError> {
    let bound = summary_input.resolve(summarize::NODE_NAME);
    let stored = get_result(ctx, bound)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: no summary stored by {bound}")))?;
    stored
        .get("summary")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: stored summary missing `summary`")))
}

/// Reads whichever identity the bound `critic_input` resolves to (falling
/// back to `SelfCriticNode`'s identity when unbound) and returns its
/// `issues` list for the revision prompt.
fn read_issues(ctx: &TaskContext, critic_input: &InputBinding) -> Result<Vec<String>, NodeError> {
    let bound = critic_input.resolve(self_critic::NODE_NAME);
    let stored = get_result(ctx, bound).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: no critic evaluation stored by {bound}"
        ))
    })?;
    let issues = stored
        .get("issues")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(issues)
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

/// Build the revision prompt from the current summary plus the critic's
/// issues.
fn build_prompt(summary: &str, issues: &[String]) -> String {
    let issues_text = if issues.is_empty() {
        "(no specific issues listed)".to_string()
    } else {
        issues
            .iter()
            .map(|issue| format!("- {issue}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You are revising a summary drafted for a content digest, per a \
         critic's issues. Apply the issues below and produce a corrected \
         summary. Respond with strict JSON matching {{summary, entities, \
         key_points}}: `summary` a concise prose summary, `entities` the \
         named people/organizations/products mentioned, `key_points` the \
         salient takeaways as short bullet strings.\n\n\
         Critic issues:\n{issues_text}\n\n\
         Original summary:\n{summary}"
    )
}

/// The `revise`-stage node that applies the critic's issues to produce a
/// corrected `SummaryResult`. Forwards to `SelfCriticNode` — the loop's
/// back-edge re-entry.
pub struct ReviseNode {
    config: Config,
    transport: Option<ModelTransport>,
    summary_input: InputBinding,
    critic_input: InputBinding,
}

impl ReviseNode {
    /// Construct with the `SummaryResult` `json_schema` set; `process`
    /// overwrites `model` per the resolved `revise`-stage tier. Both
    /// upstream bindings start unbound — `EN.5.E` task 1's `InputBinding`
    /// default, which falls back to today's identities
    /// (`SummarizeNode`/`SelfCriticNode`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(summary_json_schema()),
                ..Config::default()
            },
            transport: None,
            summary_input: InputBinding::default(),
            critic_input: InputBinding::default(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Bind the identity this node reads its current summary from. Unbound
    /// falls back to `SummarizeNode`'s identity (today's default).
    #[must_use]
    pub fn with_summary_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.summary_input = InputBinding::bound(upstream);
        self
    }

    /// Bind the identity this node reads its critic evaluation from.
    /// Unbound falls back to `SelfCriticNode`'s identity (today's
    /// default).
    #[must_use]
    pub fn with_critic_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.critic_input = InputBinding::bound(upstream);
        self
    }
}

impl Default for ReviseNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ReviseNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let summary = read_summary(&ctx, &self.summary_input)?;
        let issues = read_issues(&ctx, &self.critic_input)?;
        let policy = resolved_policy(&ctx)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.revise, &policy.local.model);
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&summary, &issues),
            policy.output_verbosity,
        );

        let mut step = ClaudeCodeStep::new(NODE_NAME, config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let revised: SummaryResult = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a revised SummaryResult from the model's \
                     reply: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&revised).map_err(|err| {
                NodeError::new(format!("failed to serialize revised SummaryResult: {err}"))
            })?,
        );

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

    fn revised_summary_json() -> serde_json::Value {
        json!({
            "summary": "A corrected, more accurate summary.",
            "entities": ["Acme Corp"],
            "key_points": ["Corrected point one"],
        })
    }

    fn ctx_with_summary_and_critic(
        summary_node: &str,
        summary: &str,
        critic_node: &str,
        issues: Vec<&str>,
    ) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            summary_node,
            json!({ "summary": summary, "entities": [], "key_points": [] }),
        );
        put_result(
            &mut ctx,
            critic_node,
            json!({
                "verdict": "revise",
                "confidence": 0.4,
                "issues": issues,
                "iteration": 0,
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
                    text: serde_json::to_string(&revised_summary_json()).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 90,
                        output_tokens: 40,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 90,
                            output_tokens: 40,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    structured_output: structured,
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    #[tokio::test]
    async fn process_produces_summary_result_shape() {
        let node = ReviseNode::new().with_transport(stub_transport(Some(revised_summary_json())));
        let ctx = ctx_with_summary_and_critic(
            summarize::NODE_NAME,
            "The original summary.",
            self_critic::NODE_NAME,
            vec!["missing citation"],
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let revised: SummaryResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid SummaryResult");
        assert_eq!(revised.summary, "A corrected, more accurate summary.");
        assert_eq!(revised.entities, vec!["Acme Corp".to_string()]);
        assert_eq!(revised.key_points.len(), 1);

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(90));
        assert_eq!(usage.output_tokens, Some(40));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = ReviseNode::new().with_transport(stub_transport(None));
        let ctx = ctx_with_summary_and_critic(
            summarize::NODE_NAME,
            "The original summary.",
            self_critic::NODE_NAME,
            vec![],
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let revised: SummaryResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid SummaryResult");
        assert_eq!(revised.entities.len(), 1);
    }

    #[tokio::test]
    async fn process_reads_upstream_summary_and_issues_into_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&revised_summary_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(revised_summary_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new().with_transport(transport);
        let ctx = ctx_with_summary_and_critic(
            summarize::NODE_NAME,
            "The original, flawed summary.",
            self_critic::NODE_NAME,
            vec!["Composite math is off."],
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Composite math is off."));
        assert!(prompt.contains("The original, flawed summary."));
    }

    #[tokio::test]
    async fn process_reads_bound_upstreams_via_with_input_from() {
        // EN.5.E: with the node built `with_summary_input_from`/
        // `with_critic_input_from`, it reads its summary/issues off the
        // *bound* identities rather than the default `SummarizeNode`/
        // `SelfCriticNode`.
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&revised_summary_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(revised_summary_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new()
            .with_transport(transport)
            .with_summary_input_from("CustomSummaryNode")
            .with_critic_input_from("CustomCriticNode");
        let ctx = ctx_with_summary_and_critic(
            "CustomSummaryNode",
            "Bound summary text.",
            "CustomCriticNode",
            vec!["Bound issue surfaced."],
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Bound issue surfaced."));
        assert!(prompt.contains("Bound summary text."));
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let policy = ContentPipelinePolicy {
            output_verbosity: OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: ModelTiers {
                revise: ModelTier::Opus,
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
                    text: serde_json::to_string(&revised_summary_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(revised_summary_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new().with_transport(transport);
        let mut ctx = ctx_with_summary_and_critic(
            summarize::NODE_NAME,
            "The original summary.",
            self_critic::NODE_NAME,
            vec![],
        );
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
        let node = ReviseNode::new().with_transport(stub_transport(Some(revised_summary_json())));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            self_critic::NODE_NAME,
            json!({ "verdict": "revise", "confidence": 0.4, "issues": [], "iteration": 0 }),
        );
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
    async fn process_errors_when_no_critic_evaluation_is_stored() {
        let node = ReviseNode::new().with_transport(stub_transport(Some(revised_summary_json())));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({ "summary": "A summary.", "entities": [], "key_points": [] }),
        );
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({
                "envelope": { "envelope_id": "env-1" },
                "policy": ContentPipelinePolicy::default(),
            }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no critic evaluation stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_policy_is_stored() {
        let node = ReviseNode::new().with_transport(stub_transport(Some(revised_summary_json())));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({ "summary": "A summary.", "entities": [], "key_points": [] }),
        );
        put_result(
            &mut ctx,
            self_critic::NODE_NAME,
            json!({ "verdict": "revise", "confidence": 0.4, "issues": [], "iteration": 0 }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no policy stored by"));
    }
}
