//! `ProposalReviewNode` — the `review`-stage node that judges the drafted
//! roadmap and emits a `pass`/`revise` verdict; short-circuits to `pass`
//! under `ReviewMode::Skip`.
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. resolve the run's [`super::policy::ProposalGeneratorPolicy`] via
//!    [`super::profiles::resolve_policy_for_run`];
//! 2. when `policy.review_mode` is [`super::policy::ReviewMode::Skip`],
//!    short-circuit straight to a `pass` verdict with no model call;
//! 3. otherwise compose a review prompt from the upstream
//!    `ProposalWriterNode` draft, apply `review`-stage shaping (model tier,
//!    prompt cache, verbosity directive), await the (injectable) transport,
//!    and parse its reply into a `pass`/`revise` verdict + notes;
//! 4. stamp `{"verdict": "pass" | "revise", "notes": String}` onto
//!    `ctx.nodes["ProposalReviewNode"]` for `ProposalReviewRouterNode` to
//!    read.

use std::path::PathBuf;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::ReviewMode;
use super::profiles::resolve_policy_for_run;
use super::schema::ProposalGeneratorEventSchema;

/// The `Node::name()` identity `ProposalReviewNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto. Read by `ProposalReviewRouterNode`.
pub const NODE_NAME: &str = "ProposalReviewNode";

/// The upstream `ctx.nodes` identity this node reads the drafted roadmap
/// from (filled by `super::writer::ProposalWriterNode`).
const WRITER_NODE_NAME: &str = "ProposalWriterNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs PROPOSAL_GENERATOR workflow, \
     review stage. This system prompt is held constant across calls so its \
     tokens can be cached.";

/// The verdict `ProposalReviewNode` emits into `ctx.nodes["ProposalReviewNode"]`,
/// consumed by `ProposalReviewRouterNode::route`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Revise,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Revise => "revise",
        }
    }

    /// Normalize a model's free-form verdict text into a [`Verdict`],
    /// defaulting to `Revise` (fail closed — an ambiguous or malformed
    /// verdict should not silently pass a roadmap through) unless the text
    /// unambiguously reads as a pass.
    fn from_model_text(text: &str) -> Self {
        if text.trim().to_lowercase().starts_with("pass") {
            Verdict::Pass
        } else {
            Verdict::Revise
        }
    }
}

/// The model's reply shape: a verdict plus supporting notes.
#[derive(Debug, Clone, Deserialize)]
struct ReviewOutput {
    verdict: String,
    #[serde(default)]
    notes: String,
}

/// JSON schema matching [`ReviewOutput`].
fn review_output_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["pass", "revise"] },
            "notes": { "type": "string" },
        },
        "required": ["verdict"],
    })
}

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Build the review prompt from the upstream drafted roadmap (or a note
/// that none is present yet — this node can be driven standalone).
fn build_prompt(ctx: &TaskContext, event: &ProposalGeneratorEventSchema) -> String {
    let draft = get_result(ctx, WRITER_NODE_NAME)
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream drafted roadmap available)".to_string());

    format!(
        "You are reviewing a drafted AutomationRoadmap proposal for \
         \"{company}\" before it is sent to the client. Check that the \
         situation summary is grounded, the candidate scores/composites are \
         plausible and sorted composite-descending, there are at most 3 top \
         workflow profiles, and the recommendation names the highest-scoring \
         candidate. Respond with strict JSON of the shape \
         {{\"verdict\": \"pass\" | \"revise\", \"notes\": str}} — \"notes\" \
         should explain what to fix when the verdict is \"revise\".\n\n\
         Drafted roadmap:\n{draft}",
        company = event.company_name,
    )
}

/// The `review`-stage node that judges the drafted roadmap. Forwards to
/// `ProposalReviewRouterNode`, which reads this node's stored verdict.
pub struct ProposalReviewNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl ProposalReviewNode {
    /// Construct with the review-verdict `json_schema` set; `process`
    /// overwrites `model` per the resolved `review`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(review_output_json_schema()),
                ..Config::default()
            },
            transport: None,
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
}

impl Default for ProposalReviewNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ProposalReviewNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let worktree = get_result(&ctx, "SetupWorktreeNode")
            .and_then(|value| value.get("worktree_path"))
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let policy = resolve_policy_for_run(&ctx, &worktree)?;

        // `review_mode = Skip` short-circuits straight to `pass` — no
        // model call, no revise branch ever taken.
        if policy.review_mode == ReviewMode::Skip {
            let mut ctx = ctx;
            put_result(
                &mut ctx,
                NODE_NAME,
                json!({
                    "verdict": Verdict::Pass.as_str(),
                    "notes": "review skipped (review_mode = Skip)",
                }),
            );
            return Ok(ctx);
        }

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.review, &policy.local.model);
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&ctx, &event),
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

        let parsed: ReviewOutput =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a review verdict from the model's reply: {err}"
                ))
            })?;

        let verdict = Verdict::from_model_text(&parsed.verdict);

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "verdict": verdict.as_str(),
                "notes": parsed.notes,
            }),
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

    use super::super::policy::{
        ModelTier, PartialModelTiers, PartialProposalGeneratorPolicy, ReviewMode,
    };
    use super::*;

    fn empty_ctx(event: ProposalGeneratorEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn base_event() -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            company_name: "Loja da Ana".to_string(),
            company_url: None,
            diagnostic_intake: None,
            policy: None,
            profile: None,
        }
    }

    fn stub_transport(verdict: &'static str, notes: &'static str) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&json!({ "verdict": verdict, "notes": notes }))
                        .unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 50,
                        output_tokens: 20,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 50,
                            output_tokens: 20,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    structured_output: Some(json!({ "verdict": verdict, "notes": notes })),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    fn panics_if_called_transport() -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            async move { panic!("transport should not be called under review_mode = Skip") }.boxed()
        })
    }

    #[tokio::test]
    async fn process_emits_pass_verdict() {
        let node = ProposalReviewNode::new().with_transport(stub_transport("pass", "Looks good."));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["verdict"], json!("pass"));
        assert_eq!(result["notes"], json!("Looks good."));
    }

    #[tokio::test]
    async fn process_emits_revise_verdict() {
        let node = ProposalReviewNode::new()
            .with_transport(stub_transport("revise", "Composite math is off."));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["verdict"], json!("revise"));
        assert_eq!(result["notes"], json!("Composite math is off."));
    }

    #[tokio::test]
    async fn process_short_circuits_to_pass_under_review_mode_skip_with_no_transport_call() {
        let mut event = base_event();
        event.policy = Some(PartialProposalGeneratorPolicy {
            review_mode: Some(ReviewMode::Skip),
            ..Default::default()
        });

        let node = ProposalReviewNode::new().with_transport(panics_if_called_transport());
        let ctx = empty_ctx(event);

        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["verdict"], json!("pass"));
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let mut event = base_event();
        event.policy = Some(PartialProposalGeneratorPolicy {
            output_verbosity: Some(super::super::policy::OutputVerbosity::Terse),
            prompt_cache: Some(true),
            model_tiers: Some(PartialModelTiers {
                review: Some(ModelTier::Local),
                ..Default::default()
            }),
            local: Some(super::super::policy::PartialLocalConfig {
                endpoint: None,
                model: Some("qwen2.5-coder:7b".to_string()),
                constrained_json: None,
            }),
            ..Default::default()
        });

        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&json!({ "verdict": "pass", "notes": "" }))
                        .unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(json!({ "verdict": "pass", "notes": "" })),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviewNode::new().with_transport(transport);
        let ctx = empty_ctx(event);
        node.process(ctx).await.expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("qwen2.5-coder:7b"));
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));
        assert!(prompt.contains("Be terse"));
    }

    #[tokio::test]
    async fn process_reads_upstream_writer_output_into_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&json!({ "verdict": "pass", "notes": "" }))
                        .unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(json!({ "verdict": "pass", "notes": "" })),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviewNode::new().with_transport(transport);
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            WRITER_NODE_NAME.to_string(),
            json!({ "situation": { "company_name": "Loja da Ana" } }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Loja da Ana"));
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node = ProposalReviewNode::new().with_transport(stub_transport("pass", ""));
        let ctx = TaskContext {
            event: json!({ "not_company_name": "oops" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid PROPOSAL_GENERATOR event"));
    }
}
