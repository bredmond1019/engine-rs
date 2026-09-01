//! `ProposalReviseNode` — the `revise`-stage node that applies review notes
//! to produce a corrected `AutomationRoadmap`, then forwards to
//! `PersistToBrainNode`.
//!
//! A non-terminal, Local-eligible model node wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`. On `process`:
//! 1. read the run's [`super::policy::ProposalGeneratorPolicy`] stamped once
//!    at dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8)
//!    — no per-node re-resolution;
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

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::locale::{EngagementKind, Locale, MoneyRange, RateCard, RateSheet};
use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::policy::PolicyConfigSource;
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, ModelTransport, TransportSlot,
};

use super::policy::ProposalGeneratorPolicy;
use super::schema::{
    automation_roadmap_json_schema, AutomationRoadmap, ProposalGeneratorEventSchema,
};

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
/// also run standalone in a unit test). Reads both upstreams through their
/// [`InputBinding`]s (`EN.5.E` task 1/4) rather than a `NODE_NAME` const
/// imported from `super::writer`/`super::review` — an unbound binding falls
/// back to today's identities (`ProposalWriterNode`/`ProposalReviewNode`),
/// so a node that never calls `with_draft_input_from`/`with_review_input_from`
/// behaves exactly as before this primitive existed.
///
/// `locale` names the language the model must write ALL prose in — same
/// directive as `ProposalWriterNode::build_prompt`, in the per-run prompt
/// body only (CLAUDE.md rule 6, cache breakpoints; `STABLE_SYSTEM_PROMPT`
/// stays byte-identical across locales). The model is also told not to
/// author a price: `investment` is filled in deterministically from the
/// rate card after the reply parses (see `Node::process` below).
fn build_prompt(
    ctx: &TaskContext,
    event: &ProposalGeneratorEventSchema,
    draft_input: &InputBinding,
    review_input: &InputBinding,
    locale: Locale,
) -> String {
    let draft = get_result(ctx, draft_input.resolve("ProposalWriterNode"))
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(no upstream drafted roadmap available)".to_string());
    let notes = get_result(ctx, review_input.resolve("ProposalReviewNode"))
        .and_then(|value| value.get("notes"))
        .and_then(|value| value.as_str())
        .filter(|notes| !notes.is_empty())
        .unwrap_or("(no review notes available)");
    let language = locale.language_name();

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
         Original drafted roadmap:\n{draft}\n\n\
         Write ALL prose in {language}. This includes candidate names, \
         rationale, proposed_solution, how_it_works, and call_to_action. Do \
         not mix languages. Do NOT include a price, fee, or investment \
         figure anywhere in your reply — that value is filled in \
         deterministically after you respond.",
        company = event.company_name,
    )
}

/// Look up the [`MoneyRange`] for one [`EngagementKind`] on a [`RateSheet`].
/// Mirrors `writer::engagement_range` — kept local to each node rather than
/// a `RateSheet` method, since only the writer/revise nodes select a range
/// by engagement kind.
fn engagement_range(sheet: &RateSheet, kind: EngagementKind) -> MoneyRange {
    match kind {
        EngagementKind::Diagnostic => sheet.diagnostic,
        EngagementKind::Project => sheet.project,
        EngagementKind::Retainer => sheet.retainer,
    }
}

/// The `revise`-stage node that applies review notes to produce a
/// corrected `AutomationRoadmap`. Forwards to `PersistToBrainNode`.
pub struct ProposalReviseNode {
    config: Config,
    transport: TransportSlot,
    draft_input: InputBinding,
    review_input: InputBinding,
}

impl ProposalReviseNode {
    /// Construct with the `AutomationRoadmap` `json_schema` set; `process`
    /// overwrites `model` per the resolved `revise`-stage tier. Both
    /// upstream bindings start unbound — `EN.5.E` task 1's `InputBinding`
    /// default, which falls back to today's identities.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                json_schema: Some(automation_roadmap_json_schema()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            draft_input: InputBinding::default(),
            review_input: InputBinding::default(),
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

    /// Bind the identity this node reads its drafted-roadmap upstream from.
    /// Unbound falls back to `"ProposalWriterNode"` (today's default).
    #[must_use]
    pub fn with_draft_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.draft_input = InputBinding::bound(upstream);
        self
    }

    /// Bind the identity this node reads its review-notes upstream from.
    /// Unbound falls back to `"ProposalReviewNode"` (today's default).
    #[must_use]
    pub fn with_review_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.review_input = InputBinding::bound(upstream);
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
        let policy: ProposalGeneratorPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config =
            crate::policy::apply_model_tier(config, policy.model_tiers.revise, &policy.local.model);
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(
                &ctx,
                &event,
                &self.draft_input,
                &self.review_input,
                event.locale,
            ),
            policy.output_verbosity,
        );

        let step = self
            .transport
            .apply(ClaudeCodeStep::new(NODE_NAME, config, prompt));

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        // `put_result` below re-serializes the strict `AutomationRoadmap`
        // type, which would otherwise silently drop the `"transport"` stamp
        // `ClaudeCodeStep::process` just wrote — the exact tier-telemetry
        // `RunTelemetry`/`observed_model_tiers` (`policy/telemetry.rs`) reads
        // back out by this same node name.
        let transport_stamp = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("transport"))
            .cloned();

        let mut roadmap: AutomationRoadmap = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a corrected AutomationRoadmap from the \
                     model's reply: {err}"
                ))
            })?;

        // Deterministic stamp: the event's locale always wins over anything
        // the model emitted. Mirrors `ProposalWriterNode` — without this, a
        // run whose reviewer rejects the draft would produce a revised
        // roadmap with no locale stamp, and `PersistToBrainNode` prefers the
        // revised roadmap over the original draft.
        roadmap.authored_locale = event.locale;

        // Pricing is config, never model output — same rate-card lookup as
        // `ProposalWriterNode`. `PolicyConfigSource::Builtin` is correct
        // here: served runs resolve config at dispatch time (EN.5.D) and
        // this node has no worktree path.
        let rate_card = RateCard::load_from(&PolicyConfigSource::Builtin)?;
        if let Some(recommendation) = roadmap.recommendation.as_mut() {
            recommendation.investment = Some(engagement_range(
                rate_card.sheet(event.locale),
                EngagementKind::Project,
            ));
        }

        // Stamp the resolved locale into this node's ctx.nodes result so
        // RunTelemetry/PolicyAggregate can attribute observed cost/quality
        // to the locale that caused it (CLAUDE.md rule 6). `authored_locale`
        // is that stamp: it lives on `AutomationRoadmap` itself (set above,
        // deterministically, from the event) rather than as a sibling key,
        // so this node's `ctx.nodes` entry stays byte-identical to the
        // `AutomationRoadmap` the roadmap round-trips through downstream
        // (`PersistToBrainNode` re-serializes the strict schema type, which
        // would silently drop any extra sibling key and break that
        // round-trip).
        let mut result = serde_json::to_value(&roadmap).map_err(|err| {
            NodeError::new(format!(
                "failed to serialize corrected AutomationRoadmap: {err}"
            ))
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

    use super::super::policy::ModelTier;
    use super::super::schema::validate_automation_roadmap;
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
                    session_id: None,
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
                    session_id: None,
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
            "ProposalWriterNode".to_string(),
            json!({ "situation": { "company_name": "Loja da Ana" } }),
        );
        ctx.nodes.insert(
            "ProposalReviewNode".to_string(),
            json!({ "verdict": "revise", "notes": "Composite math is off for candidate 1." }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Composite math is off for candidate 1."));
        assert!(prompt.contains("Loja da Ana"));
    }

    #[tokio::test]
    async fn process_reads_bound_upstreams_via_with_input_from() {
        // EN.5.E task 4: with the node built `with_draft_input_from`/
        // `with_review_input_from`, it reads its draft/review notes off the
        // *bound* identities rather than the default `ProposalWriterNode`/
        // `ProposalReviewNode` — proving no cross-module `NODE_NAME` const
        // is needed to wire this up.
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
                    session_id: None,
                    structured_output: Some(corrected_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviseNode::new()
            .with_transport(transport)
            .with_draft_input_from("CustomDraftNode")
            .with_review_input_from("CustomReviewNode");
        let mut ctx = empty_ctx(base_event());
        ctx.nodes.insert(
            "CustomDraftNode".to_string(),
            json!({ "situation": { "company_name": "Loja da Ana" } }),
        );
        ctx.nodes.insert(
            "CustomReviewNode".to_string(),
            json!({ "verdict": "revise", "notes": "Bound review notes surfaced." }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains("Bound review notes surfaced."));
        assert!(prompt.contains("Loja da Ana"));
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
                revise: ModelTier::Local,
                ..ProposalGeneratorPolicy::default().model_tiers
            },
            local: super::super::policy::LocalConfig {
                model: "qwen2.5-coder:7b".to_string(),
                ..super::super::policy::LocalConfig::default()
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
                    text: serde_json::to_string(&corrected_roadmap_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    session_id: None,
                    structured_output: Some(corrected_roadmap_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProposalReviseNode::new().with_transport(transport);
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
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

    // --- Locale + rate card (EN.4.F task 5) -------------------------------

    fn event_with_locale(locale: Locale) -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            locale,
            ..base_event()
        }
    }

    #[tokio::test]
    async fn stable_system_prompt_is_byte_identical_across_locales() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for locale in [Locale::PtBr, Locale::EnUs] {
            let node = ProposalReviseNode::new()
                .with_transport(stub_transport(Some(corrected_roadmap_json())));
            let ctx = empty_ctx(event_with_locale(locale));
            node.process(ctx).await.expect("process should succeed");
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    #[tokio::test]
    async fn revised_roadmap_keeps_the_event_locale() {
        let node = ProposalReviseNode::new()
            .with_transport(stub_transport(Some(corrected_roadmap_json())));
        let ctx = empty_ctx(event_with_locale(Locale::EnUs));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        assert_eq!(roadmap.authored_locale, Locale::EnUs);
    }

    #[tokio::test]
    async fn revised_roadmap_is_priced_from_the_rate_card() {
        let node = ProposalReviseNode::new()
            .with_transport(stub_transport(Some(corrected_roadmap_json())));
        let ctx = empty_ctx(event_with_locale(Locale::PtBr));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let roadmap: AutomationRoadmap =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid AutomationRoadmap");
        let investment = roadmap
            .recommendation
            .expect("recommendation present")
            .investment
            .expect("investment populated");
        let expected = crate::locale::RateCard::default()
            .sheet(Locale::PtBr)
            .project;
        assert_eq!(investment.currency, crate::locale::Currency::Brl);
        assert_eq!(investment.min, expected.min);
        assert_eq!(investment.max, expected.max);
    }
}
