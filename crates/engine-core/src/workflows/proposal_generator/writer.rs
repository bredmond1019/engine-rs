//! `ProposalWriterNode` — the `writer`-stage node that drafts the
//! `AutomationRoadmap` from the identified opportunities.
//!
//! A non-terminal, cloud-default model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep` (no WebSearch — it works
//! from `OpportunityIdentifierNode`'s already-scored candidates plus the
//! `ProposalCompanyResearchNode` brief). On `process`:
//! 1. read the run's [`super::policy::ProposalGeneratorPolicy`] stamped once
//!    at dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8)
//!    — no per-node re-resolution;
//! 2. compose a drafting prompt from the upstream
//!    `OpportunityIdentifierNode`/`ProposalCompanyResearchNode` output (when
//!    present — this node can also run standalone in a unit test) plus
//!    `Config.json_schema = automation_roadmap_json_schema()`;
//! 3. apply `writer`-stage shaping (model tier, prompt cache, verbosity
//!    directive);
//! 4. await the (injectable) transport and parse its reply into an
//!    [`super::schema::AutomationRoadmap`] via
//!    `parse_structured_or_fenced::<AutomationRoadmap>`;
//! 5. stamp the drafted roadmap + usage onto `ctx` and forward to
//!    `ProposalReviewNode`.

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::ProposalGeneratorPolicy;
use super::schema::{
    automation_roadmap_json_schema, AutomationRoadmap, ProposalGeneratorEventSchema,
};

/// The `Node::name()` identity `ProposalWriterNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto.
pub const NODE_NAME: &str = "ProposalWriterNode";

/// The upstream `ctx.nodes` identity this node reads scored candidates from,
/// when present (filled by the task-7 `OpportunityIdentifierNode`).
const OPPORTUNITY_NODE_NAME: &str = "OpportunityIdentifierNode";

/// The upstream `ctx.nodes` identity this node reads the web brief from,
/// when present (filled by `super::company_research::ProposalCompanyResearchNode`).
const RESEARCH_NODE_NAME: &str = "ProposalCompanyResearchNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs PROPOSAL_GENERATOR workflow, \
     writer stage. This system prompt is held constant across calls so its \
     tokens can be cached.";

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Build the drafting prompt: the triggering event's company inputs, the
/// upstream opportunity-scoring output (or a note that none is present yet
/// — this node can be driven standalone), and the upstream web brief.
fn build_prompt(ctx: &TaskContext, event: &ProposalGeneratorEventSchema) -> String {
    let opportunities = get_result(ctx, OPPORTUNITY_NODE_NAME)
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream opportunity scoring available)".to_string());
    let brief = get_result(ctx, RESEARCH_NODE_NAME)
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream research brief available)".to_string());

    format!(
        "You are drafting an AutomationRoadmap proposal for \"{company}\" on \
         behalf of a solo AI & Automations Engineer. Use the scored \
         automation candidates and the company research brief below to write \
         the four-section deliverable: (1) Situation & Opportunity, (2) the \
         ranked candidates table (frequency/time_cost/buildability on a 1-5 \
         scale, and their composite = (freq*0.35)+(time_cost*0.40)+(buildability*0.25), \
         sorted composite-descending), (3) at most 3 Top Workflow Profiles \
         (one page each, for the highest-composite candidates), and (4) the \
         Recommended First Engagement.\n\n\
         Scored candidates:\n{opportunities}\n\n\
         Company research brief:\n{brief}\n\n\
         Respond with strict JSON matching the AutomationRoadmap schema \
         (situation, candidates, top_profiles, recommendation).",
        company = event.company_name,
    )
}

/// The `writer`-stage node that drafts the `AutomationRoadmap`. Forwards to
/// `ProposalReviewNode`.
pub struct ProposalWriterNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl ProposalWriterNode {
    /// Construct with the `AutomationRoadmap` `json_schema` set (no
    /// WebSearch/WebFetch — this stage works from upstream outputs);
    /// `process` overwrites `model` per the resolved `writer`-stage tier.
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

impl Default for ProposalWriterNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ProposalWriterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let policy: ProposalGeneratorPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.writer, &policy.local.model);
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

        let roadmap: AutomationRoadmap =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse an AutomationRoadmap from the model's reply: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&roadmap).map_err(|err| {
                NodeError::new(format!("failed to serialize AutomationRoadmap: {err}"))
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

    use super::super::policy::ModelTier;
    use super::*;

    /// Builds a `ctx` and stamps a default [`ProposalGeneratorPolicy`] under
    /// `RESOLVED_POLICY_IDENTITY` — required since task 8's strict
    /// `resolved_policy_strict` read (no more per-node re-resolution or a
    /// silent `Default` fallback).
    fn empty_ctx(event: ProposalGeneratorEventSchema) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(ProposalGeneratorPolicy::default()).expect("policy serializes"),
        );
        ctx
    }

    fn base_event() -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            company_name: "Loja da Ana".to_string(),
            company_url: None,
            diagnostic_intake: None,
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn stub_roadmap_json() -> serde_json::Value {
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
                    text: serde_json::to_string(&stub_roadmap_json()).unwrap(),
                    cost_usd: 0.03,
                    usage: SdkUsage {
                        input_tokens: 200,
                        output_tokens: 150,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 200,
                            output_tokens: 150,
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
    async fn process_populates_automation_roadmap_and_usage() {
        let node =
            ProposalWriterNode::new().with_transport(stub_transport(Some(stub_roadmap_json())));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        assert_eq!(
            roadmap.situation.as_ref().unwrap().company_name,
            "Loja da Ana"
        );
        assert_eq!(roadmap.candidates.len(), 1);
        assert_eq!(roadmap.top_profiles.len(), 1);
        assert!(super::super::schema::validate_automation_roadmap(&roadmap).is_ok());

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(200));
        assert_eq!(usage.output_tokens, Some(150));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = ProposalWriterNode::new().with_transport(stub_transport(None));
        let ctx = empty_ctx(base_event());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        assert_eq!(roadmap.candidates.len(), 1);
    }

    #[tokio::test]
    async fn process_reads_upstream_opportunity_and_research_output_into_prompt() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_roadmap_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalWriterNode::new().with_transport(transport);
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            OPPORTUNITY_NODE_NAME.to_string(),
            json!({ "candidates": [{"name": "WhatsApp order tracking"}] }),
        );
        ctx.nodes.insert(
            RESEARCH_NODE_NAME.to_string(),
            json!({ "summary": "Widget manufacturer expanding into SaaS." }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("WhatsApp order tracking"));
        assert!(prompt.contains("Widget manufacturer expanding into SaaS."));
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        // Task 8: the node reads a policy already resolved (event override
        // merged in) and stamped at dispatch — it no longer re-merges
        // `event.policy` itself, so this test stamps the *already-merged*
        // final policy directly rather than an `event.policy` override.
        let event = base_event();
        let policy = ProposalGeneratorPolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ModelTiers {
                writer: ModelTier::Opus,
                ..ProposalGeneratorPolicy::default().model_tiers
            },
            ..ProposalGeneratorPolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_roadmap_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalWriterNode::new().with_transport(transport);
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        node.process(ctx).await.expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));
        assert!(prompt.contains("Be terse"));
        assert!(config.allowed_tools.is_empty());
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node =
            ProposalWriterNode::new().with_transport(stub_transport(Some(stub_roadmap_json())));
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
