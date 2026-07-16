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

use claude_code_rs::{parse::ContentBlock, Config, Outcome};
use engine_contract::{NodeRun, NodeRunStatus, TaskContext, Usage};
use futures::future::BoxFuture;

use crate::node::{Node, NodeError};

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

    /// Concatenate the response's `ContentBlock::Text` blocks into a single
    /// string (unrecognized block types are ignored here, not an error — the
    /// SDK already preserves them as `ContentBlock::Unknown`).
    fn text_output(outcome: &Outcome) -> String {
        outcome
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Unknown { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[async_trait::async_trait]
impl Node for ClaudeCodeStep {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let prompt = match &self.prompt {
            PromptSource::Fixed(prompt) => prompt.clone(),
            PromptSource::Builder(builder) => builder(&ctx),
        };

        let outcome = (self.transport)(self.config.clone(), prompt)
            .await
            .map_err(|err| NodeError::new(err.to_string()))?;

        let output = serde_json::json!({
            "content": Self::text_output(&outcome),
            "cost_usd": outcome.cost_usd,
            "model": outcome.model,
        });
        ctx.nodes.insert(self.name.clone(), output);

        let usage = Usage {
            input_tokens: Some(outcome.usage.input_tokens),
            output_tokens: Some(outcome.usage.output_tokens),
            model: outcome.model.clone(),
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

    use claude_code_rs::parse::Usage as SdkUsage;

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_outcome() -> Outcome {
        Outcome {
            cost_usd: 0.01,
            usage: SdkUsage {
                input_tokens: 12,
                output_tokens: 34,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model: "claude-sonnet-4-5".to_string(),
            content: vec![ContentBlock::Text {
                text: "ok".to_string(),
            }],
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
                outcome.content = vec![ContentBlock::Text { text: prompt }];
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
