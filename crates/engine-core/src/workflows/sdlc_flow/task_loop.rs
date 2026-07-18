//! Task-loop nodes + routers for the SDLC Flow workflow: implement -> test
//! -> triage -> review -> update/save, closing the loop back to
//! `TaskQueueRouterNode`.
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/`:
//! `task_queue_router_node.py`, `implement_task_node.py`,
//! `test_task_node.py`, `triage_task_node.py`, `review_router_node.py`,
//! `consolidated_review_node.py`, `update_task_status_node.py`,
//! `save_state_node.py`.
//!
//! Model/deterministic split (per the spec's Context Pointers):
//! `ImplementTaskNode` and `ConsolidatedReviewNode` always call a model;
//! `TriageTaskNode` is deterministic by default and only calls a model when
//! `event.llm_triage` is true. Everything else here — the routers,
//! `TestTaskNode`, `UpdateTaskStatusNode`, `SaveStateNode` — is pure Rust.

use std::path::Path;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::routing::Router;

use super::policy::{ModelTier, OutputVerbosity, ReviewMode, SdlcPolicy};
use super::schema::{SDLCState, SDLCTask, SDLCTaskStatus, SDLCTriageVerdict};
use super::setup::RESOLVED_POLICY_IDENTITY;
use super::{
    get_result, put_result, strip_json_fence, CommandOutput, CommandRunner, ModelTransport,
};

/// A stable, run-invariant system-prompt prefix used as the cache-breakpoint
/// anchor when `policy.prompt_cache` is true (lever #2b). `claude-code-rs`'s
/// `Config` has no dedicated `cache_control` field, so the seam this Config
/// type exposes is `system_prompt`: keeping it byte-identical across calls
/// gives the underlying `claude` CLI a stable prefix to cache against,
/// instead of folding the same boilerplate into the ever-changing per-call
/// prompt string.
const STABLE_SYSTEM_PROMPT: &str =
    "You are running inside the engine-rs SDLC Flow task loop. This system \
     prompt is held constant across calls so its tokens can be cached.";

/// Stage identity used to look up the resolved policy's per-stage
/// [`ModelTier`] (`policy::ModelTiers` field names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Implement,
    Triage,
    Review,
}

/// Read the resolved [`SdlcPolicy`] stamped by `SetupWorktreeNode`
/// (`setup::RESOLVED_POLICY_IDENTITY`). Falls back to the built-in default
/// (today's hardcoded behavior) when absent or unparsable — defensive, so a
/// ctx driven directly in a unit test (no `SetupWorktreeNode` run) still
/// gets sane behavior rather than an error.
fn resolved_policy(ctx: &TaskContext) -> SdlcPolicy {
    get_result(ctx, RESOLVED_POLICY_IDENTITY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// Map a stage's resolved [`ModelTier`] to a concrete `claude` CLI model
/// string. `Local` resolves to the policy's configured local model name for
/// display/bookkeeping purposes only — routing a `local`-tier stage through
/// `openai_compat_transport` instead of the real `claude` CLI is EN.3.C
/// task 5's `with_transport` injection in `graph.rs`; this function only
/// picks the tier, never the transport.
fn model_tier_to_model_string(tier: ModelTier, policy: &SdlcPolicy) -> String {
    match tier {
        ModelTier::Sonnet => "claude-sonnet-4-5".to_string(),
        ModelTier::Haiku => "claude-haiku-4-5".to_string(),
        ModelTier::Opus => "claude-opus-4-8".to_string(),
        ModelTier::Local => policy.local.model.clone(),
    }
}

/// Prompt directive injected when `output_verbosity` deviates from the
/// baseline `Normal` (lever #2a). `Normal` intentionally injects nothing so
/// the built-in-default prompt text is byte-identical to pre-EN.3.C.
fn output_verbosity_directive(verbosity: OutputVerbosity) -> Option<&'static str> {
    match verbosity {
        OutputVerbosity::Normal => None,
        OutputVerbosity::Terse => Some(
            "\n\nBe terse: keep your response as short as possible while still \
             satisfying the request. Target well under 200 output tokens where \
             the task allows it.",
        ),
        OutputVerbosity::Verbose => Some(
            "\n\nBe thorough: explain your reasoning and any trade-offs in \
             detail before giving the final answer.",
        ),
    }
}

/// Apply the resolved policy's model-tier + prompt-cache knobs to a stage's
/// `Config`, then append the `output_verbosity` directive to `prompt`.
/// Returns `(config, prompt)`.
fn apply_policy(
    mut config: Config,
    prompt: String,
    policy: &SdlcPolicy,
    stage: Stage,
) -> (Config, String) {
    let tier = match stage {
        Stage::Implement => policy.model_tiers.implement,
        Stage::Triage => policy.model_tiers.triage,
        Stage::Review => policy.model_tiers.review,
    };
    config.model = Some(model_tier_to_model_string(tier, policy));

    if policy.prompt_cache {
        config.system_prompt = Some(STABLE_SYSTEM_PROMPT.to_string());
    }

    let prompt = match output_verbosity_directive(policy.output_verbosity) {
        Some(directive) => format!("{prompt}{directive}"),
        None => prompt,
    };

    (config, prompt)
}

/// Deterministically classify the current task's diff as "trivial" against
/// the resolved policy's `review_skip_max_files`/`review_skip_max_diff_lines`
/// thresholds (lever #3a, `trivial_skip` mode) — zero model tokens spent.
/// Reads `git diff --numstat main..HEAD` via the injectable [`CommandRunner`]
/// seam: one line per changed file, `<added>\t<deleted>\t<path>`. Any
/// unparsable line (e.g. a binary file's `-\t-\tpath`) is treated
/// conservatively as non-trivial. Falls back to non-trivial (`false`) when
/// the worktree path or the `git diff` invocation is unavailable, so this
/// never turns a `process` failure into an error path — trivial-skip is an
/// optimization, not a correctness requirement.
fn classify_trivial(ctx: &TaskContext, runner: &CommandRunner, policy: &SdlcPolicy) -> bool {
    let Ok(worktree) = worktree_path(ctx) else {
        return false;
    };
    let Ok(output) = runner(
        "git",
        &["diff", "--numstat", "main..HEAD"],
        Path::new(&worktree),
    ) else {
        return false;
    };

    let mut files_changed: u32 = 0;
    let mut diff_lines: u32 = 0;
    for line in output.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        files_changed += 1;
        let mut parts = line.split_whitespace();
        let added = parts.next().and_then(|s| s.parse::<u32>().ok());
        let deleted = parts.next().and_then(|s| s.parse::<u32>().ok());
        match (added, deleted) {
            (Some(a), Some(d)) => diff_lines = diff_lines.saturating_add(a).saturating_add(d),
            // Binary or otherwise unparsable numstat line: unknown size,
            // classify conservatively as non-trivial.
            _ => return false,
        }
    }

    files_changed <= policy.review_skip_max_files && diff_lines <= policy.review_skip_max_diff_lines
}

/// A review with more than this many distinct issues is treated as a
/// structural failure (re-implementation is unlikely to converge) rather
/// than a minor, fixable one. Mirrors
/// `review_router_node._STRUCTURAL_ISSUE_THRESHOLD` in Python.
const STRUCTURAL_ISSUE_THRESHOLD: usize = 5;

/// Return the most recently mutated `SDLCState` among every node identity
/// that can write one: `IncrementAttemptNode` (the retry back-edge target,
/// EN.3.B), `UpdateTaskStatusNode` (a task's eventual PASS/MAJOR_BAIL), and
/// `LoadTaskStateNode` (the initial load). Mirrors the `_latest_state_dict`
/// helper shared by `TaskQueueRouterNode`/`UpdateTaskStatusNode`/
/// `SaveStateNode` in Python, extended for the new retry-increment source.
///
/// A fixed priority order (`IncrementAttemptNode` before `UpdateTaskStatusNode`
/// before `LoadTaskStateNode`) is NOT correct here: across a whole run,
/// `IncrementAttemptNode` may hold a *stale* entry from an earlier task's
/// retries while a *later* task's `UpdateTaskStatusNode` write is actually
/// the newest state (or vice versa, mid-retry, within the same task). Instead
/// this compares each candidate's `telemetry.total_attempts` — a counter every
/// state-mutating node in this loop increments by exactly one on every write,
/// so it is a monotonically increasing logical clock for the whole run — and
/// keeps whichever candidate's value is highest. No wall-clock/`node_runs`
/// dependency needed.
fn latest_state(ctx: &TaskContext) -> Result<SDLCState, NodeError> {
    let mut best: Option<SDLCState> = None;
    for identity in [
        "IncrementAttemptNode",
        "UpdateTaskStatusNode",
        "LoadTaskStateNode",
    ] {
        let Some(value) = get_result(ctx, identity) else {
            continue;
        };
        let state: SDLCState = serde_json::from_value(value.clone())
            .map_err(|err| NodeError::new(format!("failed to parse SDLCState: {err}")))?;
        let is_newer = best
            .as_ref()
            .map(|current| state.telemetry.total_attempts > current.telemetry.total_attempts)
            .unwrap_or(true);
        if is_newer {
            best = Some(state);
        }
    }
    best.ok_or_else(|| {
        NodeError::new(
            "no SDLCState found: none of IncrementAttemptNode, UpdateTaskStatusNode, \
             LoadTaskStateNode has run",
        )
    })
}

fn worktree_path(ctx: &TaskContext) -> Result<String, NodeError> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new("SetupWorktreeNode output missing worktree_path"))
}

fn current_task_fields(ctx: &TaskContext) -> Result<&serde_json::Value, NodeError> {
    get_result(ctx, "TaskQueueRouterNode")
        .ok_or_else(|| NodeError::new("TaskQueueRouterNode has not run yet"))
}

// --- TaskQueueRouterNode ---------------------------------------------------

/// Deterministic router that dispatches the next `PENDING` task or ends the
/// task loop by routing to the `PatchDocsNode` identity (an EN.3.B stub
/// terminal here).
///
/// `Router::route(&self, ctx: &TaskContext)` takes `&ctx` and cannot mutate
/// it, but the Python node writes its own output as a side effect of
/// routing. That write is moved into `Node::process` here (run by the
/// framework before `route` is consulted for a router — see
/// `crate::workflow`), so `process` decides+stores the current task's
/// fields and `route` stays a pure read of the same state to pick
/// `ImplementTaskNode` vs `PatchDocsNode`.
pub struct TaskQueueRouterNode;

impl TaskQueueRouterNode {
    /// Find the first `PENDING` task in `state`, if any.
    fn next_pending(state: &SDLCState) -> Option<&SDLCTask> {
        state
            .tasks
            .iter()
            .find(|task| task.status == SDLCTaskStatus::Pending)
    }
}

#[async_trait::async_trait]
impl Node for TaskQueueRouterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let state = latest_state(&ctx)?;
        if let Some(task) = Self::next_pending(&state) {
            put_result(
                &mut ctx,
                "TaskQueueRouterNode",
                json!({
                    "current_task_id": task.task_id,
                    "title": task.title,
                    "description": task.description,
                    "acceptance_criteria": task.acceptance_criteria,
                    "attempt_count": task.attempt_count,
                    "max_attempts": task.max_attempts,
                }),
            );
        }
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TaskQueueRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TaskQueueRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let state = latest_state(ctx).ok()?;
        if Self::next_pending(&state).is_some() {
            Some("ImplementTaskNode".to_string())
        } else {
            Some("PatchDocsNode".to_string())
        }
    }
}

// --- ImplementTaskNode ------------------------------------------------------

/// Model node (Sonnet): drives Claude Code to implement the current task.
/// Composes a `ClaudeCodeStep` under its own identity so it can post-process
/// the model's JSON output into `{summary, modified_files, tests_added}`.
pub struct ImplementTaskNode {
    config: Config,
    transport: Option<ModelTransport>,
}

/// Model output shape `ImplementTaskNode` expects. Non-JSON model output is
/// tolerated (the loop doesn't route on these fields) by falling back to the
/// raw text as `summary` with empty vecs.
#[derive(Debug, Deserialize)]
struct ImplementOutput {
    summary: String,
    #[serde(default)]
    modified_files: Vec<String>,
    #[serde(default)]
    tests_added: Vec<String>,
}

impl ImplementTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
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

    /// Override the base `Config` entirely (model/tool-permission/etc.
    /// fields) — `process` still overwrites `model` per the resolved
    /// policy and `cwd` per `SetupWorktreeNode`'s worktree path, but every
    /// other field (e.g. `disallowed_tools`, `dangerously_skip_permissions`)
    /// passes through untouched. Live/manual tests use this to grant real
    /// tool-use permission for a genuine agentic session without changing
    /// this node's own safe-by-default `new()` construction.
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for ImplementTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ImplementTaskNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current = current_task_fields(&ctx)?.clone();
        let title = current
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let description = current
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let acceptance_criteria = current
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));

        let prompt = format!(
            "Implement the following SDLC task. Respond with strict JSON of \
             the shape {{\"summary\": str, \"modified_files\": [str], \
             \"tests_added\": [str]}}.\n\nTitle: {title}\nDescription: \
             {description}\nAcceptance criteria: {acceptance_criteria}"
        );

        let policy = resolved_policy(&ctx);
        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Implement);
        // Scope the model's session to the actual worktree so it edits the
        // right checkout rather than inheriting the host process's ambient
        // cwd. Best-effort: a ctx driven directly (no `SetupWorktreeNode`
        // run, e.g. a unit test) falls back to today's behavior (no `cwd`
        // override) instead of failing the node.
        if let Ok(worktree) = worktree_path(&ctx) {
            config.cwd = Some(std::path::PathBuf::from(worktree));
        }

        let mut step = ClaudeCodeStep::new("ImplementTaskNode", config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;

        let content = ctx
            .nodes
            .get("ImplementTaskNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();

        let parsed: ImplementOutput =
            serde_json::from_str(strip_json_fence(&content)).unwrap_or(ImplementOutput {
                summary: content.clone(),
                modified_files: Vec::new(),
                tests_added: Vec::new(),
            });

        put_result(
            &mut ctx,
            "ImplementTaskNode",
            json!({
                "summary": parsed.summary,
                "modified_files": parsed.modified_files,
                "tests_added": parsed.tests_added,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ImplementTaskNode"
    }
}

// --- TestTaskNode ------------------------------------------------------------

/// Outcome of a single harness check. Mirrors Python's `CheckResult`.
#[derive(Debug, Clone, serde::Serialize)]
struct CheckResult {
    name: String,
    kind: String,
    passed: bool,
    #[serde(default)]
    output: String,
    #[serde(default)]
    message: String,
}

/// Deterministic node: runs the worktree's `planning/harness.json`
/// validation suite via the injectable [`CommandRunner`] seam so tests can
/// drive fail-then-pass across attempts without a real subprocess.
///
/// Only the `command` check kind (the default) is fully ported; the richer
/// kinds (`forbidden-pattern-scan`, `baseline-diff`, `count-delta`,
/// `warning-scan`) are left as a documented reduced-scope TODO for EN.3.B+
/// (see this spec's Amendment Log) — an unsupported kind fails closed with a
/// message noting the gap, so a harness.json using them doesn't silently
/// pass.
pub struct TestTaskNode {
    runner: CommandRunner,
}

impl TestTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
        }
    }

    /// Override the command runner used for check invocations. Tests use
    /// this to stub the subprocess so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    fn run_command_check(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let kind = check
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("command")
            .to_string();
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");

        let outcome = (self.runner)("sh", &["-c", command], worktree);
        match outcome {
            Ok(CommandOutput {
                status,
                stdout,
                stderr,
            }) => {
                let passed = status == 0;
                CheckResult {
                    name,
                    kind,
                    passed,
                    output: format!("{stdout}{stderr}"),
                    message: if passed {
                        String::new()
                    } else {
                        format!("exit code {status}")
                    },
                }
            }
            Err(err) => CheckResult {
                name,
                kind,
                passed: false,
                output: String::new(),
                message: format!("failed to spawn check: {err}"),
            },
        }
    }

    fn run_unsupported_kind(&self, check: &serde_json::Value, kind: &str) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        CheckResult {
            name,
            kind: kind.to_string(),
            passed: false,
            output: String::new(),
            message: format!(
                "check kind {kind:?} is not yet supported by TestTaskNode \
                 (TODO(EN.3.B+): richer harness kinds)"
            ),
        }
    }

    fn run_checks(
        &self,
        checks: &[serde_json::Value],
        worktree: &Path,
    ) -> (Vec<CheckResult>, Vec<String>) {
        let mut results = Vec::new();
        let mut failed_names = Vec::new();

        for check in checks {
            if check.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }

            let kind = check
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("command")
                .to_string();
            let result = if kind == "command" {
                self.run_command_check(check, worktree)
            } else {
                self.run_unsupported_kind(check, &kind)
            };

            let gates = check.get("gates").and_then(|v| v.as_bool()).unwrap_or(true);
            if gates && !result.passed {
                failed_names.push(result.name.clone());
            }
            results.push(result);
        }

        (results, failed_names)
    }
}

impl Default for TestTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for TestTaskNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let worktree = Path::new(&worktree);
        let harness_path = worktree.join("planning").join("harness.json");

        if !harness_path.exists() {
            put_result(
                &mut ctx,
                "TestTaskNode",
                json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
            );
            return Ok(ctx);
        }

        let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
            NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
        })?;
        let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
            NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
        })?;
        let checks: Vec<serde_json::Value> = harness
            .get("validation")
            .and_then(|v| v.get("checks"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let (check_results, failed_names) = self.run_checks(&checks, worktree);
        let all_passed = failed_names.is_empty();
        let failure_summary = if all_passed {
            String::new()
        } else {
            format!("Failed checks: {}", failed_names.join(", "))
        };

        put_result(
            &mut ctx,
            "TestTaskNode",
            json!({
                "all_passed": all_passed,
                "check_results": check_results,
                "failure_summary": failure_summary,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TestTaskNode"
    }
}

// --- TriageTaskNode -----------------------------------------------------------

/// Node that classifies a task's test-failure output into
/// `PASS`/`RETRYABLE`/`MAJOR_BAIL`. Deterministic by default (a passing test
/// forces `PASS`; an over-budget task forces `MAJOR_BAIL`; a failing task
/// still under budget is deterministically `RETRYABLE`), consulting a
/// `ClaudeCodeStep` (Sonnet) only when `event.llm_triage` is true.
pub struct TriageTaskNode {
    config: Config,
    transport: Option<ModelTransport>,
    runner: CommandRunner,
}

#[derive(Debug, Deserialize)]
struct TriageOutput {
    verdict: String,
    reason: String,
}

impl TriageTaskNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: None,
            runner: super::default_command_runner(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep` for the
    /// `llm_triage` model branch. Tests use this to assert it is (or isn't)
    /// invoked.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Override the command runner used for the `git diff --numstat`
    /// trivial-classification invocation. Tests use this to stub the
    /// subprocess.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the base `Config` entirely. Live/manual tests use this to
    /// set `isolated: true` when driving a real `claude` call from inside
    /// another interactive session (see `Config::isolated`'s doc comment).
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for TriageTaskNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for TriageTaskNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let test_result = get_result(&ctx, "TestTaskNode")
            .cloned()
            .ok_or_else(|| NodeError::new("TestTaskNode has not run yet"))?;
        let current = current_task_fields(&ctx)?.clone();
        let current_task_id = current
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;

        // `attempt_count`/`max_attempts` must come from the *live* durable
        // state, not `current` (`TaskQueueRouterNode`'s snapshot, frozen at
        // task dispatch): `IncrementAttemptNode` mutates the durable state on
        // every retry back-edge but never re-runs `TaskQueueRouterNode`, so a
        // read from `current` would see `attempt_count == 0` forever and the
        // bail gate below would never fire. See this spec's Amendment Log
        // (EN.3.B retry-bail fix).
        let state = latest_state(&ctx)?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.task_id == current_task_id)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "TriageTaskNode: no task with task_id={current_task_id} found in state"
                ))
            })?;
        let attempt_count = u64::from(task.attempt_count);
        let max_attempts = u64::from(task.max_attempts);

        let all_passed = test_result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if all_passed {
            let policy = resolved_policy(&ctx);
            let trivial = classify_trivial(&ctx, &self.runner, &policy);
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "PASS",
                    "reason": "All harness checks passed.",
                    "trivial": trivial,
                }),
            );
            return Ok(ctx);
        }

        if attempt_count >= max_attempts {
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "MAJOR_BAIL",
                    "reason": format!(
                        "Max attempts ({max_attempts}) reached without a passing run."
                    ),
                }),
            );
            return Ok(ctx);
        }

        let llm_triage = ctx
            .event
            .get("llm_triage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !llm_triage {
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({
                    "verdict": "RETRYABLE",
                    "reason": format!(
                        "Checks failed; retrying (attempt {} of {max_attempts}).",
                        attempt_count + 1
                    ),
                }),
            );
            return Ok(ctx);
        }

        let failure_summary = test_result
            .get("failure_summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let title = current
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let prompt = format!(
            "Classify this task's test failure as RETRYABLE or MAJOR_BAIL. \
             Respond with strict JSON of the shape {{\"verdict\": str, \
             \"reason\": str}}.\n\nTask: {title}\nAttempt {} of \
             {max_attempts}.\nFailure summary: {failure_summary}",
            attempt_count + 1
        );

        let policy = resolved_policy(&ctx);
        let (config, prompt) = apply_policy(self.config.clone(), prompt, &policy, Stage::Triage);

        let mut step = ClaudeCodeStep::new("TriageTaskNode", config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;
        let content = ctx
            .nodes
            .get("TriageTaskNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("TriageTaskNode: model returned no content"))?
            .to_string();

        let parsed: TriageOutput =
            serde_json::from_str(strip_json_fence(&content)).map_err(|err| {
                NodeError::new(format!(
                    "TriageTaskNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            "TriageTaskNode",
            // Same normalization as `ConsolidatedReviewNode`'s, and for the
            // same reason — `TriageRouterNode` exact-matches this string.
            json!({ "verdict": parsed.verdict.trim().to_uppercase(), "reason": parsed.reason }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TriageTaskNode"
    }
}

/// Deterministic router: branches on `TriageTaskNode`'s stored verdict.
pub struct TriageRouterNode;

#[async_trait::async_trait]
impl Node for TriageRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TriageRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TriageRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let triage = get_result(ctx, "TriageTaskNode")?;
        let verdict = triage.get("verdict")?.as_str()?;
        match verdict {
            // `review_mode` (lever #3a) decides whether a `PASS` verdict
            // still routes to `ConsolidatedReviewNode`:
            //   - `PerTask` (built-in default): unchanged, always review —
            //     reproduces pre-EN.3.C behavior byte-for-byte.
            //   - `EndOnly`: per-task review is collapsed away entirely (a
            //     single end-of-run review happens elsewhere), so every
            //     `PASS` skips straight to `UpdateTaskStatusNode`.
            //   - `TrivialSkip`: only a task `TriageTaskNode` classified
            //     `trivial` (small diff under `review_skip_max_files`/
            //     `review_skip_max_diff_lines`) skips review; a non-trivial
            //     `PASS` still routes to `ConsolidatedReviewNode`.
            "PASS" => {
                let policy = resolved_policy(ctx);
                match policy.review_mode {
                    ReviewMode::PerTask => Some("ConsolidatedReviewNode".to_string()),
                    ReviewMode::EndOnly => Some("UpdateTaskStatusNode".to_string()),
                    ReviewMode::TrivialSkip => {
                        let trivial = triage
                            .get("trivial")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if trivial {
                            Some("UpdateTaskStatusNode".to_string())
                        } else {
                            Some("ConsolidatedReviewNode".to_string())
                        }
                    }
                }
            }
            // Retry back-edge (EN.3.B fix): routes through `IncrementAttemptNode`
            // first (not straight to `ImplementTaskNode`) so the durable
            // `attempt_count`/`total_attempts` counters actually advance —
            // `Router::route` takes `&ctx` and cannot mutate state itself.
            "RETRYABLE" => Some("IncrementAttemptNode".to_string()),
            "MAJOR_BAIL" => Some("WrapUpNode".to_string()),
            _ => None,
        }
    }
}

// --- ConsolidatedReviewNode ----------------------------------------------------

/// Model node (Sonnet): reviews the task's `git diff main..HEAD` against its
/// acceptance criteria via a composed `ClaudeCodeStep`.
pub struct ConsolidatedReviewNode {
    config: Config,
    transport: Option<ModelTransport>,
    runner: CommandRunner,
}

#[derive(Debug, Deserialize)]
struct ReviewOutput {
    verdict: String,
    summary: String,
    #[serde(default)]
    issues: Vec<String>,
}

impl ConsolidatedReviewNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: None,
            runner: super::default_command_runner(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Override the command runner used for the `git diff` invocation.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the base `Config` entirely. Live/manual tests use this to
    /// set `isolated: true` when driving a real `claude` call from inside
    /// another interactive session (see `Config::isolated`'s doc comment).
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for ConsolidatedReviewNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ConsolidatedReviewNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let current = current_task_fields(&ctx)?.clone();
        let acceptance_criteria = current
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!([]));

        let diff = (self.runner)("git", &["diff", "main..HEAD"], Path::new(&worktree))
            .map(|output| output.stdout)
            .unwrap_or_default();

        let prompt = format!(
            "Review this task's diff against its acceptance criteria. \
             Respond with strict JSON of the shape {{\"verdict\": str, \
             \"summary\": str, \"issues\": [str]}}.\n\nAcceptance criteria: \
             {acceptance_criteria}\n\nDiff:\n{diff}"
        );

        let policy = resolved_policy(&ctx);
        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Review);
        // Scope the model's session to the actual worktree, matching
        // `ImplementTaskNode`'s fix — without this, a real call that reads
        // the filesystem checks the host process's ambient cwd instead of
        // the task's worktree (observed live: the model correctly reported
        // the file it was asked to review as "missing", because it was
        // looking in the wrong directory).
        config.cwd = Some(std::path::PathBuf::from(&worktree));

        let mut step = ClaudeCodeStep::new("ConsolidatedReviewNode", config, prompt);
        if let Some(transport) = self.transport.clone() {
            step = step.with_transport(move |config, prompt| (transport)(config, prompt));
        }

        let mut ctx = step.process(ctx).await?;
        let content = ctx
            .nodes
            .get("ConsolidatedReviewNode")
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new("ConsolidatedReviewNode: model returned no content"))?
            .to_string();

        let parsed: ReviewOutput =
            serde_json::from_str(strip_json_fence(&content)).map_err(|err| {
                NodeError::new(format!(
                    "ConsolidatedReviewNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            "ConsolidatedReviewNode",
            json!({
                // Normalized to the canonical uppercase form `ReviewRouterNode`
                // matches on — a real model reply doesn't reliably preserve
                // the exact casing asked for (observed live: a real Sonnet
                // reply returned "pass"), and an un-normalized mismatch here
                // makes the router's exact match fail closed to `None`,
                // silently halting the whole run at this node.
                "verdict": parsed.verdict.trim().to_uppercase(),
                "summary": parsed.summary,
                "issues": parsed.issues,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ConsolidatedReviewNode"
    }
}

/// Deterministic router: branches on `ConsolidatedReviewNode`'s stored
/// verdict, distinguishing "structural" from "minor" failures by issue
/// count.
pub struct ReviewRouterNode;

#[async_trait::async_trait]
impl Node for ReviewRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ReviewRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for ReviewRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let review = get_result(ctx, "ConsolidatedReviewNode")?;
        let verdict = review.get("verdict")?.as_str()?;
        let issues = review
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|v| v.len())
            .unwrap_or(0);

        match verdict {
            "PASS" => Some("UpdateTaskStatusNode".to_string()),
            "FAIL" | "PARTIAL" => {
                if issues == 0 || issues > STRUCTURAL_ISSUE_THRESHOLD {
                    Some("WrapUpNode".to_string())
                } else {
                    // Minor-issue retry back-edge (EN.3.B fix): same reasoning
                    // as `TriageRouterNode`'s `RETRYABLE` branch — route
                    // through `IncrementAttemptNode` so the retry counters
                    // advance in lockstep across both back-edges.
                    Some("IncrementAttemptNode".to_string())
                }
            }
            _ => None,
        }
    }
}

/// Bump the task identified by `task_id`'s `attempt_count` and the state's
/// `telemetry.total_attempts`, both by exactly one. Shared by
/// [`IncrementAttemptNode`] (the live retry back-edge target) and
/// `UpdateTaskStatusNode`'s now-unreachable `Retryable` arm (see its doc
/// comment) so the two counters can never drift apart.
fn bump_attempt(state: &mut SDLCState, task_id: u32) -> Result<(), NodeError> {
    let spec_slug = state.spec_slug.clone();
    let task = state
        .tasks
        .iter_mut()
        .find(|task| task.task_id == task_id)
        .ok_or_else(|| {
            NodeError::new(format!(
                "no task with task_id={task_id} found in state for spec {spec_slug:?}"
            ))
        })?;
    task.attempt_count += 1;
    state.telemetry.total_attempts += 1;
    Ok(())
}

// --- IncrementAttemptNode ---------------------------------------------------

/// Deterministic node: the retry back-edge target for both
/// `TriageRouterNode`'s `RETRYABLE` verdict and `ReviewRouterNode`'s minor
/// `FAIL`/`PARTIAL` verdict (EN.3.B retry-bail fix). Bumps the current
/// task's `attempt_count` and `telemetry.total_attempts` in the durable
/// `SDLCState` via [`bump_attempt`], then hands off to `ImplementTaskNode`
/// for the retry (the forward hop is a declared graph connection — see
/// `graph.rs`).
///
/// `Router::route(&self, ctx: &TaskContext)` takes `&ctx` and cannot mutate
/// state, so the increment cannot live in `TriageRouterNode`'s or
/// `ReviewRouterNode`'s routing logic itself — it must be a real `Node`
/// sitting on both back-edges, between the router and `ImplementTaskNode`.
pub struct IncrementAttemptNode;

#[async_trait::async_trait]
impl Node for IncrementAttemptNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;

        let mut state = latest_state(&ctx)?;
        bump_attempt(&mut state, current_task_id)?;

        let value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "IncrementAttemptNode", value);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "IncrementAttemptNode"
    }
}

// --- UpdateTaskStatusNode --------------------------------------------------

/// Deterministic node: mutates the current task's status in the durable
/// `SDLCState`, keeping `SDLCTelemetry` counters in lockstep.
pub struct UpdateTaskStatusNode;

#[async_trait::async_trait]
impl Node for UpdateTaskStatusNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current_task_id = current_task_fields(&ctx)?
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;
        let verdict_str = get_result(&ctx, "TriageTaskNode")
            .and_then(|v| v.get("verdict"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| NodeError::new("TriageTaskNode has not run yet"))?
            .to_string();
        let verdict: SDLCTriageVerdict = match verdict_str.as_str() {
            "PASS" => SDLCTriageVerdict::Pass,
            "RETRYABLE" => SDLCTriageVerdict::Retryable,
            "MAJOR_BAIL" => SDLCTriageVerdict::MajorBail,
            other => {
                return Err(NodeError::new(format!(
                    "UpdateTaskStatusNode: unknown triage verdict {other:?}"
                )))
            }
        };

        let mut state = latest_state(&ctx)?;
        let spec_slug = state.spec_slug.clone();

        match verdict {
            SDLCTriageVerdict::Pass => {
                let task = state
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_id == current_task_id)
                    .ok_or_else(|| {
                        NodeError::new(format!(
                            "UpdateTaskStatusNode: no task with task_id={current_task_id} found \
                             in state for spec {spec_slug:?}"
                        ))
                    })?;
                task.status = SDLCTaskStatus::Done;
                state.telemetry.tasks_passed += 1;
                state.telemetry.total_attempts += 1;
            }
            SDLCTriageVerdict::MajorBail => {
                let task = state
                    .tasks
                    .iter_mut()
                    .find(|task| task.task_id == current_task_id)
                    .ok_or_else(|| {
                        NodeError::new(format!(
                            "UpdateTaskStatusNode: no task with task_id={current_task_id} found \
                             in state for spec {spec_slug:?}"
                        ))
                    })?;
                task.status = SDLCTaskStatus::Failed;
                state.telemetry.tasks_failed += 1;
                state.telemetry.total_attempts += 1;
            }
            SDLCTriageVerdict::Retryable => {
                // Unreachable via the assembled graph as of EN.3.B: both
                // `TriageRouterNode`'s `RETRYABLE` and `ReviewRouterNode`'s
                // minor `FAIL`/`PARTIAL` back-edges now target
                // `IncrementAttemptNode` directly (see their `Router::route`
                // impls above), so `UpdateTaskStatusNode` is only ever
                // reached with a `PASS` verdict (via `ReviewRouterNode`) —
                // never `RETRYABLE`. Kept for defensive completeness and
                // direct unit-test coverage (`update_status_mutations`),
                // reusing `bump_attempt` so the counters can't drift apart
                // from `IncrementAttemptNode`'s if this is ever hit.
                bump_attempt(&mut state, current_task_id)?;
            }
        }

        let value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        put_result(&mut ctx, "UpdateTaskStatusNode", value);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "UpdateTaskStatusNode"
    }
}

// --- SaveStateNode ----------------------------------------------------------

/// Deterministic node: serializes the latest `SDLCState` to
/// `planning/{spec_slug}/sdlc-flow-state.json` inside the worktree and
/// commits it via the injectable [`CommandRunner`] seam, so state survives
/// across resumed runs. A non-zero `git commit` (e.g. "nothing to commit")
/// is logged, not treated as a failure — mirrors the Python behavior.
pub struct SaveStateNode {
    runner: CommandRunner,
}

impl SaveStateNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: super::default_command_runner(),
        }
    }

    /// Override the command runner used for the `git add`/`git commit`
    /// invocations. Tests use this to stub the subprocess.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for SaveStateNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for SaveStateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let worktree = worktree_path(&ctx)?;
        let state = latest_state(&ctx)?;

        let state_dir = Path::new(&worktree).join("planning").join(&state.spec_slug);
        std::fs::create_dir_all(&state_dir).map_err(|err| {
            NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
        })?;
        let state_path = state_dir.join("sdlc-flow-state.json");
        let json = serde_json::to_string_pretty(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        std::fs::write(&state_path, json).map_err(|err| {
            NodeError::new(format!("failed to write {}: {err}", state_path.display()))
        })?;

        let state_path_str = state_path.to_string_lossy().to_string();
        let _ = (self.runner)("git", &["add", &state_path_str], Path::new(&worktree));
        let commit = (self.runner)(
            "git",
            &["commit", "-m", "chore: flow state update"],
            Path::new(&worktree),
        );
        if let Ok(output) = &commit {
            if output.status != 0 {
                // "nothing to commit" or an equivalent no-op — logged, not
                // an error, mirroring `save_state_node.py`.
                log_noop_commit(&output.stderr);
            }
        }

        put_result(
            &mut ctx,
            "SaveStateNode",
            json!({ "saved_to": state_path_str }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SaveStateNode"
    }
}

/// Best-effort no-op logging hook for a non-fatal `git commit` outcome
/// (e.g. "nothing to commit, working tree clean"). Kept as a tiny named
/// function rather than an inline `eprintln!` so its intent — "logged, not
/// an error" — reads at the call site.
fn log_noop_commit(_stderr: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::policy::ModelTiers;
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn state_with_tasks(tasks: Vec<SDLCTask>) -> SDLCState {
        let mut state = SDLCState::new("my-spec");
        state.tasks = tasks;
        state
    }

    fn ctx_with_state(state: &SDLCState) -> TaskContext {
        let mut ctx = empty_context(json!({ "spec_slug": state.spec_slug }));
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(state).unwrap(),
        );
        ctx
    }

    fn ctx_with_current_task(state: &SDLCState, task: &SDLCTask) -> TaskContext {
        let mut ctx = ctx_with_state(state);
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({
                "current_task_id": task.task_id,
                "title": task.title,
                "description": task.description,
                "acceptance_criteria": task.acceptance_criteria,
                "attempt_count": task.attempt_count,
                "max_attempts": task.max_attempts,
            }),
        );
        ctx
    }

    fn panicking_transport() -> ModelTransport {
        Arc::new(|_config, _prompt| {
            panic!("transport should not be invoked for a deterministic branch")
        })
    }

    // --- TaskQueueRouterNode -----------------------------------------------

    #[tokio::test]
    async fn task_queue_dispatches_first_pending() {
        let mut task1 = SDLCTask::new(1, "One", "d1");
        task1.status = SDLCTaskStatus::Done;
        let task2 = SDLCTask::new(2, "Two", "d2");
        let state = state_with_tasks(vec![task1, task2]);
        let ctx = ctx_with_state(&state);

        let node = TaskQueueRouterNode;
        let out = node.process(ctx).await.expect("process should succeed");
        let result = out
            .nodes
            .get("TaskQueueRouterNode")
            .expect("output present");
        assert_eq!(result["current_task_id"], 2);

        assert_eq!(node.route(&out), Some("ImplementTaskNode".to_string()));
    }

    #[tokio::test]
    async fn task_queue_ends_on_none_pending() {
        let mut task1 = SDLCTask::new(1, "One", "d1");
        task1.status = SDLCTaskStatus::Done;
        let state = state_with_tasks(vec![task1]);
        let ctx = ctx_with_state(&state);

        let node = TaskQueueRouterNode;
        let out = node.process(ctx).await.expect("process should succeed");
        assert!(!out.nodes.contains_key("TaskQueueRouterNode"));
        assert_eq!(node.route(&out), Some("PatchDocsNode".to_string()));
    }

    // --- TriageTaskNode ------------------------------------------------------

    fn ctx_with_test_result(all_passed: bool, task: &SDLCTask) -> TaskContext {
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, task);
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": all_passed, "check_results": [], "failure_summary": "" }),
        );
        ctx
    }

    #[tokio::test]
    async fn triage_deterministic_branches() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // Passing test -> PASS, no transport call.
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(true, &task);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");

        // Over budget -> MAJOR_BAIL, no transport call.
        let mut over_budget = task.clone();
        over_budget.attempt_count = 3;
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(false, &over_budget);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");

        // Under budget + llm_triage=false (default) -> RETRYABLE, no
        // transport call.
        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let ctx = ctx_with_test_result(false, &task);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
    }

    #[tokio::test]
    async fn triage_llm_gate_invokes_model_when_enabled() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let called = Arc::new(Mutex::new(false));
        let called_clone = called.clone();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            *called_clone.lock().unwrap() = true;
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({ "verdict": "MAJOR_BAIL", "reason": "hopeless" }).to_string(),
                is_error: false,
                api_error_status: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert!(*called.lock().unwrap());
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
    }

    /// A real model reply's casing isn't guaranteed to match the prompt's
    /// literal request — a lowercase (or mixed-case) verdict is normalized
    /// to uppercase so `TriageRouterNode`'s exact match still routes
    /// correctly instead of silently falling through to `None`.
    #[tokio::test]
    async fn triage_llm_branch_normalizes_lowercase_verdict() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "retryable", "reason": "try again" }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
    }

    // --- TriageRouterNode ------------------------------------------------

    #[test]
    fn triage_router_back_edge() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "RETRYABLE", "reason": "retry" }),
        );
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("IncrementAttemptNode".to_string()));
    }

    #[test]
    fn triage_router_all_verdicts() {
        let router = TriageRouterNode;
        for (verdict, expected) in [
            ("PASS", "ConsolidatedReviewNode"),
            ("RETRYABLE", "IncrementAttemptNode"),
            ("MAJOR_BAIL", "WrapUpNode"),
        ] {
            let mut ctx = empty_context(json!({}));
            ctx.nodes.insert(
                "TriageTaskNode".to_string(),
                json!({ "verdict": verdict, "reason": "r" }),
            );
            assert_eq!(router.route(&ctx), Some(expected.to_string()));
        }
    }

    // --- IncrementAttemptNode / retry-bail (EN.3.B) -------------------------

    #[tokio::test]
    async fn increment_attempt_node_bumps_state() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;
        let out = node.process(ctx).await.expect("process should succeed");
        let bumped: SDLCState =
            serde_json::from_value(out.nodes["IncrementAttemptNode"].clone()).unwrap();

        assert_eq!(bumped.tasks[0].attempt_count, 1);
        assert_eq!(bumped.telemetry.total_attempts, 1);
    }

    #[tokio::test]
    async fn increment_attempt_node_compounds_across_retries() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;
        let out = node.process(ctx).await.expect("first retry should succeed");
        let out = node
            .process(out)
            .await
            .expect("second retry should succeed");

        // `latest_state` must pick up its own prior write (via the
        // `total_attempts` logical clock), not fall back to the stale
        // `LoadTaskStateNode` snapshot, or this would still read 1.
        let bumped: SDLCState =
            serde_json::from_value(out.nodes["IncrementAttemptNode"].clone()).unwrap();
        assert_eq!(bumped.tasks[0].attempt_count, 2);
        assert_eq!(bumped.telemetry.total_attempts, 2);
    }

    #[tokio::test]
    async fn retry_bail_fires_at_exactly_max_attempts_via_triage_back_edge() {
        // A never-passing task with max_attempts = 2: drive the loop by hand
        // (TriageTaskNode -> IncrementAttemptNode, repeated) and assert the
        // bail fires at exactly the 2nd retry attempt, never earlier, never
        // later — proving `IncrementAttemptNode` actually advances the
        // counter `TriageTaskNode`'s bail gate reads.
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 2;
        let mut ctx = ctx_with_test_result(false, &task);

        let triage = TriageTaskNode::new().with_transport(panicking_transport());
        let increment = IncrementAttemptNode;

        // Attempt 0 (initial dispatch, attempt_count == 0 < max_attempts):
        // RETRYABLE.
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        ctx = increment
            .process(ctx)
            .await
            .expect("first increment should succeed");

        // Re-seed TestTaskNode's failing result for the retry (TriageTaskNode
        // reads it fresh every pass) and triage again: attempt_count == 1 <
        // max_attempts (2) -> still RETRYABLE, one retry left.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": false, "check_results": [], "failure_summary": "" }),
        );
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
        ctx = increment
            .process(ctx)
            .await
            .expect("second increment should succeed");

        // attempt_count is now 2 == max_attempts -> MAJOR_BAIL, exactly here,
        // not before.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": false, "check_results": [], "failure_summary": "" }),
        );
        ctx = triage.process(ctx).await.expect("triage should succeed");
        assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");

        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));

        let final_state: SDLCState =
            serde_json::from_value(ctx.nodes["IncrementAttemptNode"].clone()).unwrap();
        assert_eq!(final_state.tasks[0].attempt_count, 2);
        assert_eq!(final_state.telemetry.total_attempts, 2);
    }

    #[tokio::test]
    async fn both_retry_back_edges_increment_attempt_count() {
        // TriageRouterNode's RETRYABLE and ReviewRouterNode's minor
        // FAIL/PARTIAL both route to IncrementAttemptNode; assert both
        // paths actually advance the counter (not just one of them).
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let node = IncrementAttemptNode;

        // Simulates the TriageRouterNode::RETRYABLE back-edge.
        let after_triage_retry = node.process(ctx).await.expect("process should succeed");
        let state_after_triage: SDLCState =
            serde_json::from_value(after_triage_retry.nodes["IncrementAttemptNode"].clone())
                .unwrap();
        assert_eq!(state_after_triage.tasks[0].attempt_count, 1);

        // Simulates the ReviewRouterNode minor FAIL/PARTIAL back-edge,
        // continuing from the same accumulated context.
        let after_review_retry = node
            .process(after_triage_retry)
            .await
            .expect("process should succeed");
        let state_after_review: SDLCState =
            serde_json::from_value(after_review_retry.nodes["IncrementAttemptNode"].clone())
                .unwrap();
        assert_eq!(state_after_review.tasks[0].attempt_count, 2);
        assert_eq!(state_after_review.telemetry.total_attempts, 2);
    }

    // --- ReviewRouterNode --------------------------------------------------

    fn review_ctx(verdict: &str, issue_count: usize) -> TaskContext {
        let issues: Vec<String> = (0..issue_count).map(|i| format!("issue {i}")).collect();
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": verdict, "summary": "s", "issues": issues }),
        );
        ctx
    }

    #[test]
    fn review_router_structural_vs_minor() {
        let router = ReviewRouterNode;

        assert_eq!(
            router.route(&review_ctx("FAIL", 3)),
            Some("IncrementAttemptNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("FAIL", 6)),
            Some("WrapUpNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("FAIL", 0)),
            Some("WrapUpNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("PASS", 0)),
            Some("UpdateTaskStatusNode".to_string())
        );
        assert_eq!(
            router.route(&review_ctx("PARTIAL", 2)),
            Some("IncrementAttemptNode".to_string())
        );
    }

    // --- UpdateTaskStatusNode ------------------------------------------------

    fn ctx_for_update(task: &SDLCTask, verdict: &str) -> TaskContext {
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, task);
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": verdict, "reason": "r" }),
        );
        ctx
    }

    #[tokio::test]
    async fn update_status_mutations() {
        let task = SDLCTask::new(1, "One", "d1");

        let node = UpdateTaskStatusNode;
        let ctx = ctx_for_update(&task, "PASS");
        let out = node.process(ctx).await.expect("process should succeed");
        let state: SDLCState =
            serde_json::from_value(out.nodes["UpdateTaskStatusNode"].clone()).unwrap();
        assert_eq!(state.tasks[0].status, SDLCTaskStatus::Done);
        assert_eq!(state.telemetry.tasks_passed, 1);
        assert_eq!(state.telemetry.total_attempts, 1);

        let node = UpdateTaskStatusNode;
        let ctx = ctx_for_update(&task, "RETRYABLE");
        let out = node.process(ctx).await.expect("process should succeed");
        let state: SDLCState =
            serde_json::from_value(out.nodes["UpdateTaskStatusNode"].clone()).unwrap();
        assert_eq!(state.tasks[0].attempt_count, 1);
        assert_eq!(state.tasks[0].status, SDLCTaskStatus::Pending);
        assert_eq!(state.telemetry.total_attempts, 1);

        let node = UpdateTaskStatusNode;
        let ctx = ctx_for_update(&task, "MAJOR_BAIL");
        let out = node.process(ctx).await.expect("process should succeed");
        let state: SDLCState =
            serde_json::from_value(out.nodes["UpdateTaskStatusNode"].clone()).unwrap();
        assert_eq!(state.tasks[0].status, SDLCTaskStatus::Failed);
        assert_eq!(state.telemetry.tasks_failed, 1);
    }

    #[tokio::test]
    async fn update_status_missing_task_errors() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "TaskQueueRouterNode".to_string(),
            json!({ "current_task_id": 99 }),
        );
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "PASS", "reason": "r" }),
        );

        let node = UpdateTaskStatusNode;
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no task with task_id=99"));
    }

    // --- TestTaskNode --------------------------------------------------------

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-sdlc-flow-task-loop-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("planning")).unwrap();
        dir
    }

    fn write_harness(dir: &Path, checks: serde_json::Value) {
        let harness = json!({ "validation": { "checks": checks } });
        std::fs::write(
            dir.join("planning").join("harness.json"),
            serde_json::to_string(&harness).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_task_no_harness_json_passes() {
        let worktree = temp_worktree();
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn test_task_reports_gating_failure() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "always_fail", "command": "exit 1", "gates": true }]),
        );

        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
    }

    #[tokio::test]
    async fn test_task_uses_injected_runner_for_fail_then_pass() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "check", "command": "does-not-matter", "gates": true }]),
        );

        let attempt: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let attempt_clone = attempt.clone();
        let runner: CommandRunner = Arc::new(move |_program, _args, _cwd| {
            let n = attempt_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(CommandOutput {
                status: if n == 0 { 1 } else { 0 },
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = TestTaskNode::new().with_runner(runner.clone());
        let out = node.process(ctx.clone()).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);

        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    // --- ConsolidatedReviewNode ------------------------------------------

    #[tokio::test]
    async fn review_parses_content_and_uses_diff() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let diff_called = Arc::new(Mutex::new(false));
        let diff_called_clone = diff_called.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            *diff_called_clone.lock().unwrap() = true;
            assert_eq!(args, ["diff", "main..HEAD"]);
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let canned =
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: canned.clone(),
                is_error: false,
                api_error_status: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert!(*diff_called.lock().unwrap());
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
    }

    /// The model's `Config.cwd` is scoped to the run's worktree — without
    /// this, a real review call that reads the filesystem checks the host
    /// process's ambient cwd instead of the task's actual worktree.
    #[tokio::test]
    async fn review_node_scopes_config_cwd_to_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "/tmp/some-worktree" }),
        );

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(
            config.cwd,
            Some(std::path::PathBuf::from("/tmp/some-worktree"))
        );
    }

    /// Same normalization as `TriageTaskNode`'s llm branch, and for the same
    /// reason: a real model reply's casing isn't guaranteed, and an
    /// un-normalized mismatch makes `ReviewRouterNode`'s exact match fail
    /// closed to `None` (observed live: a real reply returned "pass").
    #[tokio::test]
    async fn review_node_normalizes_lowercase_verdict() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "pass", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "PASS");
    }

    // --- Policy consumption (EN.3.C task 3) ---------------------------------

    /// Stamp a resolved [`SdlcPolicy`] into `ctx` under the same identity
    /// `SetupWorktreeNode` uses, so a node's `resolved_policy(&ctx)` read
    /// sees it.
    fn ctx_with_policy(mut ctx: TaskContext, policy: &SdlcPolicy) -> TaskContext {
        put_result(
            &mut ctx,
            RESOLVED_POLICY_IDENTITY,
            serde_json::to_value(policy).expect("SdlcPolicy serializes"),
        );
        ctx
    }

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: std::collections::BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
        }
    }

    /// A node built with `model_tiers.implement = haiku` produces a
    /// `Config` carrying the haiku model string.
    #[tokio::test]
    async fn implement_node_consumes_resolved_model_tier() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                implement: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
    }

    /// `output_verbosity = terse` injects the terseness directive into the
    /// stage prompt; the default (`normal`) leaves the prompt untouched.
    #[tokio::test]
    async fn implement_node_injects_terse_directive() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            output_verbosity: OutputVerbosity::Terse,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(prompt.contains("Be terse"));
    }

    /// When `SetupWorktreeNode` has stamped a `worktree_path`, the model's
    /// `Config.cwd` is scoped to it — so a real session edits the actual
    /// checkout rather than the host process's ambient cwd.
    #[tokio::test]
    async fn implement_node_scopes_config_cwd_to_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);
        let mut ctx = ctx;
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "/tmp/some-worktree" }),
        );

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(
            config.cwd,
            Some(std::path::PathBuf::from("/tmp/some-worktree"))
        );
    }

    /// Without a `SetupWorktreeNode` result (e.g. a unit test driving the
    /// node directly), `Config.cwd` falls back to `None` rather than
    /// failing the node — today's pre-fix behavior is preserved when no
    /// worktree is known.
    #[tokio::test]
    async fn implement_node_leaves_cwd_none_without_worktree() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.cwd, None);
    }

    /// The `normal` (default) verbosity injects no directive, reproducing
    /// the pre-EN.3.C prompt text.
    #[tokio::test]
    async fn implement_node_normal_verbosity_adds_no_directive() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);
        // No RESOLVED_POLICY_IDENTITY stamped -> falls back to built-in
        // default, which is `normal`.

        let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_prompt_clone = seen_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *seen_prompt_clone.lock().unwrap() = Some(prompt);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let prompt = seen_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert!(!prompt.contains("Be terse"));
        assert!(!prompt.contains("Be thorough"));
    }

    /// `prompt_cache = true` sets a stable `system_prompt` cache breakpoint
    /// on the composed `ClaudeCodeStep`'s `Config`; the default
    /// (`prompt_cache = false`) leaves it unset.
    #[tokio::test]
    async fn implement_node_sets_cache_breakpoint_when_prompt_cache_enabled() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task(&state, &task);

        let policy = SdlcPolicy {
            prompt_cache: true,
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = ImplementTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.system_prompt.as_deref(), Some(STABLE_SYSTEM_PROMPT));

        // Baseline: no policy stamped -> falls back to the built-in
        // default (`prompt_cache = false`) -> no breakpoint set.
        let ctx2 = ctx_with_current_task(&state, &task);
        let seen_config2: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config2_clone = seen_config2.clone();
        let transport2: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config2_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(json!({ "summary": "done" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });
        let node2 = ImplementTaskNode::new().with_transport(transport2);
        node2.process(ctx2).await.expect("process should succeed");
        let config2 = seen_config2
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config2.system_prompt, None);
    }

    /// `TriageTaskNode`'s `llm_triage` model branch also consumes the
    /// resolved policy's `triage` tier.
    #[tokio::test]
    async fn triage_node_llm_branch_consumes_resolved_model_tier() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                triage: ModelTier::Opus,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });
        let ctx = ctx_with_policy(ctx, &policy);

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome =
                canned_outcome(json!({ "verdict": "RETRYABLE", "reason": "r" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    /// `ConsolidatedReviewNode` consumes the resolved policy's `review`
    /// tier.
    #[tokio::test]
    async fn review_node_consumes_resolved_model_tier() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                review: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let seen_config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let seen_config_clone = seen_config.clone();
        let transport: ModelTransport = Arc::new(move |config, _prompt| {
            *seen_config_clone.lock().unwrap() = Some(config);
            let outcome = canned_outcome(
                json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = ConsolidatedReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        node.process(ctx).await.expect("process should succeed");

        let config = seen_config
            .lock()
            .unwrap()
            .clone()
            .expect("transport should have been called");
        assert_eq!(config.model.as_deref(), Some("claude-haiku-4-5"));
    }

    // --- Review-gate policy consumption (EN.3.C task 4) ---------------------

    /// A small `git diff --numstat` stub: one changed file, one added +
    /// one deleted line — well under the default
    /// `review_skip_max_files`/`review_skip_max_diff_lines` thresholds.
    fn trivial_diff_runner() -> CommandRunner {
        Arc::new(|_program, args, _cwd| {
            assert_eq!(args, ["diff", "--numstat", "main..HEAD"]);
            Ok(CommandOutput {
                status: 0,
                stdout: "1\t1\tsrc/lib.rs\n".to_string(),
                stderr: String::new(),
            })
        })
    }

    /// A large `git diff --numstat` stub: two files with a combined diff
    /// line count well past the default thresholds.
    fn non_trivial_diff_runner() -> CommandRunner {
        Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: "50\t50\tsrc/a.rs\n50\t50\tsrc/b.rs\n".to_string(),
                stderr: String::new(),
            })
        })
    }

    fn ctx_with_worktree(mut ctx: TaskContext) -> TaskContext {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );
        ctx
    }

    /// A trivial green task (small diff, under the default thresholds)
    /// classifies `trivial: true` and, under `TrivialSkip`, the router
    /// skips `ConsolidatedReviewNode` and goes straight to
    /// `UpdateTaskStatusNode`.
    #[tokio::test]
    async fn trivial_green_task_skips_review_in_trivial_skip_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("UpdateTaskStatusNode".to_string()));
    }

    /// A non-trivial green task (diff over the thresholds) classifies
    /// `trivial: false` and, even under `TrivialSkip`, still routes to
    /// `ConsolidatedReviewNode`.
    #[tokio::test]
    async fn non_trivial_task_still_routes_to_review_in_trivial_skip_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(non_trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);

        let router = TriageRouterNode;
        assert_eq!(
            router.route(&out),
            Some("ConsolidatedReviewNode".to_string())
        );
    }

    /// A failing task's `RETRYABLE` verdict is unaffected by `review_mode`:
    /// it always routes through `IncrementAttemptNode`, never straight to
    /// review or `UpdateTaskStatusNode`.
    #[tokio::test]
    async fn failing_task_still_routes_through_retry_regardless_of_review_mode() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(false, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("IncrementAttemptNode".to_string()));
    }

    /// `per_task` (the built-in default `review_mode`) is unchanged: even a
    /// trivial green task still routes to `ConsolidatedReviewNode` — no
    /// policy stamped at all reproduces today's behavior byte-for-byte.
    #[tokio::test]
    async fn per_task_default_routes_trivial_task_to_review() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // No RESOLVED_POLICY_IDENTITY stamped -> falls back to the built-in
        // default, which is `per_task`.
        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);

        let router = TriageRouterNode;
        assert_eq!(
            router.route(&out),
            Some("ConsolidatedReviewNode".to_string())
        );
    }

    /// `end_only` collapses per-task review away entirely: a `PASS` verdict
    /// routes straight to `UpdateTaskStatusNode` regardless of triviality.
    #[tokio::test]
    async fn end_only_mode_skips_per_task_review_regardless_of_triviality() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::EndOnly,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree(ctx_with_test_result(true, &task));
        let ctx = ctx_with_policy(ctx, &policy);

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(non_trivial_diff_runner());
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);

        let router = TriageRouterNode;
        assert_eq!(router.route(&out), Some("UpdateTaskStatusNode".to_string()));
    }

    /// `classify_trivial` falls back to non-trivial (`false`) when the
    /// worktree/`git diff` invocation is unavailable, rather than erroring
    /// `TriageTaskNode::process`.
    #[tokio::test]
    async fn trivial_classification_defaults_false_without_worktree() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        // No `SetupWorktreeNode` output stamped -> `worktree_path` fails ->
        // `classify_trivial` defensively returns `false`.
        let ctx = ctx_with_test_result(true, &task);

        let node = TriageTaskNode::new().with_transport(panicking_transport());
        let out = node
            .process(ctx)
            .await
            .expect("process should succeed even without a worktree");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "PASS");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], false);
    }

    // --- SaveStateNode -------------------------------------------------------

    #[tokio::test]
    async fn save_state_writes_file_and_commits() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            calls_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| (*s).to_string()).collect());
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();
        assert!(Path::new(saved_to).exists());
        assert!(saved_to.ends_with("sdlc-flow-state.json"));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0][0], "add");
        assert_eq!(recorded[1][0], "commit");
    }
}
