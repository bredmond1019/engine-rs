//! `SelfCriticNode` — the `critic`-stage model node, emits
//! `CriticEvaluation` (EN.5.A task 7).
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. read whichever of `SummarizeNode`/`ReviseNode` most recently stored a
//!    `SummaryResult` (`ReviseNode` overwrites the `summarize`-stage output
//!    on the loop's back-edge, per `summarize.rs`'s doc comment) via the
//!    bound `summary_input` [`InputBinding`] (`EN.5.E` `with_input_from`),
//!    falling back to `SummarizeNode`'s identity when unbound;
//! 2. read the current loop pass from `IncrementCriticIterationNode`'s
//!    stored counter (`{"iteration": u64}`), defaulting to 0 when absent —
//!    the first critic pass has not yet gone through the back-edge;
//! 3. apply `critic`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 4. await the (injectable) transport and parse its reply into a
//!    `{verdict, confidence, issues}` shape via `parse_structured_or_fenced`,
//!    normalizing an ambiguous/malformed verdict to `Revise` — fail closed
//!    (`EN.4.C` `Verdict::from_model_text` precedent: an unclear signal must
//!    not let the loop exit early on a false `Pass`);
//! 5. `put_result` the `CriticEvaluation{verdict, confidence, issues,
//!    iteration}` under `NODE_NAME` — this slot is overwritten each loop
//!    pass; per-iteration history is carried by the `iteration` field, not
//!    `node_runs` (one entry per identity).

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, session_baseline, sessions_since,
    ModelTransport, TransportSlot,
};

use super::policy::ContentPipelinePolicy;
use super::schema::{CriticEvaluation, CriticVerdict};
use super::source_router;
use super::summarize;

/// The `Node::name()` identity `SelfCriticNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto. Read by `CriticRouterNode`.
pub const NODE_NAME: &str = "SelfCriticNode";

/// The upstream identity `SelfCriticNode` reads its loop counter from when
/// its own `iteration_input` binding is unbound.
const INCREMENT_NODE_NAME: &str = "IncrementCriticIterationNode";

/// The identity `ReviseNode` (task 9) stores its corrected `SummaryResult`
/// under on the loop's back-edge — checked as a fallback after the bound
/// summary identity so a later loop pass reads the revised summary rather
/// than the stale first-pass one. Not imported from `super::revise` (still
/// a task-9 scaffold at this task's authoring time) to avoid a forward
/// dependency; both identities are string constants by the same
/// `Node::name()` convention.
const REVISE_NODE_NAME: &str = "ReviseNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs CONTENT_PIPELINE workflow, \
     critic stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// The model's reply shape: a verdict plus confidence and issues.
#[derive(Debug, Clone, Deserialize)]
struct CriticOutput {
    verdict: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    issues: Vec<String>,
}

/// JSON schema matching [`CriticOutput`].
fn critic_output_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["pass", "revise"] },
            "confidence": { "type": "number" },
            "issues": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["verdict"],
    })
}

/// Normalize a model's free-form verdict text into a [`CriticVerdict`],
/// defaulting to `Revise` (fail closed — an ambiguous or malformed verdict
/// should not silently let the loop exit early) unless the text
/// unambiguously reads as a pass. Mirrors `proposal_generator::review::
/// Verdict::from_model_text`.
fn verdict_from_model_text(text: &str) -> CriticVerdict {
    if text.trim().to_lowercase().starts_with("pass") {
        CriticVerdict::Pass
    } else {
        CriticVerdict::Revise
    }
}

/// Reads whichever of `SummarizeNode`/`ReviseNode` most recently stored a
/// `SummaryResult` (tries `ReviseNode` first, since it overwrites
/// `SummarizeNode`'s output on the loop's back-edge, falling back to the
/// bound `summary_input` identity — `SummarizeNode`'s by default when
/// unbound — when no revise pass ran) and returns its `summary` text for
/// the critic prompt.
fn read_summary(ctx: &TaskContext, summary_input: &InputBinding) -> Result<String, NodeError> {
    let bound = summary_input.resolve(summarize::NODE_NAME);
    let stored = get_result(ctx, REVISE_NODE_NAME)
        .or_else(|| get_result(ctx, bound))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no summary stored by {bound} or {REVISE_NODE_NAME}"
            ))
        })?;
    stored
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: stored summary missing `summary`")))
}

/// Read the current loop pass from `IncrementCriticIterationNode`'s stored
/// counter (bound via `iteration_input`, falling back to its default
/// identity when unbound), defaulting to 0 when absent.
fn read_iteration(ctx: &TaskContext, iteration_input: &InputBinding) -> u32 {
    let bound = iteration_input.resolve(INCREMENT_NODE_NAME);
    get_result(ctx, bound)
        .or_else(|| get_result(ctx, INCREMENT_NODE_NAME))
        .and_then(|value| value.get("iteration"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32
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

/// Build the critic prompt from the current summary.
fn build_prompt(summary: &str) -> String {
    format!(
        "You are critiquing a summary drafted for a content digest. \
         Evaluate whether it is accurate, complete, and well-formed. \
         Respond with strict JSON matching {{\"verdict\": \"pass\" | \
         \"revise\", \"confidence\": number (0.0-1.0), \"issues\": \
         [str]}} — \"issues\" should list concrete problems when the \
         verdict is \"revise\", and may be empty on \"pass\".\n\n\
         Summary:\n{summary}"
    )
}

/// The `critic`-stage model node. Forwards to `CriticRouterNode`.
pub struct SelfCriticNode {
    config: Config,
    transport: TransportSlot,
    summary_input: InputBinding,
    iteration_input: InputBinding,
}

impl SelfCriticNode {
    /// Construct with the critic-output `json_schema` set; `process`
    /// overwrites `model` per the resolved `critic`-stage tier. Both
    /// upstream bindings start unbound — `EN.5.E` task 1's `InputBinding`
    /// default, which falls back to today's identities
    /// (`SummarizeNode`/`IncrementCriticIterationNode`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(critic_output_json_schema()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            summary_input: InputBinding::default(),
            iteration_input: InputBinding::default(),
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
    /// falls back to `SummarizeNode`'s identity (today's default).
    #[must_use]
    pub fn with_summary_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.summary_input = InputBinding::bound(upstream);
        self
    }

    /// Bind the identity this node reads its loop counter from. Unbound
    /// falls back to `IncrementCriticIterationNode`'s identity (today's
    /// default).
    #[must_use]
    pub fn with_iteration_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.iteration_input = InputBinding::bound(upstream);
        self
    }
}

impl Default for SelfCriticNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SelfCriticNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let summary = read_summary(&ctx, &self.summary_input)?;
        let iteration = read_iteration(&ctx, &self.iteration_input);
        let policy = resolved_policy(&ctx)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.critic, &policy.local.model);
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&summary),
            policy.output_verbosity,
        );

        let step = self
            .transport
            .apply(ClaudeCodeStep::new(NODE_NAME, config, prompt));

        // Baseline taken immediately before the billed call, per EN.14.C —
        // any wrapper `Err` returned below carries whatever the inner
        // `ClaudeCodeStep` appended to the ledger, so this critic wrapper's
        // billed session survives a post-billed-call parse/serialize failure.
        let baseline = session_baseline(&ctx);
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

        let parsed: CriticOutput =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a CriticEvaluation from the model's reply: {err}"
                ))
                .with_sessions(sessions_since(&ctx, baseline))
            })?;

        let verdict = verdict_from_model_text(&parsed.verdict);

        let evaluation = CriticEvaluation {
            verdict,
            confidence: parsed.confidence,
            issues: parsed.issues,
            iteration,
        };

        let mut result = serde_json::to_value(&evaluation).map_err(|err| {
            NodeError::new(format!("failed to serialize CriticEvaluation: {err}"))
                .with_sessions(sessions_since(&ctx, baseline))
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

    use super::super::policy::{ModelTier, ModelTiers, OutputVerbosity};
    use super::*;

    fn stub_critic_json(verdict: &str, confidence: f64, issues: Vec<&str>) -> serde_json::Value {
        json!({
            "verdict": verdict,
            "confidence": confidence,
            "issues": issues,
        })
    }

    fn ctx_with_summary(node_name: &str, summary: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            node_name,
            json!({ "summary": summary, "entities": [], "key_points": [] }),
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

    fn stub_transport(structured: serde_json::Value) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
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
                    structured_output: Some(structured.clone()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    #[tokio::test]
    async fn process_emits_pass_verdict() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.9,
            vec![],
        )));
        let ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
        assert!((evaluation.confidence - 0.9).abs() < f64::EPSILON);
        assert!(evaluation.issues.is_empty());
        assert_eq!(evaluation.iteration, 0);
    }

    #[tokio::test]
    async fn process_emits_revise_verdict_with_issues() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "revise",
            0.4,
            vec!["missing citation"],
        )));
        let ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
        assert_eq!(evaluation.issues, vec!["missing citation".to_string()]);
    }

    #[tokio::test]
    async fn process_defaults_ambiguous_verdict_text_to_revise() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "maybe good enough",
            0.5,
            vec![],
        )));
        let ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
    }

    #[tokio::test]
    async fn process_stamps_iteration_from_increment_counter_state() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "revise",
            0.5,
            vec![],
        )));
        let mut ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");
        put_result(&mut ctx, INCREMENT_NODE_NAME, json!({ "iteration": 2 }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.iteration, 2);
    }

    #[tokio::test]
    async fn process_defaults_iteration_to_zero_when_increment_node_absent() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.9,
            vec![],
        )));
        let ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.iteration, 0);
    }

    #[tokio::test]
    async fn process_reads_revise_node_summary_on_a_later_loop_pass() {
        // Simulates the back-edge: ReviseNode overwrote the summarize-stage
        // shape under its own identity after a prior revise.
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.95,
            vec![],
        )));
        let ctx = ctx_with_summary("ReviseNode", "A revised, more accurate summary.");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
    }

    #[tokio::test]
    async fn process_reads_bound_summary_and_iteration_inputs() {
        let node = SelfCriticNode::new()
            .with_transport(stub_transport(stub_critic_json("pass", 0.9, vec![])))
            .with_summary_input_from("CustomSummaryNode")
            .with_iteration_input_from("CustomIncrementNode");
        let mut ctx = ctx_with_summary("CustomSummaryNode", "Bound summary text.");
        put_result(&mut ctx, "CustomIncrementNode", json!({ "iteration": 1 }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let evaluation: CriticEvaluation =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation");
        assert_eq!(evaluation.iteration, 1);
        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let policy = ContentPipelinePolicy {
            output_verbosity: OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: ModelTiers {
                critic: ModelTier::Opus,
                ..ContentPipelinePolicy::default().model_tiers
            },
            ..ContentPipelinePolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let structured = stub_critic_json("pass", 0.9, vec![]);
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(structured.clone()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = SelfCriticNode::new().with_transport(transport);
        let mut ctx = ctx_with_summary(summarize::NODE_NAME, "A concise summary.");
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
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.9,
            vec![],
        )));
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
        assert!(err.message.contains("no summary stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_policy_is_stored() {
        let node = SelfCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.9,
            vec![],
        )));
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

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no policy stored by"));
    }
}
