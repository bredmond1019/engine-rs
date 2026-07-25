//! `ProposalReviseNode` — the `revise`-stage node that applies review notes
//! to produce a corrected `AutomationRoadmap`, then forwards to
//! `PersistToBrainNode`.
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. resolve the run's [`super::policy::ProposalGeneratorPolicy`] via
//!    [`super::profiles::resolve_policy_for_run`];
//! 2. compose a revision prompt from the upstream `ProposalWriterNode`
//!    draft plus `ProposalReviewNode`'s `notes`, with
//!    `Config.json_schema = automation_roadmap_json_schema()` so the
//!    corrected reply is schema-constrained (the composite/sort/≤3-profile
//!    validators still hold against it);
//! 3. apply `revise`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 4. await the (injectable) transport and parse its reply into a
//!    corrected [`super::schema::AutomationRoadmap`] via
//!    `parse_structured_or_fenced::<AutomationRoadmap>`;
//! 5. stamp the corrected roadmap + usage onto `ctx` and forward to
//!    `PersistToBrainNode`.

use std::path::PathBuf;

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::profiles::resolve_policy_for_run;
use super::review::NODE_NAME as REVIEW_NODE_NAME;
use super::schema::{
    automation_roadmap_json_schema, AutomationRoadmap, ProposalGeneratorEventSchema,
};
use super::writer::NODE_NAME as WRITER_NODE_NAME;

/// The `Node::name()` identity `ProposalReviseNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto. Read by `PersistToBrainNode`.
pub const NODE_NAME: &str = "ProposalReviseNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs PROPOSAL_GENERATOR workflow, \
     revise stage. This system prompt is held constant across calls so its \
     tokens can be cached.";

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Build the revision prompt: the upstream drafted roadmap plus the
/// review's notes (or notes that neither is present yet — this node can
/// also run standalone in a unit test).
fn build_prompt(ctx: &TaskContext, event: &ProposalGeneratorEventSchema) -> String {
    let draft = get_result(ctx, WRITER_NODE_NAME)
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream drafted roadmap available)".to_string());
    let notes = get_result(ctx, REVIEW_NODE_NAME)
        .and_then(|value| value.get("notes"))
        .and_then(|value| value.as_str())
        .filter(|notes| !notes.is_empty())
        .unwrap_or("(no review notes available)");

    format!(
        "You are correcting a drafted AutomationRoadmap proposal for \
         \"{company}\" per a reviewer's notes. Apply the review notes below \
         and produce a corrected roadmap that still (1) computes each \
         candidate's composite as \
         (frequency*0.35)+(time_cost*0.40)+(buildability*0.25), (2) sorts \
         candidates composite-descending, and (3) carries at most 3 top \
         workflow profiles. Respond with strict JSON matching the \
         AutomationRoadmap schema (situation, candidates, top_profiles, \
         recommendation).\n\n\
         Review notes:\n{notes}\n\n\
         Original drafted roadmap:\n{draft}",
        company = event.company_name,
    )
}

/// Read the worktree path stamped by an upstream setup node, if this run
/// went through one. Falls back to the process's current working directory.
fn worktree_path(ctx: &TaskContext) -> PathBuf {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The `revise`-stage node that applies review notes to produce a
/// corrected `AutomationRoadmap`. Forwards to `PersistToBrainNode`.
pub struct ProposalReviseNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl ProposalReviseNode {
    /// Construct with the `AutomationRoadmap` `json_schema` set; `process`
    /// overwrites `model` per the resolved `revise`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(automation_roadmap_json_schema()),
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

impl Default for ProposalReviseNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ProposalReviseNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let worktree = worktree_path(&ctx);

        let policy = resolve_policy_for_run(&ctx, &worktree)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.revise, &policy.local.model);
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

        let roadmap: AutomationRoadmap = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a corrected AutomationRoadmap from the \
                     model's reply: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&roadmap).map_err(|err| {
                NodeError::new(format!(
                    "failed to serialize corrected AutomationRoadmap: {err}"
                ))
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

    use super::super::policy::{ModelTier, PartialModelTiers, PartialProposalGeneratorPolicy};
    use super::super::schema::validate_automation_roadmap;
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

    fn corrected_roadmap_json() -> serde_json::Value {
        json!({
            "situation": {
                "company_name": "Loja da Ana",
                "business_type": "retail SMB",
                "team_size": 4,
                "painful_workflow_summary": "Orders tracked by scrolling WhatsApp threads.",
                "candidate_count": 1,
            },
            "candidates": [
                {
                    "name": "WhatsApp order tracking",
                    "frequency": 5.0,
                    "time_cost": 4.0,
                    "buildability": 4.0,
                    "composite": 4.35,
                    "tier": "quick_win",
                    "rationale": "Happens daily and is fully manual today.",
                }
            ],
            "top_profiles": [
                {
                    "name": "WhatsApp order tracking",
                    "today": "Manually scrolled.",
                    "proposed_solution": "Automated bot with human approval gate.",
                    "stack": "WhatsApp Business API + small service.",
                    "rough_scope": "2-3 weeks.",
                    "expected_roi": "Saves ~5 hrs/week.",
                }
            ],
            "recommendation": {
                "start_with": "WhatsApp order tracking",
                "phase_1_scope": ["Order intake bot"],
                "investment": "R$8,000-12,000 fixed fee",
                "how_it_works": "Connects to WhatsApp Business API.",
                "call_to_action": "Book a call to proceed.",
            },
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&corrected_roadmap_json()).unwrap(),
                    cost_usd: 0.03,
                    usage: SdkUsage {
                        input_tokens: 220,
                        output_tokens: 160,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 220,
                            output_tokens: 160,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.03,
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
    async fn process_produces_validator_passing_roadmap() {
        let node = ProposalReviseNode::new()
            .with_transport(stub_transport(Some(corrected_roadmap_json())));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        assert!(validate_automation_roadmap(&roadmap).is_ok());

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(220));
        assert_eq!(usage.output_tokens, Some(160));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = ProposalReviseNode::new().with_transport(stub_transport(None));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        assert!(validate_automation_roadmap(&roadmap).is_ok());
    }

    #[tokio::test]
    async fn process_reads_upstream_draft_and_review_notes_into_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&corrected_roadmap_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(corrected_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviseNode::new().with_transport(transport);
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            WRITER_NODE_NAME.to_string(),
            json!({ "situation": { "company_name": "Loja da Ana" } }),
        );
        ctx.nodes.insert(
            REVIEW_NODE_NAME.to_string(),
            json!({ "verdict": "revise", "notes": "Composite math is off for candidate 1." }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Composite math is off for candidate 1."));
        assert!(prompt.contains("Loja da Ana"));
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        let mut event = base_event();
        event.policy = Some(PartialProposalGeneratorPolicy {
            output_verbosity: Some(super::super::policy::OutputVerbosity::Terse),
            prompt_cache: Some(true),
            model_tiers: Some(PartialModelTiers {
                revise: Some(ModelTier::Local),
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
                    text: serde_json::to_string(&corrected_roadmap_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(corrected_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviseNode::new().with_transport(transport);
        let ctx = empty_ctx(event);
        node.process(ctx).await.expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("qwen2.5-coder:7b"));
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));
        assert!(prompt.contains("Be terse"));
        assert!(config.allowed_tools.is_empty());
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node = ProposalReviseNode::new()
            .with_transport(stub_transport(Some(corrected_roadmap_json())));
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
