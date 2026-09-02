//! `ReviseNode` — the `revise`-stage correction node on the LINKEDIN_POST
//! brand-critic loop's back-edge (`planning/EN.5.G/tasks.md` + `tasks.json`
//! task 5), following `content_pipeline::revise::ReviseNode`.
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. read the current draft (its own prior pass first, via `NODE_NAME` —
//!    a later loop pass revises the previous revision rather than
//!    re-deriving from scratch, mirroring `content_pipeline::revise.rs`'s
//!    read-preference — falling back to the bound `draft_input` binding,
//!    defaulting to [`super::draft::NODE_NAME`]) plus that same identity's
//!    `sources`, kept unchanged across a revision — the traceability
//!    invariant on `PostCandidate` is a fact about which real work backs a
//!    claim, not something a wording pass should touch;
//! 2. read `BrandCriticNode`'s stored `issues` (bound `critic_input`,
//!    falling back to [`super::brand_critic::NODE_NAME`]);
//! 3. apply `draft`-stage model-tier shaping — `LinkedInPostPolicy` has no
//!    dedicated `revise` tier (task 3 scoped the three Local-eligible
//!    stages to `{draft, critic, translate}`), and a revision is drafting
//!    work, not judging work, so it reuses `policy.model_tiers.draft`;
//! 4. await the (injectable) transport and parse its reply into a
//!    `{draft}` shape;
//! 5. `put_result` `{draft, sources}` under `NODE_NAME` — read back by
//!    `BrandCriticNode` (and, once task 6 wires it, the eventual terminal
//!    renderer) as the read-preference fallback ahead of the bound draft
//!    identity.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::brand_critic;
use super::draft;
use super::policy::LinkedInPostPolicy;
use super::schema::WorkSource;

/// The `Node::name()` identity `ReviseNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes` key its output is stamped
/// onto. Read by `BrandCriticNode` as its read-preference fallback ahead
/// of the bound draft identity.
pub const NODE_NAME: &str = "ReviseNode";

/// The model's reply shape: only the revised prose changes; `sources`
/// carries over from the pre-revision draft rather than being re-asserted
/// by the model, so a revision pass cannot silently widen or drop what a
/// candidate is allowed to claim.
#[derive(Debug, Clone, Deserialize)]
struct RevisedDraft {
    draft: String,
}

/// JSON schema matching [`RevisedDraft`].
fn revised_draft_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "draft": { "type": "string" },
        },
        "required": ["draft"],
    })
}

/// Reads this node's own prior output first (`NODE_NAME`), falling back to
/// whichever identity the bound `draft_input` resolves to
/// ([`draft::NODE_NAME`] when unbound), and returns its `(draft, sources)`
/// for the revision prompt and pass-through.
fn read_draft(
    ctx: &TaskContext,
    draft_input: &InputBinding,
) -> Result<(String, Vec<WorkSource>), NodeError> {
    let bound = draft_input.resolve(draft::NODE_NAME);
    let stored = get_result(ctx, NODE_NAME)
        .or_else(|| get_result(ctx, bound))
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: no draft stored by {bound}")))?;
    let text = stored
        .get("draft")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: stored result missing `draft`")))?;
    let sources: Vec<WorkSource> = stored
        .get("sources")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid stored `sources`: {err}")))?
        .unwrap_or_default();
    Ok((text, sources))
}

/// Reads whichever identity the bound `critic_input` resolves to (falling
/// back to [`brand_critic::NODE_NAME`] when unbound) and returns its
/// `issues` list for the revision prompt.
fn read_issues(ctx: &TaskContext, critic_input: &InputBinding) -> Result<Vec<String>, NodeError> {
    let bound = critic_input.resolve(brand_critic::NODE_NAME);
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

/// Build the revision prompt from the current draft plus the brand
/// critic's issues, carrying [`brand_critic::RUBRIC`] verbatim so the
/// revision pass sees the exact rubric its output will be re-judged
/// against.
fn build_prompt(draft: &str, issues: &[String]) -> String {
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
        "You are revising a drafted LinkedIn post per a brand critic's issues. Apply the issues \
         below and produce a corrected draft that still reads first-person and plainly stated. \
         Do not introduce any new claim, metric, or detail not already present in the original \
         draft.\n\n\
         RUBRIC (verbatim from brand.md \"Voice — avoiding AI slop\"):\n{}\n\n\
         Respond with strict JSON matching {{\"draft\": str}}.\n\n\
         Critic issues:\n{issues_text}\n\n\
         Original draft:\n{draft}",
        brand_critic::RUBRIC
    )
}

/// The `revise`-stage node that applies `BrandCriticNode`'s issues to
/// produce a corrected draft. Forwards to `BrandCriticNode` — the loop's
/// back-edge re-entry.
pub struct ReviseNode {
    config: Config,
    transport: Option<ModelTransport>,
    draft_input: InputBinding,
    critic_input: InputBinding,
}

impl ReviseNode {
    /// Construct with the revised-draft `json_schema` set; `process`
    /// overwrites `model` per the resolved `draft`-stage tier (see the
    /// module doc comment for why `draft`, not a dedicated `revise` tier).
    /// Both upstream bindings start unbound — falls back to
    /// [`draft::NODE_NAME`] / [`brand_critic::NODE_NAME`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(revised_draft_json_schema()),
                ..Config::default()
            },
            transport: None,
            draft_input: InputBinding::default(),
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

    /// Bind the identity this node reads its pre-revision draft from.
    /// Unbound falls back to [`draft::NODE_NAME`] (today's default).
    #[must_use]
    pub fn with_draft_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.draft_input = InputBinding::bound(upstream);
        self
    }

    /// Bind the identity this node reads its critic evaluation from.
    /// Unbound falls back to [`brand_critic::NODE_NAME`] (today's
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
        let (draft_text, sources) = read_draft(&ctx, &self.draft_input)?;
        let issues = read_issues(&ctx, &self.critic_input)?;
        let policy: LinkedInPostPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.draft, &policy.local.model);
        let prompt = build_prompt(&draft_text, &issues);

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

        let revised: RevisedDraft =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a revised draft from the model's reply: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({ "draft": revised.draft, "sources": sources }),
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

    use super::super::policy::{LinkedinPostModelTiers, ModelTier};
    use super::super::schema::WorkSourceKind;
    use super::*;

    fn fixture_sources() -> serde_json::Value {
        json!([{ "kind": "commit", "id": "abc123", "summary": "did a thing" }])
    }

    fn revised_json(text: &str) -> serde_json::Value {
        json!({ "draft": text })
    }

    fn ctx_with_draft_and_critic(
        draft_node: &str,
        draft: &str,
        sources: serde_json::Value,
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
            draft_node,
            json!({ "draft": draft, "sources": sources }),
        );
        put_result(
            &mut ctx,
            critic_node,
            json!({
                "verdict": "revise",
                "confidence": 0.4,
                "issues": issues,
                "iteration": 0,
                "capped": false,
            }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
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
                    session_id: None,
                    structured_output: Some(structured),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    #[tokio::test]
    async fn process_produces_revised_draft_and_carries_sources_unchanged() {
        let node = ReviseNode::new()
            .with_transport(stub_transport(revised_json("A corrected, plainer draft.")));
        let ctx = ctx_with_draft_and_critic(
            draft::NODE_NAME,
            "This isn't just a script, it's a system.",
            fixture_sources(),
            brand_critic::NODE_NAME,
            vec!["rhetorical contrast setup"],
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            ctx.nodes[NODE_NAME]["draft"],
            json!("A corrected, plainer draft.")
        );
        let sources: Vec<WorkSource> =
            serde_json::from_value(ctx.nodes[NODE_NAME]["sources"].clone()).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, WorkSourceKind::Commit);
        assert_eq!(sources[0].id, "abc123");

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(90));
        assert_eq!(usage.output_tokens, Some(40));
    }

    #[tokio::test]
    async fn process_reads_upstream_draft_and_issues_into_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&revised_json("fine")).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(revised_json("fine")),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new().with_transport(transport);
        let ctx = ctx_with_draft_and_critic(
            draft::NODE_NAME,
            "The original, tic-laden draft.",
            fixture_sources(),
            brand_critic::NODE_NAME,
            vec!["stacked em-dashes"],
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("stacked em-dashes"));
        assert!(prompt.contains("The original, tic-laden draft."));
        assert!(prompt.contains("Hedge phrases."));
    }

    #[tokio::test]
    async fn process_reads_bound_upstreams_via_with_input_from() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |_config, prompt| {
            *captured_clone.lock().unwrap() = Some(prompt);
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&revised_json("fine")).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(revised_json("fine")),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new()
            .with_transport(transport)
            .with_draft_input_from("CustomDraftNode")
            .with_critic_input_from("CustomCriticNode");
        let ctx = ctx_with_draft_and_critic(
            "CustomDraftNode",
            "Bound draft text.",
            fixture_sources(),
            "CustomCriticNode",
            vec!["bound issue surfaced"],
        );

        node.process(ctx).await.expect("process should succeed");

        let prompt = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("bound issue surfaced"));
        assert!(prompt.contains("Bound draft text."));
    }

    #[tokio::test]
    async fn process_prefers_its_own_prior_pass_over_the_bound_draft_identity() {
        let node =
            ReviseNode::new().with_transport(stub_transport(revised_json("second-pass revision")));
        let mut ctx = ctx_with_draft_and_critic(
            draft::NODE_NAME,
            "stale first-pass draft",
            fixture_sources(),
            brand_critic::NODE_NAME,
            vec!["still off"],
        );
        // Simulate a first revise pass having already run.
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({ "draft": "first-pass revision", "sources": fixture_sources() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["draft"], json!("second-pass revision"));
    }

    #[tokio::test]
    async fn process_applies_draft_stage_model_tier() {
        let policy = LinkedInPostPolicy {
            model_tiers: LinkedinPostModelTiers {
                draft: ModelTier::Opus,
                ..LinkedInPostPolicy::default().model_tiers
            },
            ..LinkedInPostPolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<Config>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, _prompt| {
            *captured_clone.lock().unwrap() = Some(config);
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&revised_json("fine")).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(revised_json("fine")),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ReviseNode::new().with_transport(transport);
        let mut ctx = ctx_with_draft_and_critic(
            draft::NODE_NAME,
            "The original draft.",
            fixture_sources(),
            brand_critic::NODE_NAME,
            vec![],
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );

        node.process(ctx).await.expect("process should succeed");

        let config = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_draft_is_stored() {
        let node = ReviseNode::new().with_transport(stub_transport(revised_json("fine")));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            brand_critic::NODE_NAME,
            json!({ "verdict": "revise", "confidence": 0.4, "issues": [], "iteration": 0 }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no draft stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_critic_evaluation_is_stored() {
        let node = ReviseNode::new().with_transport(stub_transport(revised_json("fine")));
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(
            &mut ctx,
            draft::NODE_NAME,
            json!({ "draft": "A draft.", "sources": fixture_sources() }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no critic evaluation stored by"));
    }
}
