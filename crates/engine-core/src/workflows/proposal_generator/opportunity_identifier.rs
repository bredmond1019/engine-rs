//! `OpportunityIdentifierNode` — the evidence-aware `opportunity`-stage
//! scoring node. Scores from `event.diagnostic_intake`'s `*_evidence`
//! fields when present, else falls back to the web brief.
//!
//! A non-terminal, Local-eligible model node (single-shot judgment)
//! wrapping `crate::nodes::claude_code_step::ClaudeCodeStep`. No
//! WebSearch/WebFetch — this stage only ever reasons over already-gathered
//! evidence. On `process`:
//! 1. read the run's [`super::policy::ProposalGeneratorPolicy`] stamped once
//!    at dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8)
//!    — no per-node re-resolution;
//! 2. compose a scoring prompt — when `event.diagnostic_intake` is
//!    present, from its `top_workflows[].{frequency_evidence,
//!    time_cost_evidence, buildability_notes}` fields (`rubric.md §1`);
//!    when absent, from the upstream `ProposalCompanyResearchNode`'s web
//!    brief (`ctx.nodes["ProposalCompanyResearchNode"]`) — plus
//!    `Config.json_schema` requesting the candidate list shape;
//! 3. apply `opportunity`-stage shaping (model tier, prompt cache,
//!    verbosity directive) — this stage may rewire to `ModelTier::Local`
//!    under a Local-tier profile;
//! 4. await the (injectable) transport and parse its reply into a
//!    `Vec<`[`RankedCandidate`]`>`, computing/overwriting each candidate's
//!    `composite` = `(freq*0.35)+(time_cost*0.40)+(buildability*0.25)` and
//!    `tier` from the model's raw axis scores (so the composite/sort
//!    invariants hold even if the model's own arithmetic drifts), then
//!    sorting composite-descending;
//! 5. stamp the scored candidates + usage onto `ctx` and forward to
//!    `ProposalWriterNode`.

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::ProposalGeneratorPolicy;
use super::schema::{composite_score, PriorityTier, ProposalGeneratorEventSchema, RankedCandidate};

/// The `Node::name()` identity `OpportunityIdentifierNode` runs its
/// composed `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key
/// its output/usage are stamped onto. Read by `ProposalWriterNode`.
pub const NODE_NAME: &str = "OpportunityIdentifierNode";

/// The upstream `ctx.nodes` identity this node reads the web brief from
/// when `event.diagnostic_intake` is absent (filled by
/// `super::company_research::ProposalCompanyResearchNode`).
const RESEARCH_NODE_NAME: &str = "ProposalCompanyResearchNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs PROPOSAL_GENERATOR workflow, \
     opportunity stage. This system prompt is held constant across calls so \
     its tokens can be cached.";

/// The model's raw per-candidate scoring output, before the node overwrites
/// `composite`/`tier` deterministically.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct RawScoredCandidate {
    name: String,
    frequency: f64,
    time_cost: f64,
    buildability: f64,
    #[serde(default)]
    rationale: String,
}

/// The model's reply shape: a flat list of scored candidates.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ScoredCandidates {
    #[serde(default)]
    candidates: Vec<RawScoredCandidate>,
}

/// JSON schema for [`ScoredCandidates`], passed as `Config.json_schema`.
fn scored_candidates_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "frequency": { "type": "number" },
                        "time_cost": { "type": "number" },
                        "buildability": { "type": "number" },
                        "rationale": { "type": "string" },
                    },
                    "required": ["name", "frequency", "time_cost", "buildability"],
                },
            },
        },
        "required": ["candidates"],
    })
}

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Build the evidence block from `event.diagnostic_intake.top_workflows`'s
/// `*_evidence`/`buildability_notes` fields (`rubric.md §1`), when present.
fn evidence_block(event: &ProposalGeneratorEventSchema) -> Option<String> {
    let intake = event.diagnostic_intake.as_ref()?;
    let mut block = format!(
        "Diagnostic-intake evidence for \"{}\" ({}, team size {}):\n",
        intake.company_name, intake.company_type, intake.team_size
    );
    for candidate in &intake.top_workflows {
        block.push_str(&format!(
            "- {name}: {description}\n  frequency evidence: {freq}\n  \
             time-cost evidence: {time}\n  buildability notes: {build}\n",
            name = candidate.name,
            description = candidate.description,
            freq = candidate.frequency_evidence,
            time = candidate.time_cost_evidence,
            build = candidate.buildability_notes,
        ));
    }
    Some(block)
}

/// Build the fallback web-brief block from the upstream
/// `ProposalCompanyResearchNode` output, when no `diagnostic_intake` is
/// present on the event.
fn brief_block(ctx: &TaskContext) -> String {
    get_result(ctx, RESEARCH_NODE_NAME)
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream research brief available)".to_string())
}

/// Build the scoring prompt: evidence-based when `diagnostic_intake` is
/// present, else the web-brief fallback — both instructed against the
/// same rubric axes.
fn build_prompt(ctx: &TaskContext, event: &ProposalGeneratorEventSchema) -> String {
    let (source_label, source_block) = match evidence_block(event) {
        Some(evidence) => ("diagnostic-intake evidence", evidence),
        None => ("company research brief", brief_block(ctx)),
    };

    format!(
        "You are scoring candidate automation workflows for \"{company}\" on \
         behalf of a solo AI & Automations Engineer, using the {source_label} \
         below. For each candidate workflow, score three axes on a 1-5 scale: \
         Frequency (how often it happens), Time-cost (how much time it \
         consumes), and Buildability (how feasible it is to automate given \
         the systems/APIs involved). Ground each score in the evidence — do \
         not invent workflows the source doesn't support.\n\n\
         {source_label}:\n{source_block}\n\n\
         Respond with strict JSON: {{\"candidates\": [{{\"name\": str, \
         \"frequency\": number, \"time_cost\": number, \"buildability\": \
         number, \"rationale\": str}}]}}.",
        company = event.company_name,
    )
}

/// Deterministically compute `composite`/`tier` from a model's raw
/// per-candidate axis scores, and sort the result composite-descending —
/// so the scoring invariants hold regardless of the model's own arithmetic.
fn score_and_sort(raw: Vec<RawScoredCandidate>) -> Vec<RankedCandidate> {
    let mut scored: Vec<RankedCandidate> = raw
        .into_iter()
        .map(|candidate| {
            let composite = composite_score(
                candidate.frequency,
                candidate.time_cost,
                candidate.buildability,
            );
            RankedCandidate {
                name: candidate.name,
                frequency: candidate.frequency,
                time_cost: candidate.time_cost,
                buildability: candidate.buildability,
                composite,
                tier: PriorityTier::from_composite(composite),
                rationale: candidate.rationale,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.composite
            .partial_cmp(&a.composite)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// The evidence-aware `opportunity`-stage scoring node. Forwards to
/// `ProposalWriterNode`.
pub struct OpportunityIdentifierNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl OpportunityIdentifierNode {
    /// Construct with the scored-candidates `json_schema` set (no
    /// WebSearch/WebFetch — this stage only reasons over already-gathered
    /// evidence); `process` overwrites `model` per the resolved
    /// `opportunity`-stage tier (Local-eligible).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(scored_candidates_json_schema()),
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

impl Default for OpportunityIdentifierNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for OpportunityIdentifierNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let policy: ProposalGeneratorPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.opportunity,
            &policy.local.model,
        );
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

        let raw: ScoredCandidates =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse scored candidates from the model's reply: {err}"
                ))
            })?;

        let candidates = score_and_sort(raw.candidates);

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(json!({ "candidates": candidates })).map_err(|err| {
                NodeError::new(format!("failed to serialize scored candidates: {err}"))
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
    use crate::workflows::diagnostic_intake::schema::{DiagnosticIntake, WorkflowCandidate};

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

    fn event_with_intake() -> ProposalGeneratorEventSchema {
        let mut event = base_event();
        event.diagnostic_intake = Some(DiagnosticIntake {
            company_name: "Loja da Ana".to_string(),
            company_type: "retail SMB".to_string(),
            team_size: 4,
            primary_channels: vec!["WhatsApp".to_string()],
            existing_tools: vec!["Google Sheets".to_string()],
            existing_automations: vec![],
            top_workflows: vec![
                WorkflowCandidate {
                    name: "WhatsApp order tracking".to_string(),
                    description: "Orders tracked in chat.".to_string(),
                    frequency_evidence: "\"Every day.\"".to_string(),
                    time_cost_evidence: "\"An hour a day.\"".to_string(),
                    buildability_notes: "API available.".to_string(),
                    knowledge_holder: "Maria.".to_string(),
                    failure_mode: "Orders lost.".to_string(),
                },
                WorkflowCandidate {
                    name: "Supplier follow-up messages".to_string(),
                    description: "Manual follow-up texts.".to_string(),
                    frequency_evidence: "\"A few times a week.\"".to_string(),
                    time_cost_evidence: "\"20 min each.\"".to_string(),
                    buildability_notes: "Templated messages.".to_string(),
                    knowledge_holder: "Carlos.".to_string(),
                    failure_mode: "Late follow-up.".to_string(),
                },
            ],
        });
        event
    }

    fn stub_scored_json() -> serde_json::Value {
        json!({
            "candidates": [
                {
                    "name": "Supplier follow-up messages",
                    "frequency": 3.0,
                    "time_cost": 2.0,
                    "buildability": 4.0,
                    "rationale": "Frequent but lower time cost."
                },
                {
                    "name": "WhatsApp order tracking",
                    "frequency": 5.0,
                    "time_cost": 4.0,
                    "buildability": 4.0,
                    "rationale": "Happens daily and is fully manual today."
                }
            ]
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_scored_json()).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 80,
                        output_tokens: 40,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 80,
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
    async fn process_scores_from_diagnostic_intake_evidence_when_present() {
        let node = OpportunityIdentifierNode::new()
            .with_transport(stub_transport(Some(stub_scored_json())));
        let ctx = empty_ctx(event_with_intake());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let output = &ctx.nodes[NODE_NAME];
        let candidates: Vec<RankedCandidate> =
            serde_json::from_value(output["candidates"].clone()).expect("valid candidates");
        assert_eq!(candidates.len(), 2);

        // composite-descending
        assert_eq!(candidates[0].name, "WhatsApp order tracking");
        assert_eq!(candidates[1].name, "Supplier follow-up messages");
        assert!(candidates[0].composite >= candidates[1].composite);

        // composite recomputed deterministically
        let expected = composite_score(5.0, 4.0, 4.0);
        assert!((candidates[0].composite - expected).abs() < 1e-9);

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(80));
        assert_eq!(usage.output_tokens, Some(40));
    }

    #[tokio::test]
    async fn process_falls_back_to_web_brief_when_intake_absent() {
        let node = OpportunityIdentifierNode::new()
            .with_transport(stub_transport(Some(stub_scored_json())));
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            "ProposalCompanyResearchNode".to_string(),
            json!({ "summary": "Widget manufacturer expanding into SaaS.", "pain_points": ["Manual invoicing"] }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let output = &ctx.nodes[NODE_NAME];
        let candidates: Vec<RankedCandidate> =
            serde_json::from_value(output["candidates"].clone()).expect("valid candidates");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "WhatsApp order tracking");
    }

    #[tokio::test]
    async fn process_falls_back_prompt_mentions_web_brief_when_intake_absent() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_scored_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_scored_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = OpportunityIdentifierNode::new().with_transport(transport);
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            "ProposalCompanyResearchNode".to_string(),
            json!({ "summary": "Widget manufacturer expanding into SaaS." }),
        );

        node.process(ctx).await.expect("process should succeed");
        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("company research brief"));
        assert!(prompt.contains("Widget manufacturer expanding into SaaS."));
    }

    #[tokio::test]
    async fn process_prompt_mentions_diagnostic_intake_evidence_when_present() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_scored_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_scored_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = OpportunityIdentifierNode::new().with_transport(transport);
        let ctx = empty_ctx(event_with_intake());

        node.process(ctx).await.expect("process should succeed");
        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("diagnostic-intake evidence"));
        assert!(prompt.contains("\"Every day.\""));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = OpportunityIdentifierNode::new().with_transport(stub_transport(None));
        let ctx = empty_ctx(event_with_intake());

        let ctx = node.process(ctx).await.expect("process should succeed");
        let output = &ctx.nodes[NODE_NAME];
        let candidates: Vec<RankedCandidate> =
            serde_json::from_value(output["candidates"].clone()).expect("valid candidates");
        assert_eq!(candidates.len(), 2);
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        // Task 8: the node reads a policy already resolved (event override
        // merged in) and stamped at dispatch — it no longer re-merges
        // `event.policy` itself, so this test stamps the *already-merged*
        // final policy directly rather than an `event.policy` override.
        let event = event_with_intake();
        let policy = ProposalGeneratorPolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ModelTiers {
                opportunity: ModelTier::Local,
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
                    text: serde_json::to_string(&stub_scored_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_scored_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = OpportunityIdentifierNode::new().with_transport(transport);
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        node.process(ctx).await.expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));
        assert!(prompt.contains("Be terse"));
        assert!(config.allowed_tools.is_empty());
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node = OpportunityIdentifierNode::new()
            .with_transport(stub_transport(Some(stub_scored_json())));
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
