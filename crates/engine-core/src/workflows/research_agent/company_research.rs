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

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::policy::telemetry::RunTelemetryInputs;
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::{ModelTier, ResearchAgentPolicy};
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

/// Build the single-company research-brief prompt from the triggering
/// event. Ports `orchestrator`'s RESEARCH_AGENT company-brief prompt,
/// framed against this practice's own positioning (business/docs
/// `brand.md`/`services.md`) so the model's pain-point/outreach-hook
/// suggestions map back onto real, sellable services rather than generic
/// advice.
fn build_prompt(event: &ResearchAgentEventSchema) -> String {
    let company_name = event.company_name.as_deref().unwrap_or("the company");
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
         (cite the source URL for each).\n\n\
         Respond with strict JSON matching this shape: {{\"company_name\": \
         str, \"summary\": str, \"recent_developments\": [str], \
         \"pain_points\": [str], \"outreach_hooks\": [str], \"sources\": \
         [str]}}."
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
        };
        let telemetry = crate::policy::harvest_telemetry(&ctx, chrono::Utc::now(), inputs);
        persist_state(&worktree, "company", &policy, &telemetry)?;

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

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-company-research-test-{}-{n}",
            std::process::id()
        ));
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
            model_tiers: super::super::policy::ModelTiers {
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
}
