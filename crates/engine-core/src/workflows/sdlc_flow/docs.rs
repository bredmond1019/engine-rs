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

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};
use crate::nodes::AgentCodeStep;
#[cfg(test)]
use crate::nodes::MetaTransport;
use crate::workflows::llm_node::{
    Cancellable as LlmCancellable, TransportSlotted as LlmTransportSlotted,
};

use super::task_loop::{apply_policy_config, resolved_policy, worktree_path, Stage};
#[cfg(test)]
use super::ModelTransport;
use super::{parse_model_verdict, session_baseline, sessions_since, ModelVerdict, TransportSlot};

/// Model output shape `PatchDocsNode` expects (strict JSON reply).
#[derive(Debug, Deserialize)]
struct PatchDocsOutput {
    summary: String,
    #[serde(default)]
    files_patched: Vec<String>,
    /// Docs created from scratch in BOOTSTRAP MODE. Kept distinct from
    /// `files_patched` because a reviewer needs to know which files are new.
    #[serde(default)]
    created: Vec<String>,
    /// Paths flagged `NEEDS_REVIEW`: a top-level architecture/overview/index
    /// doc that needs changing, or a doc needing a genuine rewrite.
    ///
    /// **Without this field the flagging instruction is worse than absent** —
    /// the model is told to flag rather than edit and given nowhere to put the
    /// flag, so it either edits the file anyway or drops the finding silently.
    #[serde(default)]
    flagged: Vec<String>,
}

/// JSON schema matching [`PatchDocsOutput`], passed as `Config.json_schema`
/// so `claude-code-rs` requests (and pre-parses) a schema-constrained reply
/// via `Outcome.structured_output` instead of relying solely on prompt text.
///
/// `created` and `flagged` were added by the JS prompt port
/// (`EN.ticket.prompt-parity-with-the-js-engines`, stage 3). Both are
/// `#[serde(default)]` and stay out of `required`, so a model that omits them
/// still parses and the port is behavior-stable.
fn patch_docs_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "files_patched": { "type": "array", "items": { "type": "string" } },
            "created": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Docs created from scratch in BOOTSTRAP MODE.",
            },
            "flagged": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths flagged NEEDS_REVIEW rather than edited, each with the \
                                specific gaps: a top-level architecture/overview/index doc, or \
                                a doc needing a genuine rewrite.",
            },
        },
        "required": ["summary"],
    })
}

/// Run-invariant preamble prepended to [`PatchDocsNode`]'s prompt — the bin-1
/// half of the JS engines' docs stage
/// (`base-template/.claude/workflows/sdlc-flow.js`), ported under
/// `EN.ticket.prompt-parity-with-the-js-engines`.
///
/// The prompt this replaces was four lines — *"Search docs/ for stale
/// references to the following modified files/symbols and patch them"* — and
/// never said **how** to patch: surgically or wholesale, what to leave alone,
/// what to escalate. It also had no bootstrap path at all, so a repo with no
/// `docs/` got a silent no-op from this stage.
///
/// **Classification.** Ported: the surgical-scope framing, the grep-for-
/// references method, BOOTSTRAP MODE, the four patch rules (never rewrite a
/// whole file, source is authoritative, never delete a still-existing
/// documented item, never edit CLAUDE.md / no emoji), the `write-repo-doc`
/// standard with its four gap types in priority order, and the
/// flag-don't-edit rule for top-level docs.
///
/// **Dropped as bin 2:** JS's step 5 `git add`/`git commit` sequence and the
/// D46 vaulted-`planning/` commit recipe — engine-rs commits in Rust
/// (`super::commit_all`), and a prompted commit is one a model can skip or get
/// wrong. `docs_prompt_never_asks_the_model_to_commit` pins that. The vault
/// recipe is **also bin 3** (fleet-specific routing). Dropped as bin 2:
/// `renderDocsStateWriteRecipe` — Rust nodes write state. Dropped as bin 4:
/// the `Return via StructuredOutput:` field list, carried by
/// [`patch_docs_output_schema`] instead — except `created`/`flagged`, which
/// needed real schema fields before the instructions naming them could mean
/// anything.
///
/// The `write-repo-doc` reference is a soft dependency: the skill lives in
/// `.claude/skills/write-repo-doc/`, delivered by the harness sync. Where a
/// target repo lacks it, the instruction degrades to the four self-contained
/// gap bullets spelled out below, which is why they are spelled out.
///
/// Carries no per-run value (standing rule 6) — the modified-files list stays
/// in the per-call body, built in `PatchDocsNode`'s prompt-builder closure.
pub(super) const DOCS_STABLE_PROMPT: &str = include_str!("prompts/docs.md");

/// Model node (Sonnet): patches documentation referencing the task's
/// modified files. Composes a `AgentCodeStep` under the `PatchDocsNode`
/// identity so it can post-process the model's JSON output.
pub struct PatchDocsNode {
    config: Config,
    transport: TransportSlot,
    /// Taken through this node's OWN builder, never inferred from context —
    /// mirrors `ConsolidatedReviewNode::with_cancellation_token`. `None`
    /// (the default) is behavior-stable: no token, no cancellation check.
    /// Added alongside the `llm_node` trait migration — this node had no
    /// cancellation support at all before (a scope gap in its original
    /// onboarding, not a deliberate omission; see `Cancellable`'s doc
    /// comment).
    cancellation_token: Option<CancellationToken>,
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
            transport: TransportSlot::default(),
            cancellation_token: None,
        }
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

/// See `llm_node::TransportSlotted`'s doc comment — `with_meta_transport`/
/// `with_transport` are default methods over this one field. This is what
/// lets `graph.rs::registry_for_policy` route this node through a local
/// model when `policy.model_tiers.docs == ModelTier::Local`.
impl LlmTransportSlotted for PatchDocsNode {
    fn transport_slot_mut(&mut self) -> &mut TransportSlot {
        &mut self.transport
    }
}

/// See `llm_node::Cancellable`'s doc comment — `with_cancellation_token` is
/// a default method over this one field.
impl LlmCancellable for PatchDocsNode {
    fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken> {
        &mut self.cancellation_token
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
        let mut step = AgentCodeStep::with_prompt_builder(
            "PatchDocsNode",
            config,
            move |ctx: &TaskContext| {
                let modified_files = Self::collect_modified_files(ctx);
                let prompt = format!(
                    "{DOCS_STABLE_PROMPT}Search docs/ for stale references to \
                     the following modified files/symbols and patch them. \
                     Respond with strict JSON of the shape {{\"summary\": \
                     str, \"files_patched\": [str], \"created\": [str], \
                     \"flagged\": [str]}}.\n\nModified files: {}",
                    json!(modified_files)
                );
                crate::policy::apply_verbosity_directive(prompt, verbosity)
            },
        )
        .with_retry_policy(policy.transport_retry);
        step = self.transport.apply(step);
        if let Some(token) = self.cancellation_token.clone() {
            step = step.with_cancellation_token(token);
        }

        let baseline = session_baseline(&ctx);
        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("PatchDocsNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                NodeError::new("PatchDocsNode: model returned no content")
                    .with_sessions(sessions_since(&ctx, baseline))
            })?
            .to_string();

        // A docs-patch reply that survives `parse_structured_or_fenced`'s
        // fence-stripping + balanced-extraction hardening and is STILL not
        // valid JSON already has a safe non-fatal home: the `flagged`
        // field's whole job is "the model did not resolve this, a human
        // must look" (`/close-out` routes it to a ticket/carryover). An
        // unparseable reply is exactly that same "the model did not
        // resolve this" outcome one level earlier — we cannot know which
        // docs got patched, so the modified files that triggered this pass
        // are flagged wholesale for review, via the shared `ModelVerdict`
        // abstraction (`workflows/mod.rs`), instead of a fatal `NodeError`
        // that would halt the whole SDLC_FLOW run over a docs-stage reply.
        let (parsed, raw_output_preview) =
            match parse_model_verdict::<PatchDocsOutput>(&ctx, "PatchDocsNode", &content) {
                ModelVerdict::Parsed(parsed) => (parsed, None),
                ModelVerdict::Unparseable {
                    raw_preview,
                    reason,
                } => (
                    PatchDocsOutput {
                        summary: format!(
                            "PatchDocsNode: model reply could not be parsed as JSON: {reason}"
                        ),
                        files_patched: Vec::new(),
                        created: Vec::new(),
                        flagged: Self::collect_modified_files(&ctx),
                    },
                    Some(raw_preview),
                ),
            };

        let mut result = json!({
            "summary": parsed.summary,
            "files_patched": parsed.files_patched,
            // Bootstrap creations and NEEDS_REVIEW flags, stamped so
            // `/close-out` can route a flagged rewrite to a ticket or a
            // carryover instead of it dying in the model's reply.
            "created": parsed.created,
            "flagged": parsed.flagged,
            // Stamp the resolved knob values so `RunTelemetry` /
            // `PolicyAggregate` can attribute this stage's observed cost
            // to the settings that caused it (standing rule 6).
            "model_tier": policy.model_tiers.docs,
            "call_timeout_secs": policy.timeouts.docs,
            "max_turns": policy.max_turns.docs,
        });
        if let Some(raw_output_preview) = raw_output_preview {
            // Bounded, never the full reply — diagnostics for an operator
            // reading committed state, not something any Rust branch parses.
            result["raw_output_preview"] = json!(raw_output_preview);
        }
        super::carry_forward_billing(&ctx, "PatchDocsNode", &mut result);
        super::put_result(&mut ctx, "PatchDocsNode", result);

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

    // --- The ported docs preamble (prompt parity, stage 3) ----------------

    /// The scope discipline the four-line prompt lacked entirely: it said
    /// what to look for and never how to change it.
    #[test]
    fn docs_prompt_states_the_surgical_discipline() {
        let p = DOCS_STABLE_PROMPT;
        assert!(p.contains("SURGICAL"));
        assert!(p.contains("Never rewrite a whole file"));
        assert!(p.contains("Source is authoritative"));
        assert!(p.contains("Never delete a documented item that still exists"));
        assert!(p.contains("Never edit CLAUDE.md"));
    }

    /// A whole capability engine-rs did not have: a repo with no `docs/`
    /// previously got a silent no-op from this stage.
    #[test]
    fn docs_prompt_carries_bootstrap_mode() {
        let p = DOCS_STABLE_PROMPT;
        assert!(p.contains("BOOTSTRAP MODE"));
        // Wrapped across a line break in the constant, so match the two words
        // independently rather than the phrase.
        assert!(
            p.contains("OKF") && p.contains("frontmatter (type, title, description)"),
            "a bootstrapped doc that fails the corpus gate is not a doc"
        );
    }

    /// The four gap types are spelled out rather than delegated, so the
    /// instruction still works in a repo without the `write-repo-doc` skill.
    #[test]
    fn docs_prompt_carries_the_write_repo_doc_gaps_self_containedly() {
        let p = DOCS_STABLE_PROMPT;
        assert!(p.contains("write-repo-doc"));
        assert!(p.contains("no quickstart"));
        assert!(p.contains("named but not linked"));
        assert!(p.contains("defined nowhere"));
        assert!(p.contains("plain-English sentence first"));
    }

    /// **Bin 2.** engine-rs commits in Rust (`super::commit_all`); the JS
    /// text's `git add`/commit sequence and vault recipe must not come across.
    #[test]
    fn docs_prompt_never_asks_the_model_to_commit() {
        let p = DOCS_STABLE_PROMPT;
        assert!(!p.contains("git add"));
        assert!(!p.contains("git commit"));
        assert!(!p.contains("planning/"), "vault routing is bin 2 + bin 3");
    }

    /// The prompt tells the model to flag rather than edit. Without somewhere
    /// to put the flag the instruction is worse than absent: it either edits
    /// anyway or drops the finding silently.
    #[test]
    fn flagged_and_created_output_fields_exist_for_the_instructions_naming_them() {
        let schema = patch_docs_output_schema();
        assert!(schema["properties"]["flagged"].is_object());
        assert!(schema["properties"]["created"].is_object());
        assert!(DOCS_STABLE_PROMPT.contains("flagged[]"));
        assert!(DOCS_STABLE_PROMPT.contains("created[]"));
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["summary"], "both stay behavior-stable");
    }

    /// Cache discipline (standing rule 6): the modified-files list is built
    /// per call, so nothing run-varying may reach this constant.
    #[test]
    fn docs_stable_prompt_interpolates_nothing_per_run() {
        assert!(
            !DOCS_STABLE_PROMPT.contains('{'),
            "no format placeholders in the cached prefix"
        );
        assert!(DOCS_STABLE_PROMPT.ends_with("\n\n"));
    }

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
                session_id: None,
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
                session_id: None,
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
                session_id: None,
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
                session_id: None,
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

    /// A `max_turns.docs` override reaches `config.max_turns`; the built-in
    /// default leaves it `None`.
    #[tokio::test]
    async fn resolved_docs_max_turns_reaches_the_config() {
        let policy = SdlcPolicy {
            max_turns: crate::workflows::sdlc_flow::policy::StageTurnCeilings {
                docs: Some(6),
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
        assert_eq!(config.max_turns, Some(6));

        let captured = Arc::new(std::sync::Mutex::new(None));
        let node = PatchDocsNode::new().with_transport(capturing_transport(captured.clone()));
        node.process(ctx_with_policy(&SdlcPolicy::default()))
            .await
            .expect("process should succeed");
        let (config, _) = captured.lock().unwrap().clone().expect("transport called");
        assert_eq!(config.max_turns, None);
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
        // Since the JS prompt port, the run-invariant `DOCS_STABLE_PROMPT`
        // leads the built prompt and the request line follows it.
        assert!(terse_prompt.starts_with(DOCS_STABLE_PROMPT));
        assert!(terse_prompt.contains("Search docs/"));

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
            max_turns: crate::workflows::sdlc_flow::policy::StageTurnCeilings {
                docs: Some(9),
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
        assert_eq!(result["max_turns"], json!(9));
    }

    /// A genuinely-unparseable reply (e.g. a small local model giving up on
    /// the task) must degrade to the node's own non-fatal `flagged` path —
    /// the same routing convention already used for a model's own
    /// NEEDS_REVIEW flags — rather than fail the whole SDLC_FLOW run.
    #[tokio::test]
    async fn degrades_genuinely_unparseable_reply_to_flagged_instead_of_fatal_error() {
        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs", "src/bar.rs"],
                "tests_added": [],
            }),
        );
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
                text: "I could not complete this due to an internal error.".to_string(),
                is_error: false,
                api_error_status: None,
                session_id: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        }));

        let out = node
            .process(ctx)
            .await
            .expect("an unparseable docs reply must degrade, not fail the whole run");
        let result = out.nodes.get("PatchDocsNode").expect("output present");
        assert_eq!(
            result["flagged"],
            json!(["src/foo.rs", "src/bar.rs"]),
            "the modified files that triggered this pass must be flagged for human review"
        );
        assert_eq!(result["files_patched"], json!([]));
        assert!(result["summary"]
            .as_str()
            .unwrap()
            .contains("could not be parsed as JSON"));
        assert!(
            result["raw_output_preview"].as_str().is_some(),
            "the raw reply preview must be stamped for an operator to diagnose"
        );
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 3: a non-default
    /// `transport_retry` on the resolved policy changes the observed
    /// attempt count against a persistently failing transport for
    /// `PatchDocsNode`.
    #[tokio::test]
    async fn transport_retry_nondefault_changes_observed_attempts() {
        let policy = SdlcPolicy {
            transport_retry: crate::workflows::sdlc_flow::policy::TransportRetry {
                max_attempts: 4,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(&policy);

        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let transport: ModelTransport = Arc::new({
            let calls = calls.clone();
            move |_config, _prompt| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(claude_code_rs::Error::Timeout)
                })
            }
        });

        let node = PatchDocsNode::new().with_transport(transport);
        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    // --- `with_meta_transport` local-tier routing --------------------------
    //
    // Mirrors `task_loop::triage_meta_transport_stamps_local_tier_on_stubbed_
    // local_success` / `..._stamps_cloud_tier_on_local_failure_fallback` —
    // the exact pattern `graph::registry_for_policy_with_cancellation` relies
    // on to route `PatchDocsNode` through a local model when
    // `policy.model_tiers.docs == ModelTier::Local`.

    #[tokio::test]
    async fn meta_transport_stamps_local_tier_on_stubbed_local_success() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::workflows::sdlc_flow::policy::LocalConfig;

        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs"],
                "tests_added": [],
            }),
        );

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5:7b-instruct".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost = Arc::new(|_url, _body| {
            Box::pin(async {
                Ok(json!({
                    "choices": [{ "message": {
                        "content": json!({
                            "summary": "patched via local model",
                            "files_patched": ["docs/foo.md"],
                        }).to_string()
                    } }],
                    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                }))
            })
        });
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            Box::pin(async { panic!("cloud fallback must not be called when local succeeds") })
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = PatchDocsNode::new().with_meta_transport(meta_transport);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            out.nodes["PatchDocsNode"]["summary"],
            "patched via local model"
        );
        assert_eq!(out.nodes["PatchDocsNode"]["transport"]["tier"], "local");
        assert_eq!(
            out.nodes["PatchDocsNode"]["transport"]["endpoint"],
            "http://localhost:11434"
        );
    }

    #[tokio::test]
    async fn meta_transport_stamps_cloud_tier_on_local_failure_fallback() {
        use crate::nodes::openai_compat_meta_transport;
        use crate::workflows::sdlc_flow::policy::LocalConfig;

        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs"],
                "tests_added": [],
            }),
        );

        let local = LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5:7b-instruct".to_string(),
            constrained_json: false,
        };
        let local_http_post: crate::nodes::LocalHttpPost =
            Arc::new(|_url, _body| Box::pin(async { Err("connection refused".to_string()) }));
        let cloud_fallback: ModelTransport = Arc::new(|_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({
                    "summary": "patched via cloud fallback",
                    "files_patched": [],
                })
                .to_string(),
                is_error: false,
                api_error_status: None,
                session_id: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });
        let meta_transport = openai_compat_meta_transport(local, local_http_post, cloud_fallback);

        let node = PatchDocsNode::new().with_meta_transport(meta_transport);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            out.nodes["PatchDocsNode"]["summary"],
            "patched via cloud fallback"
        );
        assert_eq!(
            out.nodes["PatchDocsNode"]["transport"]["tier"], "cloud",
            "a down local endpoint must stamp the cloud fallback's actual tier"
        );
    }

    #[tokio::test]
    async fn with_meta_transport_takes_precedence_over_plain_with_transport() {
        // Mirrors `implement_task_node_meta_transport_takes_precedence_over_
        // plain_transport` (task_loop.rs) and `TransportSlot`'s own documented
        // precedence: meta wins when both are set.
        use crate::nodes::TransportInfo;

        let mut ctx = ctx_with_policy(&SdlcPolicy::default());
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": ["src/foo.rs"],
                "tests_added": [],
            }),
        );

        let plain = stub_transport(json!({
            "summary": "plain transport ran",
            "files_patched": [],
        }));
        let meta: MetaTransport = Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({
                    "summary": "meta transport ran",
                    "files_patched": [],
                })
                .to_string(),
                is_error: false,
                api_error_status: None,
                session_id: None,
                structured_output: None,
            };
            let info = TransportInfo {
                tier: "local".to_string(),
                model: "stub-model".to_string(),
                endpoint: None,
                backend: "openai_compat".to_string(),
                cost_known: true,
                extra: std::collections::BTreeMap::new(),
            };
            Box::pin(async move { Ok((outcome, info)) })
        });

        let node = PatchDocsNode::new()
            .with_transport(plain)
            .with_meta_transport(meta);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["PatchDocsNode"]["summary"], "meta transport ran");
        assert_eq!(out.nodes["PatchDocsNode"]["transport"]["tier"], "local");
    }
}
