//! `PatchDocsNode` — the docs-patching model node (bottom-half, EN.3.B).
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/patch_docs_node.py`:
//! a Sonnet-tier model node (real judgment, not deterministic — per the
//! spec's Context Pointers economics classification) that reads the most
//! recent `ImplementTaskNode` output's `modified_files`, asks the model to
//! find + patch stale `docs/` references to those files/symbols, and stamps
//! `{summary, files_patched}` under its own identity. This node does not
//! touch the filesystem itself — the model performs any doc edits via its
//! own tool use / the harness that runs it; this node's job is to build the
//! prompt and record what came back (mirrors the Python docstring).

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;

use super::task_loop::{apply_policy_config, resolved_policy, worktree_path, Stage};
use super::{parse_structured_or_fenced, ModelTransport};

/// Model output shape `PatchDocsNode` expects (strict JSON reply).
#[derive(Debug, Deserialize)]
struct PatchDocsOutput {
    summary: String,
    #[serde(default)]
    files_patched: Vec<String>,
}

/// JSON schema matching [`PatchDocsOutput`], passed as `Config.json_schema`
/// so `claude-code-rs` requests (and pre-parses) a schema-constrained reply
/// via `Outcome.structured_output` instead of relying solely on prompt text.
fn patch_docs_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "files_patched": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["summary"],
    })
}

/// Model node (Sonnet): patches documentation referencing the task's
/// modified files. Composes a `ClaudeCodeStep` under the `PatchDocsNode`
/// identity so it can post-process the model's JSON output.
pub struct PatchDocsNode {
    config: Config,
    transport: Option<ModelTransport>,
}

impl PatchDocsNode {
    /// The base `Config` carries **no** `model`: `process` resolves it from
    /// the run policy's `model_tiers.docs` (`Stage::Docs`), whose built-in
    /// default is the `Sonnet` tier — i.e. exactly the `claude-sonnet-4-5`
    /// this constructor used to hardcode.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config::default(),
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

    /// Override the base `Config` entirely (model/tool-permission/etc.
    /// fields) — `process` still applies `json_schema` on top, but every
    /// other field (e.g. `disallowed_tools`, `dangerously_skip_permissions`)
    /// passes through untouched. Mirrors `ImplementTaskNode::with_config`;
    /// lets `graph.rs::registry()` grant this node real headless write
    /// permission without changing its safe-by-default `new()` construction.
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Return the `modified_files` reported by the most recent
    /// `ImplementTaskNode` pass, or an empty list if it hasn't run.
    /// `TaskContext.nodes` stores one entry per node *name*, so across a
    /// retry loop only the latest `ImplementTaskNode` run is available here
    /// — this is that latest pass's reported `modified_files`. Mirrors
    /// `PatchDocsNode._collect_modified_files` in Python.
    fn collect_modified_files(ctx: &TaskContext) -> Vec<String> {
        ctx.nodes
            .get("ImplementTaskNode")
            .and_then(|value| value.get("modified_files"))
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for PatchDocsNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PatchDocsNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // Strict read: an absent/unparsable stamp is an error, never a
        // silent fall back to `SdlcPolicy::default()`.
        let policy = resolved_policy(&ctx)?;

        // Only the CONFIG half of the shaping (model tier, prompt cache,
        // call timeout) can be applied here — the prompt is built by the
        // `with_prompt_builder` closure below at call time, so the verbosity
        // directive is appended in there instead. `apply_prompt_cache`'s
        // `STABLE_SYSTEM_PROMPT` prefix is run-invariant by construction:
        // the policy-varying directive goes in the per-call prompt body,
        // never in that prefix (CLAUDE.md standing rule 6).
        let mut config = apply_policy_config(self.config.clone(), &policy, Stage::Docs);
        config.json_schema = Some(patch_docs_output_schema());

        // THE P0. This node is registered with `agentic_write_config`
        // (`dangerously_skip_permissions: true`, full file-write grant), so
        // an unset `config.cwd` means it writes wherever the *host process*
        // lives — under `bastion serve` that is the primary checkout, on
        // `main`, not the run's worktree. A hard error is deliberate here,
        // unlike `ImplementTaskNode`'s best-effort `if let Ok(..)`: a
        // skip-permissions writer must never be allowed to run unscoped, so
        // the missing stamp fails the walk instead of silently defaulting.
        // `PatchDocsNode` runs strictly downstream of `SetupWorktreeNode` in
        // the declared graph, so the stamp is always present in a real walk.
        let worktree = worktree_path(&ctx)?;
        config.cwd = Some(std::path::PathBuf::from(&worktree));

        let verbosity = policy.output_verbosity;
        let mut step = ClaudeCodeStep::with_prompt_builder(
            "PatchDocsNode",
            config,
            move |ctx: &TaskContext| {
                let modified_files = Self::collect_modified_files(ctx);
                let prompt = format!(
                    "Search docs/ for stale references to the following \
                     modified files/symbols and patch them. Respond with \
                     strict JSON of the shape {{\"summary\": str, \
                     \"files_patched\": [str]}}.\n\nModified files: {}",
                    json!(modified_files)
                );
                crate::policy::apply_verbosity_directive(prompt, verbosity)
            },
        );
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("PatchDocsNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("PatchDocsNode: model returned no content"))?
            .to_string();

        let parsed: PatchDocsOutput = parse_structured_or_fenced(&ctx, "PatchDocsNode", &content)
            .map_err(|err| {
            NodeError::new(format!(
                "PatchDocsNode: failed to parse model output as JSON: {err}"
            ))
        })?;

        super::put_result(
            &mut ctx,
            "PatchDocsNode",
            json!({
                "summary": parsed.summary,
                "files_patched": parsed.files_patched,
                // Stamp the resolved knob values so `RunTelemetry` /
                // `PolicyAggregate` can attribute this stage's observed cost
                // to the settings that caused it (standing rule 6).
                "model_tier": policy.model_tiers.docs,
                "call_timeout_secs": policy.timeouts.docs,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PatchDocsNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::RESOLVED_POLICY_IDENTITY;
    use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, OutputVerbosity, SdlcPolicy};
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A stub worktree path — never touched on disk, only compared.
    const WORKTREE: &str = "/tmp/engine-rs-patch-docs-worktree";

    /// A bare ctx with NO resolved-policy stamp and NO `SetupWorktreeNode`
    /// output — used to pin the two strict failure modes.
    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// The shape a real walk hands `PatchDocsNode`: a stamped resolved
    /// policy plus `SetupWorktreeNode`'s `worktree_path`.
    fn ctx_with_policy(policy: &SdlcPolicy) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(policy).expect("policy serializes"),
        );
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": WORKTREE }),
        );
        ctx
    }

    /// A transport that records the `Config` and prompt it was handed, then
    /// replies with a valid `PatchDocsOutput`.
    #[allow(clippy::type_complexity)]
    fn capturing_transport(
        captured: Arc<std::sync::Mutex<Option<(Config, String)>>>,
    ) -> ModelTransport {
        Arc::new(move |config, prompt| {
            *captured.lock().unwrap() = Some((config.clone(), prompt.clone()));
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({ "summary": "ok", "files_patched": [] }).to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        })
    }

    fn stub_transport(reply: serde_json::Value) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: reply.to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        })
    }

    /// Like `stub_transport` but returns non-JSON `text` alongside a
    /// pre-parsed `structured_output`, so a passing test proves the
    /// `structured` field was consumed rather than the fence-strip path.
    fn stub_transport_structured(structured: serde_json::Value) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: "not fence-parseable json".to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: Some(structured.clone()),
            };
            Box::pin(async move { Ok(outcome) })
        })
    }

    #[tokio::test]
    async fn stamps_summary_and_files_patched_from_stub_transport() {
        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs", "src/bar.rs"],
                "tests_added": [],
            }),
        );

        let node = PatchDocsNode::new().with_transport(stub_transport(json!({
            "summary": "patched stale references",
            "files_patched": ["docs/foo.md"],
        })));

        let out = node.process(ctx).await.expect("process should succeed");
        let result = out.nodes.get("PatchDocsNode").expect("output present");
        assert_eq!(result["summary"], "patched stale references");
        assert_eq!(result["files_patched"], json!(["docs/foo.md"]));
    }

    #[tokio::test]
    async fn stamps_summary_and_files_patched_from_structured_output() {
        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs"],
                "tests_added": [],
            }),
        );

        let node = PatchDocsNode::new().with_transport(stub_transport_structured(json!({
            "summary": "patched via structured output",
            "files_patched": ["docs/foo.md"],
        })));

        let out = node.process(ctx).await.expect("process should succeed");
        let result = out.nodes.get("PatchDocsNode").expect("output present");
        assert_eq!(result["summary"], "patched via structured output");
        assert_eq!(result["files_patched"], json!(["docs/foo.md"]));
    }

    #[tokio::test]
    async fn collects_modified_files_from_latest_implement_task_node() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "s",
                "modified_files": ["a.rs", "b.rs"],
                "tests_added": [],
            }),
        );
        assert_eq!(
            PatchDocsNode::collect_modified_files(&ctx),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[tokio::test]
    async fn collects_empty_when_implement_task_node_absent() {
        let ctx = empty_context(json!({}));
        assert_eq!(
            PatchDocsNode::collect_modified_files(&ctx),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn with_config_overrides_the_config_passed_to_the_transport() {
        let captured_config: Arc<std::sync::Mutex<Option<Config>>> =
            Arc::new(std::sync::Mutex::new(None));
        let captured_config_clone = captured_config.clone();
        let reply = json!({
            "summary": "patched stale references",
            "files_patched": ["docs/foo.md"],
        });
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *captured_config_clone.lock().unwrap() = Some(config.clone());
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: reply.to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = PatchDocsNode::new()
            .with_config(Config {
                model: Some("claude-opus-4-1".to_string()),
                dangerously_skip_permissions: true,
                disallowed_tools: vec!["Bash".to_string()],
                isolated: true,
                ..Config::default()
            })
            .with_transport(transport);

        let ctx = ctx_with_policy(&SdlcPolicy::default());
        node.process(ctx).await.expect("process should succeed");

        let config = captured_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called with a config");
        // `model` is NOT passed through any more: `process` overwrites it
        // from the resolved `model_tiers.docs` tier. Every OTHER field of
        // the injected config still passes through untouched, which is what
        // `graph.rs::registry()` relies on for the write grant.
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(config.dangerously_skip_permissions);
        assert_eq!(config.disallowed_tools, vec!["Bash".to_string()]);
        assert!(config.isolated);
    }

    // --- policy / cwd contract (the P0) -----------------------------------

    /// THE P0 REGRESSION. `PatchDocsNode` is registered with
    /// `agentic_write_config` (`dangerously_skip_permissions`), so it must
    /// never reach the transport with an unscoped cwd — otherwise it writes
    /// into whatever directory the `bastion serve` process happens to live
    /// in (the primary checkout, on `main`) instead of the run's worktree.
    #[tokio::test]
    async fn scopes_config_cwd_to_the_stamped_worktree() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));

        node.process(ctx_with_policy(&SdlcPolicy::default()))
            .await
            .expect("process should succeed");

        let (config, _) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.cwd, Some(std::path::PathBuf::from(WORKTREE)));
    }

    /// The other half of the P0: with no `SetupWorktreeNode` stamp the node
    /// HARD-ERRORS rather than falling back to the process cwd. Deliberately
    /// stricter than `ImplementTaskNode`'s best-effort `if let Ok(..)`,
    /// because this node carries a skip-permissions write grant.
    #[tokio::test]
    async fn hard_errors_when_the_worktree_stamp_is_absent() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).unwrap(),
        );
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));

        let err = node.process(ctx).await.expect_err("must not run unscoped");
        assert!(
            err.to_string().contains("worktree_path"),
            "unexpected error: {err}"
        );
        // And it never reached the transport at all.
        assert!(captured.lock().unwrap().is_none());
    }

    /// Strict policy read: an absent stamp is an error, not a silent
    /// fall back to `SdlcPolicy::default()`.
    #[tokio::test]
    async fn hard_errors_when_the_resolved_policy_stamp_is_absent() {
        let node = PatchDocsNode::new()
            .with_transport(capturing_transport(Arc::new(std::sync::Mutex::new(None))));
        let result = node.process(empty_context(json!({}))).await;
        assert!(result.is_err());
    }

    /// Behavior stability: under the built-in default the node still runs
    /// exactly the model it used to hardcode, and sets no timeout.
    #[tokio::test]
    async fn baseline_policy_reproduces_the_former_hardcoded_model() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));

        node.process(ctx_with_policy(&SdlcPolicy::default()))
            .await
            .expect("process should succeed");

        let (config, prompt) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(config.timeout, None);
        // Normal verbosity injects nothing, so the prompt is byte-identical
        // to the pre-policy one.
        assert!(!prompt.contains("Be terse"));
        assert!(!prompt.contains("Be thorough"));
    }

    /// A tier-overriding policy reaches `config.model`, and the `docs`
    /// call-timeout reaches `config.timeout`.
    #[tokio::test]
    async fn resolved_docs_tier_and_timeout_reach_the_config() {
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                docs: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                docs: Some(900),
                ..Default::default()
            },
            ..SdlcPolicy::default()
        };
        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));

        node.process(ctx_with_policy(&policy))
            .await
            .expect("process should succeed");

        let (config, _) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(config.timeout, Some(std::time::Duration::from_secs(900)));
    }

    /// The prompt half of the shaping is applied INSIDE the
    /// `with_prompt_builder` closure — the verbosity directive lands on the
    /// built prompt, while the cached `system_prompt` prefix stays
    /// run-invariant across verbosity settings (standing rule 6).
    #[tokio::test]
    async fn verbosity_directive_reaches_the_builder_prompt_not_the_cached_prefix() {
        let policy = SdlcPolicy {
            output_verbosity: OutputVerbosity::Terse,
            prompt_cache: true,
            ..SdlcPolicy::default()
        };
        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));
        node.process(ctx_with_policy(&policy))
            .await
            .expect("process should succeed");
        let (terse_config, terse_prompt) =
            captured.lock().unwrap().clone().expect("transport called");
        assert!(terse_prompt.contains("Be terse"));
        assert!(terse_prompt.starts_with("Search docs/"));

        let policy = SdlcPolicy {
            output_verbosity: OutputVerbosity::Verbose,
            prompt_cache: true,
            ..SdlcPolicy::default()
        };
        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));
        node.process(ctx_with_policy(&policy))
            .await
            .expect("process should succeed");
        let (verbose_config, verbose_prompt) =
            captured.lock().unwrap().clone().expect("transport called");
        assert!(verbose_prompt.contains("Be thorough"));

        // The cache breakpoint is byte-stable across the two settings.
        assert_eq!(terse_config.system_prompt, verbose_config.system_prompt);
        assert!(terse_config.system_prompt.is_some());
    }

    /// The resolved knobs are stamped into the node's own result for
    /// telemetry attribution.
    #[tokio::test]
    async fn stamps_the_resolved_tier_and_timeout_for_telemetry() {
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                docs: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                docs: Some(120),
                ..Default::default()
            },
            ..SdlcPolicy::default()
        };
        let node = PatchDocsNode::new().with_transport(stub_transport(json!({
            "summary": "s",
            "files_patched": [],
        })));
        let out = node
            .process(ctx_with_policy(&policy))
            .await
            .expect("process should succeed");
        let result = out.nodes.get("PatchDocsNode").expect("output present");
        assert_eq!(result["model_tier"], json!("haiku"));
        assert_eq!(result["call_timeout_secs"], json!(120));
    }

    #[tokio::test]
    async fn errors_on_non_json_model_reply() {
        let ctx = ctx_with_policy(&SdlcPolicy::default());
        let node = PatchDocsNode::new().with_transport(Arc::new(|_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: "not json".to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        }));

        let result = node.process(ctx).await;
        assert!(result.is_err());
    }
}
