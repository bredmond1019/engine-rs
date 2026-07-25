//! `ProposalCompanyResearchNode` — the WebSearch-backed `research`-stage
//! entry node, adapting `crate::workflows::research_agent::CompanyResearchNode`
//! into the proposal-generator pipeline.
//!
//! A non-terminal model node wrapping `crate::nodes::claude_code_step::ClaudeCodeStep`.
//! On `process`:
//! 1. read the run's [`super::policy::ProposalGeneratorPolicy`] stamped once
//!    at dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8)
//!    — no per-node re-resolution;
//! 2. apply `research`-stage shaping (model tier, prompt cache, verbosity
//!    directive) to the composed `Config` — WebSearch/WebFetch stay granted
//!    regardless of tier (this stage never resolves to `ModelTier::Local`,
//!    per `policy.rs`'s documented invariant);
//! 3. await the (injectable) transport and parse its reply into a
//!    [`crate::workflows::research_agent::schema::CompanyBrief`] — the same
//!    brief shape `RESEARCH_AGENT`'s `CompanyResearchNode` produces, reused
//!    here as the web brief `OpportunityIdentifierNode` falls back to when
//!    no `DiagnosticIntake` is present on the event;
//! 4. stamp the parsed brief + usage onto `ctx` and forward to
//!    `OpportunityIdentifierNode`.

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::research_agent::schema::{company_brief_json_schema, CompanyBrief};
use crate::workflows::{parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::ProposalGeneratorPolicy;
use super::schema::ProposalGeneratorEventSchema;

/// The `Node::name()` identity `ProposalCompanyResearchNode` runs its
/// composed `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key
/// its output/usage are stamped onto.
pub const NODE_NAME: &str = "ProposalCompanyResearchNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true — mirrors
/// `research_agent::company_research::STABLE_SYSTEM_PROMPT`'s rationale.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs PROPOSAL_GENERATOR workflow, \
     research stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Build the single-company research-brief prompt from the triggering
/// event, ported from `research_agent::company_research::build_prompt`.
fn build_prompt(event: &ProposalGeneratorEventSchema) -> String {
    let company_name = event.company_name.as_str();
    let company_url = event
        .company_url
        .as_deref()
        .map(|url| format!(" ({url})"))
        .unwrap_or_default();

    format!(
        "You are researching a single company on behalf of a solo AI & \
         Automations Engineer who builds production-grade agentic systems for \
         small and mid-size businesses: private knowledge systems (RAG over \
         internal docs), agentic workflow automation, interactive AI \
         assistants, and the application layer around them.\n\n\
         Use WebSearch/WebFetch to research \"{company_name}\"{company_url}: \
         what it does, its size/stage, and any recent public developments \
         (funding, launches, hiring signals, press). From that research, \
         infer likely operational pain points this practice's services could \
         address, and 2-4 concrete outreach hooks grounded in what you found \
         (cite the source URL for each). This brief seeds the downstream \
         automation-opportunity scoring for an AutomationRoadmap proposal.\n\n\
         Respond with strict JSON matching this shape: {{\"company_name\": \
         str, \"summary\": str, \"recent_developments\": [str], \
         \"pain_points\": [str], \"outreach_hooks\": [str], \"sources\": \
         [str]}}."
    )
}

/// The WebSearch-backed `research`-stage entry node of the
/// `PROPOSAL_GENERATOR` pipeline. Forwards to `OpportunityIdentifierNode`.
pub struct ProposalCompanyResearchNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl ProposalCompanyResearchNode {
    /// Construct with `WebSearch`/`WebFetch` granted and the company-brief
    /// `json_schema` set; `process` overwrites `model` per the resolved
    /// `research`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                allowed_tools: vec!["WebSearch".to_string(), "WebFetch".to_string()],
                json_schema: Some(company_brief_json_schema()),
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

impl Default for ProposalCompanyResearchNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ProposalCompanyResearchNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let policy: ProposalGeneratorPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.research,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt =
            crate::policy::apply_verbosity_directive(build_prompt(&event), policy.output_verbosity);

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

        let brief: CompanyBrief =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a CompanyBrief from the model's reply: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&brief).map_err(|err| {
                NodeError::new(format!("failed to serialize CompanyBrief: {err}"))
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

    fn company_event() -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            company_name: "Acme Corp".to_string(),
            company_url: Some("https://acme.example".to_string()),
            diagnostic_intake: None,
            policy: None,
            profile: None,
        }
    }

    fn stub_brief_json() -> serde_json::Value {
        json!({
            "company_name": "Acme Corp",
            "summary": "Widget manufacturer expanding into SaaS.",
            "recent_developments": ["Raised Series B"],
            "pain_points": ["Manual invoicing"],
            "outreach_hooks": ["Recent Series B raise"],
            "sources": ["https://acme.example/news"],
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_brief_json()).unwrap(),
                    cost_usd: 0.02,
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
                            cost_usd: 0.02,
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
    async fn process_populates_company_brief_and_usage() {
        let node = ProposalCompanyResearchNode::new()
            .with_transport(stub_transport(Some(stub_brief_json())));
        let ctx = empty_ctx(company_event());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.company_name, "Acme Corp");
        assert!(!brief.summary.is_empty());
        assert!(!brief.pain_points.is_empty());

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = ProposalCompanyResearchNode::new().with_transport(stub_transport(None));
        let ctx = empty_ctx(company_event());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.company_name, "Acme Corp");
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        // Task 8: the node reads a policy already resolved (event override
        // merged in) and stamped at dispatch — it no longer re-merges
        // `event.policy` itself, so this test stamps the *already-merged*
        // final policy directly rather than an `event.policy` override.
        let event = company_event();
        let policy = ProposalGeneratorPolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ModelTiers {
                research: ModelTier::Opus,
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
                    text: serde_json::to_string(&stub_brief_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_brief_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalCompanyResearchNode::new().with_transport(transport);
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
        assert!(config.allowed_tools.contains(&"WebSearch".to_string()));
        assert!(config.allowed_tools.contains(&"WebFetch".to_string()));
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node = ProposalCompanyResearchNode::new()
            .with_transport(stub_transport(Some(stub_brief_json())));
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
