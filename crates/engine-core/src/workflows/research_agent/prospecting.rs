//! `ProspectingResearchNode` — the prospecting-sweep terminal node — filled
//! in task 6.
//!
//! A terminal model node (no forward connection) wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`, ported from
//! `orchestrator`'s reddit-prospecting-inspired research flow and broadened
//! onto the EN.4.0 policy framework. On `process`:
//! 1. read the run's [`super::policy::ResearchAgentPolicy`] stamped once at
//!    dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8) —
//!    no per-node re-resolution;
//! 2. apply `prospect`-stage shaping (model tier, prompt cache, verbosity
//!    directive) to the composed `Config`;
//! 3. await the (injectable) transport and parse its reply into a
//!    [`super::schema::ProspectingResult`];
//! 4. stamp the parsed result + usage onto `ctx` (via the composed
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
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::{ContactDepth, ModelTier, ResearchAgentPolicy};
use super::schema::{prospecting_result_json_schema, ProspectingResult, ResearchAgentEventSchema};

/// The `Node::name()` identity `ProspectingResearchNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto.
const NODE_NAME: &str = "ProspectingResearchNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true — mirrors
/// `sdlc_flow::task_loop::STABLE_SYSTEM_PROMPT`'s rationale: keeping this
/// byte-identical across calls gives the underlying `claude` CLI a stable
/// prefix to cache against.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs RESEARCH_AGENT workflow, prospecting \
     mode. This system prompt is held constant across calls so its tokens can be \
     cached.";

/// The verdict-bearing model-judgment stages this node's telemetry snapshot
/// inspects. `ProspectingResearchNode` has no downstream review stage — the
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

/// Build the per-prospect contact-acquisition + extraction directive block
/// appended to the base prospecting prompt, shaped by the resolved `depth`
/// (and, at non-`Off` depths, the `max_fetches` budget). Returns an empty
/// string at [`ContactDepth::Off`] so the run behaves exactly as it did
/// before `EN.4.E` — no contact directive, no per-prospect fetches.
///
/// Calibrated to this mode, not copy-pasted from company mode: breadth of
/// prospects matters more than depth of contact here (company mode is
/// where deep enrichment belongs), so both non-`Off` depths cap the spend
/// at **one cheap attempt per identifiable business** and explicitly tell
/// the model to skip pseudonymous individuals rather than burn fetches
/// chasing them. `Deep` is permitted but deliberately not reached by any
/// built-in profile (`thorough` pins prospecting at `Standard`) — treat it
/// as `Standard` plus the public-profile sweep, with `max_fetches` still
/// the authoritative per-run cap.
fn contact_directive(depth: ContactDepth, max_fetches: u8) -> String {
    match depth {
        ContactDepth::Off => String::new(),
        ContactDepth::Standard => format!(
            "\n\nACQUISITION — reachable contacts, one attempt per identifiable business \
             (total run budget: up to {max_fetches} extra page loads beyond what you've \
             already fetched; self-limit to this budget across the whole sweep, not per \
             prospect). Breadth beats depth here: prioritize surfacing more prospects over \
             chasing contact detail on any one of them. For a prospect that is an \
             identifiable business (has a named company, storefront, or public profile you \
             can attribute to an operating entity), spend at most one cheap look at its \
             public profile page or its site's contact/footer surface. Explicitly SKIP \
             pseudonymous individuals — a forum handle, a personal account with no business \
             attached — do not spend fetches chasing them; a pseudonymous poster with no \
             contact is a normal, correct result, not a gap to fill.\n\n\
             EXTRACTION — report every reachable channel you found under that prospect's \
             `contacts`. A generic channel with no named individual (e.g. \
             `contato@company.example`, a storefront WhatsApp number) is still a valid \
             contact — record it with an empty `name` rather than discarding it.\n\n\
             ANTI-FABRICATION (mandatory): only report a contact channel that appears \
             verbatim in a page you actually fetched. Never construct an email, phone \
             number, or handle from the company's domain or a person's name. Most leads \
             will legitimately have no contacts — an empty `contacts` list is the correct, \
             expected answer for the majority of prospects; do not pressure yourself toward \
             filling the field. Acquisition and anti-fabrication compose, they do not \
             compete: search hard where it's cheap, report only what you saw."
        ),
        ContactDepth::Deep => format!(
            "\n\nACQUISITION — reachable contacts, one attempt per identifiable business \
             plus a public-profile sweep (total run budget: up to {max_fetches} extra page \
             loads beyond what you've already fetched; self-limit to this budget across the \
             whole sweep, not per prospect). Breadth beats depth here: prioritize surfacing \
             more prospects over chasing contact detail on any one of them. For a prospect \
             that is an identifiable business (has a named company, storefront, or public \
             profile you can attribute to an operating entity), spend at most one cheap look \
             at its public profile page or its site's contact/footer surface, and also check \
             its public LinkedIn/Instagram/Facebook profile if you find one linked. \
             Explicitly SKIP pseudonymous individuals — a forum handle, a personal account \
             with no business attached — do not spend fetches chasing them; a pseudonymous \
             poster with no contact is a normal, correct result, not a gap to fill.\n\n\
             EXTRACTION — report every reachable channel you found under that prospect's \
             `contacts`. A generic channel with no named individual (e.g. \
             `contato@company.example`, a storefront WhatsApp number) is still a valid \
             contact — record it with an empty `name` rather than discarding it.\n\n\
             ANTI-FABRICATION (mandatory): only report a contact channel that appears \
             verbatim in a page you actually fetched. Never construct an email, phone \
             number, or handle from the company's domain or a person's name. Most leads \
             will legitimately have no contacts — an empty `contacts` list is the correct, \
             expected answer for the majority of prospects; do not pressure yourself toward \
             filling the field. Acquisition and anti-fabrication compose, they do not \
             compete: search hard where it's cheap, report only what you saw."
        ),
    }
}

/// Build the prospecting-sweep prompt from the triggering event, shaped by
/// the resolved `contact_enrichment.prospect` depth + `max_fetches` (task
/// 3's policy knob — resolved once by the caller, never re-resolved here).
/// Ports `orchestrator`'s reddit-prospecting-inspired flow: forum/web sweep
/// -> pain points -> four-pillar vertical mapping -> outreach hooks, framed
/// against this practice's own positioning (business/docs
/// `brand.md`/`services.md`) so prospects map back onto real, sellable
/// service pillars rather than generic leads.
fn build_prompt(
    event: &ResearchAgentEventSchema,
    depth: ContactDepth,
    max_fetches: u8,
    locale: Locale,
) -> String {
    let vertical = event.vertical.as_deref().unwrap_or("a relevant vertical");
    let topic = event
        .topic
        .as_deref()
        .map(|topic| format!(" Narrow the sweep to: {topic}."))
        .unwrap_or_default();

    let base = format!(
        "You are prospecting on behalf of a solo AI & Automations Engineer who \
         builds production-grade agentic systems for small and mid-size \
         businesses across four service pillars: private knowledge systems \
         (RAG over internal docs), agentic workflow automation, interactive AI \
         assistants, and the application layer around them.\n\n\
         Use WebSearch/WebFetch to sweep forums, communities, and public \
         discussion (e.g. Reddit, industry forums, X/Twitter, LinkedIn posts) \
         for people or companies in the \"{vertical}\" vertical publicly \
         describing operational pain.{topic} For each prospect found, identify \
         the pain point(s) raised, map it to the single service pillar it best \
         fits, and cite the source URL. Then summarize the pain-point themes \
         that recur across multiple prospects, and suggest a concrete outreach \
         hook per prospect grounded in what they actually said.\n\n\
         Respond with strict JSON matching this shape: {{\"vertical\": str, \
         \"prospects\": [{{\"name\": str, \"pain_points\": [str], \"pillar\": \
         str, \"outreach_hook\": str, \"source\": str, \"contacts\": \
         [{{\"name\": str, \"role\": str, \"emails\": [str], \"whatsapp\": \
         [str], \"phones\": [str], \"links\": [str], \"note\": str}}]}}], \
         \"common_pain_points\": [str], \"sources\": [str]}}."
    );

    format!(
        "{base}{}\n\n{}",
        contact_directive(depth, max_fetches),
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

/// The prospecting-sweep terminal node of the `RESEARCH_AGENT` workflow.
pub struct ProspectingResearchNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl ProspectingResearchNode {
    /// Construct with `WebSearch`/`WebFetch` granted and the
    /// prospecting-result `json_schema` set; `process` overwrites `model`
    /// per the resolved `prospect`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                allowed_tools: vec!["WebSearch".to_string(), "WebFetch".to_string()],
                json_schema: Some(prospecting_result_json_schema()),
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

impl Default for ProspectingResearchNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ProspectingResearchNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let worktree = worktree_path(&ctx);

        let policy: ResearchAgentPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.prospect,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let contact_depth = policy.contact_enrichment.prospect;
        let max_fetches = policy.contact_enrichment.max_fetches;
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&event, contact_depth, max_fetches, event.locale),
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

        let result: ProspectingResult = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a ProspectingResult from the model's reply: {err}"
                ))
            })?;

        let mut result_value = serde_json::to_value(&result).map_err(|err| {
            NodeError::new(format!("failed to serialize ProspectingResult: {err}"))
        })?;
        // Stamp the resolved contact-enrichment depth alongside the result so
        // EN.4.0 telemetry can attribute cost to the setting that caused it.
        // `ProspectingResult` ignores unknown fields on deserialize, so this
        // is a transparent addition to the node's result object.
        if let Some(obj) = result_value.as_object_mut() {
            obj.insert(
                "contact_enrichment_depth".to_string(),
                serde_json::to_value(contact_depth).unwrap_or_default(),
            );
            // Stamp the resolved locale alongside the result so EN.4.0
            // telemetry can attribute prose-language cost/quality to the
            // locale that caused it (CLAUDE.md rule 6).
            obj.insert(
                "locale".to_string(),
                serde_json::to_value(event.locale).unwrap_or_default(),
            );
        }
        put_result(&mut ctx, NODE_NAME, result_value);

        let model_tier_used = std::collections::BTreeMap::from([(
            "prospect".to_string(),
            tier_str(policy.model_tiers.prospect),
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
        persist_state(&worktree, "prospecting", &policy, &telemetry)?;

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

    fn prospecting_event() -> ResearchAgentEventSchema {
        ResearchAgentEventSchema {
            mode: ResearchMode::Prospecting,
            company_name: None,
            company_url: None,
            vertical: Some("legal-tech".to_string()),
            topic: Some("contract review pain points".to_string()),
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn stub_result_json() -> serde_json::Value {
        json!({
            "vertical": "legal-tech",
            "prospects": [{
                "name": "Jane Doe Legal",
                "pain_points": ["Slow contract turnaround"],
                "pillar": "automation",
                "outreach_hook": "Posted about contract delays on r/legaltech",
                "source": "https://reddit.com/r/legaltech/abc",
            }],
            "common_pain_points": ["Manual contract review"],
            "sources": ["https://reddit.com/r/legaltech"],
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_result_json()).unwrap(),
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

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-prospecting-research-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn process_populates_prospecting_result_and_usage() {
        let node =
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let mut ctx = empty_ctx(prospecting_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let result: ProspectingResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid ProspectingResult");
        assert_eq!(result.vertical, "legal-tech");
        assert!(!result.prospects.is_empty());
        assert!(!result.common_pain_points.is_empty());

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = ProspectingResearchNode::new().with_transport(stub_transport(None));
        let mut ctx = empty_ctx(prospecting_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let result: ProspectingResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid ProspectingResult");
        assert_eq!(result.vertical, "legal-tech");
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        // Task 8: the node reads a policy already resolved (event override
        // merged in) and stamped at dispatch — it no longer re-merges
        // `event.policy` itself, so this test stamps the *already-merged*
        // final policy directly rather than an `event.policy` override.
        let event = prospecting_event();
        let policy = ResearchAgentPolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ModelTiers {
                prospect: ModelTier::Opus,
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
                    text: serde_json::to_string(&stub_result_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_result_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = ProspectingResearchNode::new().with_transport(transport);
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
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let mut ctx = empty_ctx(prospecting_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        node.process(ctx).await.expect("process should succeed");

        let state_path = worktree.join("planning").join("research-agent-state.json");
        assert!(state_path.exists());
        let content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mode"], "prospecting");
        assert_eq!(parsed["policy"]["model_tiers"]["prospect"], "sonnet");
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node =
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let ctx = TaskContext {
            event: json!({ "mode": "not-a-real-mode" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid RESEARCH_AGENT event"));
    }

    // --- Task 5: policy-driven per-lead contact acquisition ---

    #[test]
    fn off_depth_prompt_has_no_contact_directive() {
        let prompt = build_prompt(&prospecting_event(), ContactDepth::Off, 4, Locale::PtBr);
        assert!(!prompt.to_lowercase().contains("acquisition"));
        assert!(!prompt.to_lowercase().contains("anti-fabrication"));
    }

    #[test]
    fn standard_depth_names_one_attempt_and_skip_pseudonymous_and_budget() {
        let prompt = build_prompt(
            &prospecting_event(),
            ContactDepth::Standard,
            4,
            Locale::PtBr,
        );
        assert!(prompt.contains("ACQUISITION"));
        assert!(prompt.contains("one attempt per identifiable business"));
        assert!(prompt.contains("SKIP pseudonymous individuals"));
        assert!(prompt.contains("up to 4 extra page loads"));
        // Standard does not add the social-profile sweep (the base prompt's
        // own "LinkedIn posts" forum-sweep mention is unaffected by depth).
        assert!(!prompt.contains("public LinkedIn/Instagram/Facebook"));
    }

    #[test]
    fn deep_depth_adds_public_profile_sweep_and_keeps_one_attempt_rule() {
        let prompt = build_prompt(&prospecting_event(), ContactDepth::Deep, 8, Locale::PtBr);
        assert!(prompt.contains("LinkedIn"));
        assert!(prompt.contains("Instagram"));
        assert!(prompt.contains("Facebook"));
        assert!(prompt.contains("one attempt per identifiable business"));
        assert!(prompt.contains("SKIP pseudonymous individuals"));
        assert!(prompt.contains("up to 8 extra page loads"));
    }

    #[test]
    fn non_off_depths_carry_anti_fabrication_directive() {
        for depth in [ContactDepth::Standard, ContactDepth::Deep] {
            let prompt = build_prompt(&prospecting_event(), depth, 4, Locale::PtBr);
            assert!(
                prompt.contains("Never construct"),
                "depth {depth:?} missing the anti-fabrication directive"
            );
            assert!(
                prompt.contains("legitimately have no contacts"),
                "depth {depth:?} missing the most-leads-have-none framing"
            );
            assert!(
                prompt.contains("do not compete"),
                "depth {depth:?} missing the composing-not-competing framing"
            );
        }
    }

    #[test]
    fn breadth_over_depth_framing_present_at_non_off_depths() {
        for depth in [ContactDepth::Standard, ContactDepth::Deep] {
            let prompt = build_prompt(&prospecting_event(), depth, 4, Locale::PtBr);
            assert!(prompt.to_lowercase().contains("breadth"));
        }
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_all_depths() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for depth in [
            ContactDepth::Off,
            ContactDepth::Standard,
            ContactDepth::Deep,
        ] {
            let _ = build_prompt(&prospecting_event(), depth, 4, Locale::PtBr);
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    // --- Task 6: locale-aware prose directive -----------------------------

    #[test]
    fn prompt_body_names_the_event_locale_language() {
        let pt_prompt = build_prompt(&prospecting_event(), ContactDepth::Off, 4, Locale::PtBr);
        assert!(pt_prompt.contains("Brazilian Portuguese"));
        let en_prompt = build_prompt(&prospecting_event(), ContactDepth::Off, 4, Locale::EnUs);
        assert!(en_prompt.contains("English (en-US)"));
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_locales() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for locale in [Locale::PtBr, Locale::EnUs] {
            let _ = build_prompt(&prospecting_event(), ContactDepth::Standard, 4, locale);
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    #[test]
    fn no_prompt_synthesizes_a_contact_channel() {
        for depth in [ContactDepth::Standard, ContactDepth::Deep] {
            let prompt = build_prompt(&prospecting_event(), depth, 4, Locale::PtBr);
            assert!(!prompt.to_lowercase().contains("info@{domain}"));
            assert!(!prompt.to_lowercase().contains("guess an address"));
        }
    }

    #[tokio::test]
    async fn stub_transport_with_contact_populates_lead_contacts() {
        let mut result_json = stub_result_json();
        result_json["prospects"][0]["contacts"] = json!([{
            "name": "",
            "role": "",
            "emails": [],
            "whatsapp": ["+55 11 91234-5678"],
            "phones": [],
            "links": [],
            "note": "WhatsApp link found in profile bio",
        }]);

        let node = ProspectingResearchNode::new().with_transport(stub_transport(Some(result_json)));
        let mut ctx = empty_ctx(prospecting_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let result: ProspectingResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid ProspectingResult");
        assert_eq!(result.prospects[0].contacts.len(), 1);
        assert_eq!(
            result.prospects[0].contacts[0].whatsapp,
            vec!["+55 11 91234-5678"]
        );
    }

    #[tokio::test]
    async fn leads_with_no_contacts_key_yield_empty_vec_and_succeed() {
        // stub_result_json() carries no "contacts" key on its prospect.
        let node =
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let mut ctx = empty_ctx(prospecting_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let result: ProspectingResult =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid ProspectingResult");
        assert_eq!(result.prospects[0].contacts, Vec::new());
    }

    #[tokio::test]
    async fn resolved_contact_enrichment_depth_is_stamped_into_the_result() {
        let policy = ResearchAgentPolicy {
            contact_enrichment: super::super::policy::ContactEnrichment {
                prospect: ContactDepth::Deep,
                ..ResearchAgentPolicy::default().contact_enrichment
            },
            ..ResearchAgentPolicy::default()
        };
        let node =
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let mut ctx = empty_ctx(prospecting_event());
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
            ProspectingResearchNode::new().with_transport(stub_transport(Some(stub_result_json())));
        let mut event = prospecting_event();
        event.locale = Locale::EnUs;
        let mut ctx = empty_ctx(event);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        assert_eq!(ctx.nodes[NODE_NAME]["locale"], json!("en-US"));
    }
}
