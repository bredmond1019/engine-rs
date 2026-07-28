//! `IntakeExtractNode` — the single, terminal, policy-aware structured
//! extraction node (pure extraction; no `WebSearch`/`WebFetch`).
//!
//! A terminal model node (no forward connection) wrapping
//! `crate::nodes::claude_code_step::ClaudeCodeStep`, authored fresh from
//! `agentic-portfolio/business/docs/diagnostic/intake.md`'s interview
//! groups + evidence discipline (client's own words; empty axes flagged,
//! not invented) and the São Paulo SMB priors (§5). On `process`:
//! 1. read the run's [`super::policy::DiagnosticIntakePolicy`] stamped once
//!    at dispatch (`crate::policy::resolved_policy_strict`, EN.5.D task 8)
//!    — no per-node re-resolution;
//! 2. apply `extract`-stage shaping (model tier, prompt cache, verbosity
//!    directive) to the composed `Config`;
//! 3. await the (injectable) transport and parse its reply into a
//!    [`super::schema::DiagnosticIntake`];
//! 4. stamp the parsed intake + usage onto `ctx` (via the composed
//!    `ClaudeCodeStep`) and harvest + persist a
//!    `diagnostic-intake-state.json` telemetry record;
//! 5. return `ctx` unchanged otherwise — this node is a graph exit point,
//!    both the start and the terminal node (no router).

use std::path::{Path, PathBuf};

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::json;

use crate::locale::{language_directive, Locale};
use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::policy::telemetry::RunTelemetryInputs;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::{DiagnosticIntakePolicy, ModelTier};
use super::schema::{diagnostic_intake_json_schema, DiagnosticIntake, DiagnosticIntakeEventSchema};

/// The `Node::name()` identity `IntakeExtractNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes`/`ctx.node_runs` key its
/// output/usage are stamped onto.
const NODE_NAME: &str = "IntakeExtractNode";

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true — mirrors
/// `research_agent::company_research::STABLE_SYSTEM_PROMPT`'s rationale:
/// keeping this byte-identical across calls gives the underlying `claude`
/// CLI (or local OpenAI-compat transport) a stable prefix to cache against.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs DIAGNOSTIC_INTAKE workflow, the \
     IntakeExtractNode stage. This system prompt is held constant across \
     calls so its tokens can be cached. You perform pure structured \
     extraction only — no web search, no tool use.";

/// The verdict-bearing model-judgment stages this node's telemetry snapshot
/// inspects. `IntakeExtractNode` has no downstream review stage — it is the
/// only node in this workflow.
const VERDICT_STAGES: [&str; 0] = [];

/// The model-node identities whose `ctx.nodes` output may carry a
/// `"cost_usd"` field (`ClaudeCodeStep`'s output shape).
const COST_BEARING_STAGES: [&str; 1] = [NODE_NAME];

/// Deserialize the inbound `DIAGNOSTIC_INTAKE` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<DiagnosticIntakeEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid DIAGNOSTIC_INTAKE event: {err}")))
}

/// Build the extraction prompt from the raw call notes/transcript. Ports
/// `intake.md`'s four interview groups (company context, process & pain,
/// tool landscape, existing automations), the evidence discipline (the
/// `*_evidence` fields must hold the client's own words or direct
/// observation, not inference — an empty axis is flagged, never invented),
/// and the São Paulo SMB priors (`intake.md §5`: WhatsApp as system of
/// record, Pix as payment backbone, Mercado Livre/Instagram as storefront,
/// spreadsheets as the glue).
///
/// `locale` governs only the prose fields this node itself authors (e.g.
/// summarizing/labelling), never the `*_evidence` fields — those hold the
/// client's own words verbatim per the evidence discipline below, and must
/// never be translated or paraphrased into the run's locale.
fn build_prompt(event: &DiagnosticIntakeEventSchema, locale: Locale) -> String {
    format!(
        "You are extracting a structured `DiagnosticIntake` evidence record from raw \
         diagnostic-call notes or a transcript, for a paid automation diagnostic \
         (agentic-portfolio/business/docs/diagnostic/intake.md).\n\n\
         The notes were gathered across four interview groups — extract into these \
         fields as you find evidence for them:\n\
         - Group A (company context): `company_type`, `team_size`, and rhythm-of-\
           operation context feeding `top_workflows`.\n\
         - Group B (process & pain): each repetitive, time-eating task becomes a \
           `WorkflowCandidate` — `name`, `description`, `time_cost_evidence`, \
           `knowledge_holder` (bus-factor risk), `failure_mode` (what breaks when \
           it goes wrong).\n\
         - Group C (tool landscape): `existing_tools`, `primary_channels`, and \
           `buildability_notes` (integrations, system of record, manual bridges).\n\
         - Group D (existing automations): `existing_automations` (what was tried, \
           what broke), plus more `buildability_notes` (risk tolerance, where a \
           human-in-the-loop gate is mandatory).\n\n\
         EVIDENCE DISCIPLINE — this is the most important rule: every \
         `*_evidence` field (`frequency_evidence`, `time_cost_evidence`) must hold \
         the client's own words (a direct quote) or your direct observation from \
         the notes — never your inference or a plausible-sounding guess. If the \
         notes don't support a field, leave it an empty string rather than \
         inventing content; a scoring stage downstream reads these fields directly \
         and an invented quote would corrupt the score.\n\n\
         São Paulo SMB priors to recognize (not to assume where the notes say \
         otherwise): WhatsApp is often the system of record for orders and \
         customer history, not just a chat app; Pix is the payment backbone and \
         reconciliation against it is often manual; Mercado Livre/Instagram is \
         often the storefront, out of sync with an internal sheet; spreadsheets \
         are often the 'database'. Common first workflow candidates in this \
         segment: order tracking (WhatsApp <-> sheet), supplier follow-up \
         messages, customer support queues, inventory reconciliation (Mercado \
         Livre <-> sheet), and Pix payment matching.\n\n\
         Raw call notes/transcript:\n---\n{notes}\n---\n\n\
         Respond with strict JSON matching the DiagnosticIntake schema: \
         company_name, company_type, team_size, primary_channels[], \
         existing_tools[], existing_automations[], and top_workflows[] where each \
         entry has name, description, frequency_evidence, time_cost_evidence, \
         buildability_notes, knowledge_holder, failure_mode.\n\n\
         {directive} This directive governs the prose fields you author (e.g. \
         `description`, `failure_mode`, `buildability_notes`) — it does NOT apply \
         to the `*_evidence` fields, which must stay in the client's own words \
         exactly as spoken, untranslated, per the evidence discipline above.",
        notes = event.notes,
        directive = language_directive(locale),
    )
}

/// Read the worktree path stamped by an upstream setup node, if this run
/// went through one. `DIAGNOSTIC_INTAKE` has no dedicated setup node, so
/// this is best-effort — used only by [`persist_state`] to locate
/// `planning/diagnostic-intake-state.json`, not for policy resolution (task
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

/// Persist `record` (a `{policy, telemetry}` snapshot) to
/// `planning/diagnostic-intake-state.json` inside `worktree` — the file the
/// `#[ignore]`-gated experiment harness (task 8) aggregates via
/// `crate::policy::aggregate_state_files`.
fn persist_state(
    worktree: &Path,
    policy: &DiagnosticIntakePolicy,
    telemetry: &crate::policy::RunTelemetry,
) -> Result<String, NodeError> {
    let state_dir = worktree.join("planning");
    std::fs::create_dir_all(&state_dir).map_err(|err| {
        NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
    })?;
    let state_path = state_dir.join("diagnostic-intake-state.json");
    let record = json!({
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

/// The single terminal node of the `DIAGNOSTIC_INTAKE` workflow — both the
/// start and the terminal node; there is no router.
pub struct IntakeExtractNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl IntakeExtractNode {
    /// Construct with the `DiagnosticIntake` `json_schema` set for
    /// schema-constrained extraction and **no** `WebSearch`/`WebFetch` in
    /// `allowed_tools` (pure extraction is out of scope for tool use per
    /// the block); `process` overwrites `model` per the resolved
    /// `extract`-stage tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                allowed_tools: Vec::new(),
                json_schema: Some(diagnostic_intake_json_schema()),
                ..Config::default()
            },
            transport: None,
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`. The Local-tier rewire
    /// (`graph::registry_for_policy`) also uses this seam to swap in
    /// `crate::nodes::openai_compat_transport::openai_compat_transport_live`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }
}

impl Default for IntakeExtractNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for IntakeExtractNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let worktree = worktree_path(&ctx);

        let policy: DiagnosticIntakePolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let mut config = self.config.clone();
        config = crate::policy::apply_model_tier(
            config,
            policy.model_tiers.extract,
            &policy.local.model,
        );
        config =
            crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
        let prompt = crate::policy::apply_verbosity_directive(
            build_prompt(&event, event.locale),
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

        let intake: DiagnosticIntake = parse_structured_or_fenced(&ctx, NODE_NAME, &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse a DiagnosticIntake from the model's reply: {err}"
                ))
            })?;

        let mut intake_value = serde_json::to_value(&intake).map_err(|err| {
            NodeError::new(format!("failed to serialize DiagnosticIntake: {err}"))
        })?;
        // Stamp the resolved locale alongside the intake so EN.4.0 telemetry
        // can attribute prose-language cost/quality to the locale that
        // caused it (CLAUDE.md rule 6). `DiagnosticIntake` ignores unknown
        // fields on deserialize, so this is a transparent addition.
        if let Some(obj) = intake_value.as_object_mut() {
            obj.insert(
                "locale".to_string(),
                serde_json::to_value(event.locale).unwrap_or_default(),
            );
        }
        put_result(&mut ctx, NODE_NAME, intake_value);

        let model_tier_used = std::collections::BTreeMap::from([(
            "extract".to_string(),
            tier_str(policy.model_tiers.extract),
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
        persist_state(&worktree, &policy, &telemetry)?;

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

    use super::*;

    /// Builds a `ctx` and stamps a default [`DiagnosticIntakePolicy`] under
    /// `RESOLVED_POLICY_IDENTITY` — required since task 8's strict
    /// `resolved_policy_strict` read (no more per-node re-resolution or a
    /// silent `Default` fallback).
    fn empty_ctx(event: DiagnosticIntakeEventSchema) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(DiagnosticIntakePolicy::default()).expect("policy serializes"),
        );
        ctx
    }

    fn intake_event() -> DiagnosticIntakeEventSchema {
        DiagnosticIntakeEventSchema {
            notes: "Client: \"We track orders by scrolling WhatsApp threads, probably \
                    an hour a day.\" Only Maria knows the supplier list."
                .to_string(),
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn stub_intake_json() -> serde_json::Value {
        json!({
            "company_name": "Loja da Ana",
            "company_type": "retail SMB",
            "team_size": 4,
            "primary_channels": ["WhatsApp", "Mercado Livre"],
            "existing_tools": ["Google Sheets", "WhatsApp Business"],
            "existing_automations": [],
            "top_workflows": [
                {
                    "name": "WhatsApp order tracking",
                    "description": "Orders are tracked by scrolling WhatsApp threads.",
                    "frequency_evidence": "\"Every single day.\"",
                    "time_cost_evidence": "\"Probably an hour a day.\"",
                    "buildability_notes": "WhatsApp Business API available.",
                    "knowledge_holder": "Only Maria knows the supplier list.",
                    "failure_mode": "Orders get lost when Maria is out."
                }
            ]
        })
    }

    fn stub_transport(structured: Option<serde_json::Value>) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_intake_json()).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 200,
                        output_tokens: 80,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 200,
                            output_tokens: 80,
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

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-intake-extract-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn process_populates_diagnostic_intake_with_evidence_fields_and_usage() {
        let node =
            IntakeExtractNode::new().with_transport(stub_transport(Some(stub_intake_json())));
        let mut ctx = empty_ctx(intake_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let intake: DiagnosticIntake =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid DiagnosticIntake");
        assert_eq!(intake.company_name, "Loja da Ana");
        assert_eq!(intake.team_size, 4);
        assert!(!intake.top_workflows.is_empty());
        let candidate = &intake.top_workflows[0];
        assert!(!candidate.frequency_evidence.is_empty());
        assert!(!candidate.time_cost_evidence.is_empty());
        assert_eq!(
            candidate.knowledge_holder,
            "Only Maria knows the supplier list."
        );

        let run = ctx.node_runs.get(NODE_NAME).expect("node run recorded");
        let usage = run.usage.as_ref().expect("usage recorded");
        assert_eq!(usage.input_tokens, Some(200));
        assert_eq!(usage.output_tokens, Some(80));
    }

    #[tokio::test]
    async fn process_falls_back_to_fenced_parse_when_structured_is_absent() {
        let node = IntakeExtractNode::new().with_transport(stub_transport(None));
        let mut ctx = empty_ctx(intake_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": temp_worktree().to_string_lossy() }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let intake: DiagnosticIntake =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid DiagnosticIntake");
        assert_eq!(intake.company_name, "Loja da Ana");
    }

    #[tokio::test]
    async fn process_applies_tier_cache_and_verbosity_shaping() {
        // Task 8: the node reads a policy already resolved (event override
        // merged in) and stamped at dispatch — it no longer re-merges
        // `event.policy` itself, so this test stamps the *already-merged*
        // final policy directly rather than an `event.policy` override.
        let event = intake_event();
        let policy = DiagnosticIntakePolicy {
            output_verbosity: super::super::policy::OutputVerbosity::Terse,
            prompt_cache: true,
            model_tiers: super::super::policy::ModelTiers {
                extract: ModelTier::Opus,
            },
            ..DiagnosticIntakePolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<(Config, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, prompt| {
            *captured_clone.lock().unwrap() = Some((config, prompt));
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&stub_intake_json()).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(stub_intake_json()),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = IntakeExtractNode::new().with_transport(transport);
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
        assert!(config.allowed_tools.is_empty());
    }

    #[tokio::test]
    async fn process_writes_diagnostic_intake_state_json() {
        let worktree = temp_worktree();
        let node =
            IntakeExtractNode::new().with_transport(stub_transport(Some(stub_intake_json())));
        let mut ctx = empty_ctx(intake_event());
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        node.process(ctx).await.expect("process should succeed");

        let state_path = worktree
            .join("planning")
            .join("diagnostic-intake-state.json");
        assert!(state_path.exists());
        let content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["policy"]["model_tiers"]["extract"], "sonnet");
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let node =
            IntakeExtractNode::new().with_transport(stub_transport(Some(stub_intake_json())));
        let ctx = TaskContext {
            event: json!({ "not_notes": "oops" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid DIAGNOSTIC_INTAKE event"));
    }

    // --- Task 6: locale-aware prose directive -----------------------------

    #[test]
    fn prompt_body_names_the_event_locale_language() {
        let pt_prompt = build_prompt(&intake_event(), Locale::PtBr);
        assert!(pt_prompt.contains("Brazilian Portuguese"));
        let en_prompt = build_prompt(&intake_event(), Locale::EnUs);
        assert!(en_prompt.contains("English (en-US)"));
    }

    #[test]
    fn stable_system_prompt_is_byte_identical_across_locales() {
        let anchor = STABLE_SYSTEM_PROMPT;
        for locale in [Locale::PtBr, Locale::EnUs] {
            let _ = build_prompt(&intake_event(), locale);
            assert_eq!(STABLE_SYSTEM_PROMPT, anchor);
        }
    }

    #[test]
    fn locale_directive_does_not_apply_to_evidence_fields() {
        // The directive text must be scoped away from `*_evidence` — assert
        // the carve-out sentence is present so a future edit can't silently
        // drop it and start implying evidence quotes should be translated.
        let prompt = build_prompt(&intake_event(), Locale::EnUs);
        assert!(prompt.contains("does NOT apply"));
        assert!(prompt.contains("*_evidence"));
    }

    #[tokio::test]
    async fn resolved_locale_is_stamped_into_the_result() {
        let node =
            IntakeExtractNode::new().with_transport(stub_transport(Some(stub_intake_json())));
        let mut event = intake_event();
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
