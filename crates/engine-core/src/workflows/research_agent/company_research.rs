//! `CompanyResearchNode` — the single-company brief terminal node — filled
//! in task 5. Re-exported from `research_agent::mod` for `EN.4.C` reuse.
//!
//! A terminal model node (no forward connection) wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`, ported from
//! `orchestrator`'s RESEARCH_AGENT company-brief mode and broadened onto the
//! EN.4.0 policy framework. On `process`:
//! 1. read the run's [`super::policy::ResearchAgentPolicy`] stamped once at
//!    dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8) —
//!    no per-node re-resolution;
//! 2. apply `research`-stage shaping (model tier, prompt cache, verbosity
//!    directive) to the composed `Config`;
//! 3. await the (injectable) transport and parse its reply into a
//!    [`super::schema::CompanyBrief`];
//! 4. stamp the parsed brief + usage onto `ctx` (via the composed
//!    `ClaudeCodeStep`) and harvest + persist a `research-agent-state.json`
//!    telemetry record;
//! 5. return `ctx` unchanged otherwise — this node is a graph exit point.

use std::path::{Path, PathBuf};

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::json;

use crate::locale::{language_directive, Locale};
use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::policy::telemetry::RunTelemetryInputs;
use crate::workflows::{
    get_result, parse_structured_or_fenced, put_result, session_baseline, sessions_since,
    ModelTransport,
};

use super::policy::{ContactDepth, GroundingDepth, ModelTier, ResearchAgentPolicy};
use super::schema::{company_brief_json_schema, CompanyBrief, ResearchAgentEventSchema};

/// The `Node::name()` identity `CompanyResearchNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto.
const NODE_NAME: &str = "CompanyResearchNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true — mirrors
/// `sdlc_flow::task_loop::STABLE_SYSTEM_PROMPT`'s rationale: keeping this
/// byte-identical across calls gives the underlying `claude` CLI a stable
/// prefix to cache against.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs RESEARCH_AGENT workflow, company-brief \
     mode. This system prompt is held constant across calls so its tokens can be \
     cached.";

/// The verdict-bearing model-judgment stages this node's telemetry snapshot
/// inspects. `CompanyResearchNode` has no downstream review stage — the
/// composed `ClaudeCodeStep` run under [`NODE_NAME`] is the only one.
const VERDICT_STAGES: [&str; 0] = [];

/// The model-node identities whose `ctx.nodes` output may carry a
/// `"cost_usd"` field (`ClaudeCodeStep`'s output shape).
const COST_BEARING_STAGES: [&str; 1] = [NODE_NAME];

/// Deserialize the inbound `RESEARCH_AGENT` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ResearchAgentEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid RESEARCH_AGENT event: {err}")))
}

/// Build the contact-acquisition + extraction directive block appended to
/// the base research prompt, shaped by the resolved `depth` (and, at
/// non-`Off` depths, the `max_fetches` budget). Returns an empty string at
/// [`ContactDepth::Off`] so the run behaves exactly as it did before
/// `EN.4.E` — no contact directive at all.
///
/// The two halves (acquisition, extraction) compose rather than compete:
/// effort spent *looking* carries no fabrication risk — only the
/// *reporting* step does. Both non-`Off` depths therefore direct the model
/// to search hard and report only what it saw verbatim.
fn contact_directive(depth: ContactDepth, max_fetches: u8) -> String {
    match depth {
        ContactDepth::Off => String::new(),
        ContactDepth::Standard => format!(
            "\n\nACQUISITION — reachable contacts (budget: up to {max_fetches} extra page \
             loads beyond what you've already fetched; self-limit to this budget). Visit the \
             company's own contact-bearing surfaces: its contact/about/team (or \"quem somos\") \
             page, the site footer and header, and any `mailto:` or `wa.me`/`api.whatsapp.com` \
             links in the page source. For a Brazilian small/mid-size business the reachable \
             channel is often a WhatsApp number or an Instagram handle rather than a corporate \
             email — do not fixate on email.\n\n\
             EXTRACTION — report every reachable channel you found under `contacts`. A generic \
             channel with no named individual (e.g. `contato@company.example`, a storefront \
             WhatsApp number) is still a valid contact — record it with an empty `name` rather \
             than discarding it.\n\n\
             ANTI-FABRICATION (mandatory): only report a contact channel that appears verbatim \
             in a page you actually fetched. Never construct an email, phone number, or handle \
             from the company's domain or a person's name. An empty `contacts` list is the \
             correct, expected answer when nothing reachable was found — say so in `note` or the \
             summary rather than guessing. Acquisition and anti-fabrication compose, they do not \
             compete: search hard, report only what you saw."
        ),
        ContactDepth::Deep => format!(
            "\n\nACQUISITION — reachable contacts (budget: up to {max_fetches} extra page \
             loads beyond what you've already fetched; self-limit to this budget). Visit the \
             company's own contact-bearing surfaces: its contact/about/team (or \"quem somos\") \
             page, the site footer and header, and any `mailto:` or `wa.me`/`api.whatsapp.com` \
             links in the page source. Also check the company's public LinkedIn, Instagram, and \
             Facebook profiles, and look for a named decision-maker who is publicly identified \
             (owner, founder, s\u{f3}cio, head of ops) — a named human is worth far more than \
             a generic inbox. For a Brazilian small/mid-size business the reachable channel is \
             often a WhatsApp number or an Instagram handle rather than a corporate email — do \
             not fixate on email.\n\n\
             EXTRACTION — report every reachable channel you found under `contacts`. A generic \
             channel with no named individual (e.g. `contato@company.example`, a storefront \
             WhatsApp number) is still a valid contact — record it with an empty `name` rather \
             than discarding it.\n\n\
             ANTI-FABRICATION (mandatory): only report a contact channel that appears verbatim \
             in a page you actually fetched. Never construct an email, phone number, or handle \
             from the company's domain or a person's name. An empty `contacts` list is the \
             correct, expected answer when nothing reachable was found — say so in `note` or the \
             summary rather than guessing. Acquisition and anti-fabrication compose, they do not \
             compete: search hard, report only what you saw."
        ),
    }
}

/// Build the grounding directive appended to the base research prompt,
/// shaped by the resolved `grounding.research` depth (`EN.4.G`). Unlike
/// [`contact_directive`] there is no `off` case — both variants of
/// [`GroundingDepth`] always append a directive; `harness.json` documents
/// why the knob has no floor below `standard` (brand.md Rule 5 is an
/// external contract, CLAUDE.md rule 6's own qualifier, and `EN.6.H1` reads
/// `needs_further_research` unconditionally).
///
/// Both depths instruct the model to list, under `needs_further_research`,
/// every domain-specific claim — a regulatory/compliance regime (the
/// carryover's own FAR/DFARS example), a certification, a jurisdiction-
/// specific rule (the carryover's own Brazilian local-LLM data-residency
/// example), a numeric figure, or a capability claim — that it could not
/// tie to a source it actually fetched. Flagging is not failure: the claim
/// is kept, not deleted, and "I could not ground this" is a correct,
/// expected answer for a fully-grounded brief (mirrors the `contacts`
/// omission-over-guess framing). `strict` additionally demands a per-claim
/// grounding pass over `pain_points` and `outreach_hooks`, with a stated
/// reason recorded for each flagged claim.
fn grounding_directive(depth: GroundingDepth) -> String {
    match depth {
        GroundingDepth::Standard => "\n\n\
             GROUNDING — flag ungrounded domain claims. Before you finish, review every \
             domain-specific claim in your draft `summary`, `recent_developments`, \
             `pain_points`, and `outreach_hooks`: a regulatory or compliance-regime claim \
             (e.g. FAR/DFARS-style federal compliance obligations), a certification claim, a \
             jurisdiction-specific rule (e.g. a Brazilian data-residency requirement for local \
             LLM deployment), a numeric figure, or a capability claim. For each one you cannot \
             tie to a source you actually fetched, add a short description of the claim to \
             `needs_further_research`. This is not a failure state: keep the claim where it is \
             and flag it rather than deleting it — an empty `needs_further_research` list is \
             the correct, expected answer for a fully-grounded brief."
            .to_string(),
        GroundingDepth::Strict => "\n\n\
             GROUNDING — flag ungrounded domain claims (strict). Before you finish, review \
             every domain-specific claim in your draft `summary`, `recent_developments`, \
             `pain_points`, and `outreach_hooks`: a regulatory or compliance-regime claim \
             (e.g. FAR/DFARS-style federal compliance obligations), a certification claim, a \
             jurisdiction-specific rule (e.g. a Brazilian data-residency requirement for local \
             LLM deployment), a numeric figure, or a capability claim. For each one you cannot \
             tie to a source you actually fetched, add a short description of the claim to \
             `needs_further_research`. This is not a failure state: keep the claim where it is \
             and flag it rather than deleting it — an empty `needs_further_research` list is \
             the correct, expected answer for a fully-grounded brief.\n\n\
             At this depth, additionally run a dedicated per-claim grounding pass over every \
             item in `pain_points` and `outreach_hooks`: for each one, state internally why you \
             believe it is grounded (which fetched source supports it) or flag it with a stated \
             reason in `needs_further_research`. Do not skip this pass for claims that merely \
             sound plausible."
            .to_string(),
    }
}

/// Build the single-company research-brief prompt from the triggering
/// event, shaped by the resolved `contact_enrichment.research` depth +
/// `max_fetches` (task 3's policy knob — resolved once by the caller, never
/// re-resolved here) and the resolved `grounding.research` depth
/// (`EN.4.G`). Ports `orchestrator`'s RESEARCH_AGENT company-brief
/// prompt, framed against this practice's own positioning (business/docs
/// `brand.md`/`services.md`) so the model's pain-point/outreach-hook
/// suggestions map back onto real, sellable services rather than generic
/// advice.
fn build_prompt(
    event: &ResearchAgentEventSchema,
    depth: ContactDepth,
    max_fetches: u8,
    grounding_depth: GroundingDepth,
    locale: Locale,
) -> String {
    let company_name = event.company_name.as_deref().unwrap_or("the company");
    let company_url = event
        .company_url
        .as_deref()
        .map(|url| format!(" ({url})"))
        .unwrap_or_default();

    let base = format!(
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
         (cite the source URL for each).\n\n\
         Respond with strict JSON matching this shape: {{\"company_name\": \
         str, \"summary\": str, \"recent_developments\": [str], \
         \"pain_points\": [str], \"outreach_hooks\": [str], \"sources\": \
         [str], \"contacts\": [{{\"name\": str, \"role\": str, \"emails\": \
         [str], \"whatsapp\": [str], \"phones\": [str], \"links\": [str], \
         \"note\": str}}], \"needs_further_research\": [str]}}."
    );

    format!(
        "{base}{}{}\n\n{}",
        contact_directive(depth, max_fetches),
        grounding_directive(grounding_depth),
        language_directive(locale)
    )
}

/// Read the worktree path stamped by an upstream setup node, if this run
/// went through one. `RESEARCH_AGENT` has no dedicated setup node, so this
/// is best-effort — used only by [`persist_state`] to locate
/// `planning/research-agent-state.json`, not for policy resolution (task
/// 8's stamped-policy read needs no path at all): absent, [`persist_state`]
/// falls back to the process's current working directory, and a ctx driven
/// directly in a unit test (no upstream node at all) still resolves *some*
/// path rather than failing `process`.
fn worktree_path(ctx: &TaskContext) -> PathBuf {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Persist `record` (a `{mode, policy, telemetry}` snapshot) to
/// `planning/research-agent-state.json` inside `worktree` — the file the
/// `#[ignore]`-gated experiment harness (task 8) aggregates via
/// `crate::policy::aggregate_state_files`.
fn persist_state(
    worktree: &Path,
    mode: &str,
    policy: &ResearchAgentPolicy,
    telemetry: &crate::policy::RunTelemetry,
) -> Result<String, NodeError> {
    let state_dir = worktree.join("planning");
    std::fs::create_dir_all(&state_dir).map_err(|err| {
        NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
    })?;
    let state_path = state_dir.join("research-agent-state.json");
    let record = json!({
        "mode": mode,
        "policy": policy,
        "telemetry": telemetry,
    });
    let content = serde_json::to_string_pretty(&record)
        .map_err(|err| NodeError::new(format!("failed to serialize state record: {err}")))?;
    std::fs::write(&state_path, content).map_err(|err| {
        NodeError::new(format!("failed to write {}: {err}", state_path.display()))
    })?;
    Ok(state_path.to_string_lossy().to_string())
}

/// The single-company brief terminal node of the `RESEARCH_AGENT` workflow.
pub struct CompanyResearchNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl CompanyResearchNode {
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

impl Default for CompanyResearchNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for CompanyResearchNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let worktree = worktree_path(&ctx);

        let policy: ResearchAgentPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.research,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let contact_depth = policy.contact_enrichment.research;
        let max_fetches = policy.contact_enrichment.max_fetches;
        let grounding_depth = policy.grounding.research;
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(
                &event,
                contact_depth,
                max_fetches,
                grounding_depth,
                event.locale,
            ),
            policy.output_verbosity,
        );

        let mut step = ClaudeCodeStep::new(NODE_NAME, config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        // Baseline taken immediately before the billed call, per EN.14.C —
        // any wrapper `Err` returned below carries whatever the inner
        // `ClaudeCodeStep` appended to the ledger, so this research wrapper's
        // billed session survives a post-billed-call failure.
        let baseline = session_baseline(&ctx);
        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let mut brief: CompanyBrief = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a CompanyBrief from the model's reply: {err}"
                ))
                .with_sessions(sessions_since(&ctx, baseline))
            })?;

        // Deterministic stamp: an explicitly-supplied trigger `company_url`
        // always wins over the model's own value (present or absent) — the
        // field must not be solely model-dependent.
        if let Some(company_url) = event.company_url.as_ref() {
            brief.company_url = Some(company_url.clone());
        }

        let mut brief_value = serde_json::to_value(&brief).map_err(|err| {
            NodeError::new(format!("failed to serialize CompanyBrief: {err}"))
                .with_sessions(sessions_since(&ctx, baseline))
        })?;
        // Stamp the resolved contact-enrichment depth alongside the brief so
        // EN.4.0 telemetry can attribute cost to the setting that caused it.
        // `CompanyBrief` ignores unknown fields on deserialize, so this is a
        // transparent addition to the node's result object.
        if let Some(obj) = brief_value.as_object_mut() {
            obj.insert(
                "contact_enrichment_depth".to_string(),
                serde_json::to_value(contact_depth).unwrap_or_default(),
            );
            // Stamp the resolved locale alongside the brief so EN.4.0
            // telemetry can attribute prose-language cost/quality to the
            // locale that caused it (CLAUDE.md rule 6).
            obj.insert(
                "locale".to_string(),
                serde_json::to_value(event.locale).unwrap_or_default(),
            );
            // Stamp the resolved grounding depth so EN.4.0 telemetry can
            // attribute cost to the setting that caused it (EN.4.G).
            obj.insert(
                "grounding_depth".to_string(),
                serde_json::to_value(grounding_depth).unwrap_or_default(),
            );
            // `validation_required` is derived, never read from the
            // model's reply — a stubbed or adversarial reply that sets
            // `validation_required: false` alongside a non-empty
            // `needs_further_research` list must not be trusted (design
            // call 1, tasks.md).
            obj.insert(
                "validation_required".to_string(),
                serde_json::Value::Bool(!brief.needs_further_research.is_empty()),
            );
        }
        put_result(&mut ctx, NODE_NAME, brief_value);

        let model_tier_used = std::collections::BTreeMap::from([(
            "research".to_string(),
            tier_str(policy.model_tiers.research),
        )]);
        let inputs = RunTelemetryInputs {
            start_node_identity: NODE_NAME,
            verdict_stages: &VERDICT_STAGES,
            cost_bearing_stages: &COST_BEARING_STAGES,
            total_attempts: 1,
            total_retries: 0,
            tasks_passed: 1,
            tasks_failed: 0,
            model_tier_used,
            model_stages: &COST_BEARING_STAGES,
        };
        let telemetry = crate::policy::harvest_telemetry(&ctx, chrono::Utc::now(), inputs);
        persist_state(&worktree, "company", &policy, &telemetry)
            .map_err(|err| err.with_sessions(sessions_since(&ctx, baseline)))?;

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

fn tier_str(tier: ModelTier) -> String {
    serde_json::to_value(tier)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
    use claude_code_rs::Outcome;
    use futures::FutureExt;

    use super::super::schema::ResearchMode;
    use super::*;

    /// Builds a `ctx` and stamps a default [`ResearchAgentPolicy`] under
    /// `RESOLVED_POLICY_IDENTITY` — required since task 8's strict
    /// `resolved_policy_strict` read (no more per-node re-resolution or a
    /// silent `Default` fallback).
    fn empty_ctx(event: ResearchAgentEventSchema) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(ResearchAgentPolicy::default()).expect("policy serializes"),
        );
        ctx
    }

    fn company_event() -> ResearchAgentEventSchema {
        ResearchAgentEventSchema {
            mode: ResearchMode::Company,
            company_name: Some("Acme Corp".to_string()),
            company_url: Some("https://acme.example".to_string()),
            vertical: None,
            topic: None,
            locale: crate::locale::Locale::default(),
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
                    session_id: None,
                    structured_output: structured,
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-company-research-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn process_populates_company_brief_and_usage() {
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

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
        let node = CompanyResearchNode::new().with_transport(stub_transport(None));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

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
        let policy = ResearchAgentPolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ResearchAgentModelTiers {
                research: ModelTier::Opus,
                ..ResearchAgentPolicy::default().model_tiers
            },
            ..ResearchAgentPolicy::default()
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
                    session_id: None,
                    structured_output: Some(stub_brief_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = CompanyResearchNode::new().with_transport(transport);
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );
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
    async fn process_writes_research_agent_state_json() {
        let worktree = temp_worktree();
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        node.process(ctx).await.expect("process should succeed");

        let state_path = worktree.join("planning").join("research-agent-state.json");
        assert!(state_path.exists());
        let content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mode"], "company");
        assert_eq!(parsed["policy"]["model_tiers"]["research"], "sonnet");
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let ctx = TaskContext {
            event: json!({ "mode": "not-a-real-mode" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid RESEARCH_AGENT event"));
    }

    // --- Task 4: policy-driven contact acquisition + company_url stamping ---

    #[test]
    fn off_depth_prompt_has_no_contact_directive() {
        let prompt = build_prompt(
            &company_event(),
            ContactDepth::Off,
            4,
            GroundingDepth::Standard,
            Locale::PtBr,
        );
        assert!(!prompt.to_lowercase().contains("acquisition"));
        assert!(!prompt.to_lowercase().contains("anti-fabrication"));
    }

    #[test]
    fn standard_depth_names_contact_surfaces_and_fetch_budget() {
        let prompt = build_prompt(
            &company_event(),
            ContactDepth::Standard,
            4,
            GroundingDepth::Standard,
            Locale::PtBr,
        );
        assert!(prompt.contains("ACQUISITION"));
        assert!(prompt.contains("contact/about/team"));
        assert!(prompt.contains("mailto:"));
        assert!(prompt.contains("wa.me"));
        assert!(prompt.contains("up to 4 extra page loads"));
        // Standard does not add the social-profile/decision-maker sweep.
        assert!(!prompt.contains("LinkedIn"));
    }

    #[test]
    fn deep_depth_adds_social_profiles_and_decision_maker_ask() {
        let prompt = build_prompt(
            &company_event(),
            ContactDepth::Deep,
            8,
            GroundingDepth::Standard,
            Locale::PtBr,
        );
        assert!(prompt.contains("LinkedIn"));
        assert!(prompt.contains("Instagram"));
        assert!(prompt.contains("Facebook"));
        assert!(prompt.contains("decision-maker"));
        assert!(prompt.contains("up to 8 extra page loads"));
        // Deep still carries everything Standard has.
        assert!(prompt.contains("contact/about/team"));
        assert!(prompt.contains("mailto:"));
    }

    #[test]
    fn non_off_depths_carry_omission_over_guess_directive() {
        for depth in [ContactDepth::Standard, ContactDepth::Deep] {
            let prompt = build_prompt(
                &company_event(),
                depth,
                4,
                GroundingDepth::Standard,
                Locale::PtBr,
            );
            assert!(
                prompt.contains("Never construct"),
                "depth {depth:?} missing the anti-fabrication directive"
            );
            assert!(
                prompt.contains("empty `contacts` list is the"),
                "depth {depth:?} missing the omission-over-guess directive"
            );
            assert!(
                prompt.contains("do not compete"),
                "depth {depth:?} missing the composing-not-competing framing"
            );
        }
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_all_depths() {
        // STABLE_SYSTEM_PROMPT itself never varies — it's a `const`, not a
        // function of depth — but assert this explicitly so a future edit
        // that tries to make it policy-varying trips a test rather than
        // silently defeating the cache breakpoint.
        let anchor = STABLE_SYSTEM_PROMPT;
        for depth in [
            ContactDepth::Off,
            ContactDepth::Standard,
            ContactDepth::Deep,
        ] {
            let _ = build_prompt(
                &company_event(),
                depth,
                4,
                GroundingDepth::Standard,
                Locale::PtBr,
            );
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    // --- Task 6: locale-aware prose directive -----------------------------

    #[test]
    fn prompt_body_names_the_event_locale_language() {
        let pt_prompt = build_prompt(
            &company_event(),
            ContactDepth::Off,
            4,
            GroundingDepth::Standard,
            Locale::PtBr,
        );
        assert!(pt_prompt.contains("Brazilian Portuguese"));
        let en_prompt = build_prompt(
            &company_event(),
            ContactDepth::Off,
            4,
            GroundingDepth::Standard,
            Locale::EnUs,
        );
        assert!(en_prompt.contains("English (en-US)"));
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_locales() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for locale in [Locale::PtBr, Locale::EnUs] {
            let _ = build_prompt(
                &company_event(),
                ContactDepth::Standard,
                4,
                GroundingDepth::Standard,
                locale,
            );
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    #[test]
    fn no_prompt_synthesizes_a_contact_channel() {
        for depth in [ContactDepth::Standard, ContactDepth::Deep] {
            let prompt = build_prompt(
                &company_event(),
                depth,
                4,
                GroundingDepth::Standard,
                Locale::PtBr,
            );
            assert!(!prompt.to_lowercase().contains("info@{domain}"));
            assert!(!prompt.to_lowercase().contains("guess an address"));
        }
    }

    #[tokio::test]
    async fn stub_transport_with_contact_populates_brief_contacts() {
        let mut brief_json = stub_brief_json();
        brief_json["contacts"] = json!([{
            "name": "",
            "role": "",
            "emails": ["contato@acme.example"],
            "whatsapp": [],
            "phones": [],
            "links": [],
            "note": "Generic inbox found on contact page",
        }]);

        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.contacts.len(), 1);
        assert_eq!(brief.contacts[0].emails, vec!["contato@acme.example"]);
        assert_eq!(brief.contacts[0].name, "");
    }

    #[tokio::test]
    async fn brief_with_no_contacts_key_yields_empty_vec_and_succeeds() {
        // stub_brief_json() carries no "contacts" key at all.
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.contacts, Vec::new());
    }

    #[tokio::test]
    async fn company_url_from_event_lands_on_brief_when_model_omits_it() {
        // stub_brief_json() carries no "company_url" key.
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.company_url, Some("https://acme.example".to_string()));
    }

    #[tokio::test]
    async fn company_url_from_event_overrides_a_different_model_value() {
        let mut brief_json = stub_brief_json();
        brief_json["company_url"] = json!("https://model-guessed-wrong.example");

        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        // The trigger event's company_url always wins.
        assert_eq!(brief.company_url, Some("https://acme.example".to_string()));
    }

    #[tokio::test]
    async fn event_without_company_url_leaves_models_value_intact() {
        let mut event = company_event();
        event.company_url = None;
        let mut brief_json = stub_brief_json();
        brief_json["company_url"] = json!("https://model-found-this.example");

        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(
            brief.company_url,
            Some("https://model-found-this.example".to_string())
        );
    }

    #[tokio::test]
    async fn event_without_company_url_and_model_omitting_it_leaves_none() {
        let mut event = company_event();
        event.company_url = None;
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.company_url, None);
    }

    #[tokio::test]
    async fn resolved_contact_enrichment_depth_is_stamped_into_the_result() {
        let policy = ResearchAgentPolicy {
            contact_enrichment: super::super::policy::ContactEnrichment {
                research: ContactDepth::Deep,
                ..ResearchAgentPolicy::default().contact_enrichment
            },
            ..ResearchAgentPolicy::default()
        };
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(
            ctx.nodes[NODE_NAME]["contact_enrichment_depth"],
            json!("deep")
        );
    }

    #[tokio::test]
    async fn resolved_locale_is_stamped_into_the_result() {
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut event = company_event();
        event.locale = Locale::EnUs;
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["locale"], json!("en-US"));
    }

    #[tokio::test]
    async fn zero_contact_model_response_is_a_success_path() {
        // A brief that explicitly returns an empty contacts array must
        // still complete `process` successfully — "none found" is not a
        // failure.
        let mut brief_json = stub_brief_json();
        brief_json["contacts"] = json!([]);
        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let brief: CompanyBrief =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CompanyBrief");
        assert_eq!(brief.contacts, Vec::new());
    }

    // --- Task 3 (EN.4.G): grounding directive + derived validation_required ---

    #[test]
    fn grounding_directive_text_differs_between_depths() {
        let standard = grounding_directive(GroundingDepth::Standard);
        let strict = grounding_directive(GroundingDepth::Strict);
        assert_ne!(standard, strict);
        assert!(strict.contains("per-claim grounding pass"));
        assert!(!standard.contains("per-claim grounding pass"));
    }

    #[test]
    fn both_grounding_depths_name_the_carryover_examples_and_anti_deletion_framing() {
        for depth in [GroundingDepth::Standard, GroundingDepth::Strict] {
            let directive = grounding_directive(depth);
            assert!(
                directive.contains("FAR/DFARS"),
                "depth {depth:?} missing the FAR/DFARS example"
            );
            assert!(
                directive.contains("Brazilian data-residency"),
                "depth {depth:?} missing the Brazilian data-residency example"
            );
            assert!(
                directive.contains("not a failure state"),
                "depth {depth:?} missing the anti-deletion framing"
            );
            assert!(
                directive.contains("needs_further_research"),
                "depth {depth:?} missing the field name"
            );
        }
    }

    #[test]
    fn strict_grounding_adds_a_dedicated_pass_over_pain_points_and_outreach_hooks() {
        let strict = grounding_directive(GroundingDepth::Strict);
        assert!(strict.contains("pain_points"));
        assert!(strict.contains("outreach_hooks"));
        assert!(strict.contains("stated reason"));
    }

    #[test]
    fn prompt_json_shape_line_carries_needs_further_research() {
        let prompt = build_prompt(
            &company_event(),
            ContactDepth::Off,
            4,
            GroundingDepth::Standard,
            Locale::PtBr,
        );
        assert!(prompt.contains("\"needs_further_research\": [str]"));
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_grounding_depths() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for depth in [GroundingDepth::Standard, GroundingDepth::Strict] {
            let _ = build_prompt(&company_event(), ContactDepth::Off, 4, depth, Locale::PtBr);
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    fn stub_brief_json_with_flags(flags: Vec<&str>) -> serde_json::Value {
        let mut value = stub_brief_json();
        value["needs_further_research"] =
            json!(flags.into_iter().map(str::to_string).collect::<Vec<_>>());
        value
    }

    #[tokio::test]
    async fn resolved_grounding_depth_is_stamped_into_the_result() {
        let policy = ResearchAgentPolicy {
            grounding: super::super::policy::Grounding {
                research: GroundingDepth::Strict,
                ..ResearchAgentPolicy::default().grounding
            },
            ..ResearchAgentPolicy::default()
        };
        let node =
            CompanyResearchNode::new().with_transport(stub_transport(Some(stub_brief_json())));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["grounding_depth"], json!("strict"));
    }

    #[tokio::test]
    async fn validation_required_is_true_when_a_claim_is_flagged() {
        let brief_json = stub_brief_json_with_flags(vec!["FAR/DFARS compliance unconfirmed"]);
        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["validation_required"], json!(true));
        assert_eq!(
            ctx.nodes[NODE_NAME]["needs_further_research"],
            json!(["FAR/DFARS compliance unconfirmed"])
        );
    }

    #[tokio::test]
    async fn validation_required_is_false_when_no_claim_is_flagged() {
        let brief_json = stub_brief_json_with_flags(vec![]);
        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["validation_required"], json!(false));
        assert_eq!(ctx.nodes[NODE_NAME]["needs_further_research"], json!([]));
    }

    #[tokio::test]
    async fn validation_required_is_always_derived_never_trusted_from_the_model() {
        // A stubbed reply that itself claims `validation_required: false`
        // alongside a non-empty `needs_further_research` list must still be
        // stamped `true` — the field is computed, never read from the
        // model's reply (design call 1, tasks.md).
        let mut brief_json = stub_brief_json_with_flags(vec!["unverified numeric claim"]);
        brief_json["validation_required"] = json!(false);
        let node = CompanyResearchNode::new().with_transport(stub_transport(Some(brief_json)));
        let mut ctx = empty_ctx(company_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["validation_required"], json!(true));
    }
}
