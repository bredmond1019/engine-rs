//! `ClaudeCodeStep` — a reusable `Node` that spawns a Claude Code session via
//! `claude_code_rs::execute` and maps its `Outcome` into `NodeRun`/`TaskContext`
//! (`EN.2.A`).
//!
//! Per `D4-claude-code-transport-choice`, this module owns none of the
//! subprocess/argv/parse logic — that surface belongs entirely to
//! `core/claude-code-rs`. This node's only job is: build a prompt from the
//! `TaskContext`, await the SDK's `execute()`, and stamp the result onto the
//! node's own `TaskContext::nodes` entry and `NodeRun.usage`.

use std::fmt;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRun, NodeRunStatus, TaskContext, Usage};
use futures::future::BoxFuture;

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};

/// Stand-in for `Usage::model` when the SDK reports no model.
///
/// The CLI names models only as keys in its `modelUsage` map, which is empty on
/// the error envelope — so `Outcome::primary_model()` is an `Option`. The
/// orchestrator data contract (v1.0.1 §6) requires `usage.model` to be a
/// non-null string, so absence has to be spelled *somehow*; it is spelled here,
/// at the seam, rather than by loosening the contract type for a vendor quirk.
const UNKNOWN_MODEL: &str = "unknown";

/// The injectable transport signature: takes an owned `Config` + prompt and
/// returns a boxed future resolving to a `claude_code_rs::Result<Outcome>`.
/// Defaults to `claude_code_rs::execute`; tests substitute a stub via
/// [`ClaudeCodeStep::with_transport`] so the gated suite never spawns a real
/// subprocess.
type Transport = Arc<
    dyn Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> + Send + Sync,
>;

/// Where a `ClaudeCodeStep`'s prompt text comes from: either a fixed string
/// decided at construction, or a closure built fresh from the live
/// `TaskContext` on each `process` call.
#[derive(Clone)]
enum PromptSource {
    Fixed(String),
    Builder(Arc<dyn Fn(&TaskContext) -> String + Send + Sync>),
}

impl fmt::Debug for PromptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromptSource::Fixed(prompt) => f.debug_tuple("Fixed").field(prompt).finish(),
            PromptSource::Builder(_) => f.debug_tuple("Builder").field(&"<closure>").finish(),
        }
    }
}

/// A `Node` that runs a single Claude Code session (via `core/claude-code-rs`)
/// and maps its result into `TaskContext`.
///
/// Constructed with an instance name (its `Node::name()` identity — Phase 3
/// registers multiple instances under distinct identities for
/// implement/test/triage/review), a `claude_code_rs::Config`, and a prompt
/// source (fixed string or a builder closure over `&TaskContext`).
#[derive(Clone)]
pub struct ClaudeCodeStep {
    name: String,
    config: Config,
    prompt: PromptSource,
    transport: Transport,
    cancellation_token: Option<CancellationToken>,
}

impl fmt::Debug for ClaudeCodeStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCodeStep")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("prompt", &self.prompt)
            .finish_non_exhaustive()
    }
}

/// The default transport: delegates straight to `claude_code_rs::execute`,
/// owning its own clones of `config`/`prompt` so the returned future is
/// `'static`.
fn default_transport(
    config: Config,
    prompt: String,
) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> {
    Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
}

impl ClaudeCodeStep {
    /// Construct a step with a fixed prompt string.
    pub fn new(name: impl Into<String>, config: Config, prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config,
            prompt: PromptSource::Fixed(prompt.into()),
            transport: Arc::new(default_transport),
            cancellation_token: None,
        }
    }

    /// Construct a step whose prompt is built fresh from the live
    /// `TaskContext` on each `process` call.
    pub fn with_prompt_builder(
        name: impl Into<String>,
        config: Config,
        builder: impl Fn(&TaskContext) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            config,
            prompt: PromptSource::Builder(Arc::new(builder)),
            transport: Arc::new(default_transport),
            cancellation_token: None,
        }
    }

    /// Override the transport used to run the Claude Code session. Tests use
    /// this to stub a real subprocess call with a canned `Outcome`/`Error`, so
    /// the gated `cargo test` suite stays hermetic (no real `claude` spawn).
    #[must_use]
    pub fn with_transport(
        mut self,
        transport: impl Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.transport = Arc::new(transport);
        self
    }

    /// Attach a `CancellationToken` (EN.2.B task 1) that is raced against the
    /// awaited transport future in `process` (task 4). Absent a token,
    /// `process` behaves exactly as before — the transport future is simply
    /// awaited to completion.
    ///
    /// A cancel win drops the in-flight transport future (per D4 the SDK owns
    /// kill-on-drop — this is not subprocess signalling) and returns `Ok(ctx)`
    /// with the context untouched. `process` never stamps its own `NodeRun`
    /// (that envelope is framework-owned, see `crate::node::Node`), so this
    /// deliberately does *not* return a `NodeError` for a cancel: task 3's
    /// `Workflow::run_with` re-checks the token at the very next node
    /// boundary and stamps the run cancelled there — an `Err` here would
    /// instead be read as FAILED by `workflow::node_context`.
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }
}

#[async_trait::async_trait]
impl Node for ClaudeCodeStep {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let prompt = match &self.prompt {
            PromptSource::Fixed(prompt) => prompt.clone(),
            PromptSource::Builder(builder) => builder(&ctx),
        };

        let transport_fut = (self.transport)(self.config.clone(), prompt);

        let outcome = match &self.cancellation_token {
            Some(token) => {
                tokio::select! {
                    // A cancel win drops `transport_fut` (tokio::select!
                    // drops the losing branch's future) rather than awaiting
                    // it to completion. Returning `Ok(ctx)` unchanged here —
                    // not `Err` — is deliberate: see
                    // `with_cancellation_token`'s doc comment.
                    _ = token.cancelled() => {
                        return Ok(ctx);
                    }
                    result = transport_fut => {
                        result.map_err(|err| NodeError::new(err.to_string()))?
                    }
                }
            }
            None => transport_fut
                .await
                .map_err(|err| NodeError::new(err.to_string()))?,
        };

        // `primary_model` is a heuristic over the SDK's `modelUsage` map and is
        // `None` when no model ran. `engine_contract::Usage::model` is a required
        // `String` (the orchestrator data contract's shape, v1.0.1) — so the
        // fallback belongs here, at the seam, rather than in the contract type.
        let model = outcome.primary_model().unwrap_or(UNKNOWN_MODEL).to_string();

        let output = serde_json::json!({
            "content": outcome.text,
            "cost_usd": outcome.cost_usd,
            "model": model,
        });
        ctx.nodes.insert(self.name.clone(), output);

        let usage = Usage {
            input_tokens: Some(outcome.usage.input_tokens),
            output_tokens: Some(outcome.usage.output_tokens),
            model,
        };
        ctx.node_runs
            .entry(self.name.clone())
            .and_modify(|run| run.usage = Some(usage.clone()))
            .or_insert_with(|| NodeRun {
                status: NodeRunStatus::Running,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(usage),
            });

        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_outcome() -> Outcome {
        stub_outcome_with_models(&[("claude-sonnet-4-5", 0.01, 34)])
    }

    /// Build an `Outcome` whose `modelUsage` carries the given
    /// `(name, cost_usd, output_tokens)` entries.
    fn stub_outcome_with_models(models: &[(&str, f64, u64)]) -> Outcome {
        Outcome {
            cost_usd: 0.01,
            usage: SdkUsage {
                input_tokens: 12,
                output_tokens: 34,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: models
                .iter()
                .map(|(name, cost_usd, output_tokens)| {
                    (
                        (*name).to_string(),
                        SdkModelUsage {
                            input_tokens: 12,
                            output_tokens: *output_tokens,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: *cost_usd,
                        },
                    )
                })
                .collect(),
            text: "ok".to_string(),
            is_error: false,
            api_error_status: None,
        }
    }

    #[tokio::test]
    async fn success_maps_output_and_usage() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| Box::pin(async { Ok(stub_outcome()) }));

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx
            .nodes
            .get("ClaudeCodeStep")
            .expect("output present under node identity");
        assert_eq!(output["content"], "ok");
        assert_eq!(output["model"], "claude-sonnet-4-5");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry present");
        let usage = run.usage.as_ref().expect("usage stamped");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));
        assert_eq!(usage.model, "claude-sonnet-4-5");
    }

    /// The SDK reports no model when `modelUsage` is empty. The contract's
    /// `usage.model` is a required `String`, so the seam must supply a stand-in
    /// rather than panic or drop the `NodeRun`.
    #[tokio::test]
    async fn absent_model_usage_falls_back_to_unknown_model() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async { Ok(stub_outcome_with_models(&[])) })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("an empty modelUsage must not fail the node");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry");
        assert_eq!(run.usage.as_ref().expect("usage stamped").model, "unknown");
        assert_eq!(ctx.nodes["ClaudeCodeStep"]["model"], "unknown");
    }

    /// A single call can bill several models. Attribution follows the SDK's
    /// cost-ranked heuristic, so the expensive model that did the work wins over
    /// a chatty cheap one.
    #[tokio::test]
    async fn multi_model_usage_attributes_to_the_primary_model() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async {
                    Ok(stub_outcome_with_models(&[
                        ("claude-haiku-4-5", 0.001, 900),
                        ("claude-opus-4-8", 0.42, 12),
                    ]))
                })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry");
        assert_eq!(
            run.usage.as_ref().expect("usage stamped").model,
            "claude-opus-4-8"
        );
    }

    #[tokio::test]
    async fn sdk_error_maps_to_node_error() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async { Err(claude_code_rs::Error::Timeout) })
            });

        let err = step
            .process(empty_context())
            .await
            .expect_err("process should surface the SDK error");

        assert_eq!(err.message, claude_code_rs::Error::Timeout.to_string());
    }

    #[tokio::test]
    async fn prompt_builder_receives_live_context() {
        let step = ClaudeCodeStep::with_prompt_builder(
            "ClaudeCodeStep",
            Config::default(),
            |ctx: &TaskContext| format!("event was: {}", ctx.event),
        )
        .with_transport(|_config, prompt| {
            Box::pin(async move {
                let mut outcome = stub_outcome();
                outcome.text = prompt;
                Ok(outcome)
            })
        });

        let mut ctx = empty_context();
        ctx.event = serde_json::json!({"ticket_id": "T-1"});

        let out = step.process(ctx).await.expect("process should succeed");

        let output = out.nodes.get("ClaudeCodeStep").expect("output present");
        assert!(output["content"]
            .as_str()
            .unwrap()
            .contains("\"ticket_id\":\"T-1\""));
    }
}
