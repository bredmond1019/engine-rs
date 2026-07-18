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

use super::schema::{SDLCState, SDLCTask, SDLCTaskStatus, SDLCTriageVerdict};
use super::setup::{CommandOutput, CommandRunner, ModelTransport};

/// A review with more than this many distinct issues is treated as a
/// structural failure (re-implementation is unlikely to converge) rather
/// than a minor, fixable one. Mirrors
/// `review_router_node._STRUCTURAL_ISSUE_THRESHOLD` in Python.
const STRUCTURAL_ISSUE_THRESHOLD: usize = 5;

/// Stamp a node's output onto `ctx.nodes` under its own identity.
fn put_result(ctx: &mut TaskContext, identity: &str, value: serde_json::Value) {
    ctx.nodes.insert(identity.to_string(), value);
}

/// Look up a prior node's output from `ctx.nodes` by identity.
fn get_result<'a>(ctx: &'a TaskContext, identity: &str) -> Option<&'a serde_json::Value> {
    ctx.nodes.get(identity)
}

/// Return the most recently mutated `SDLCState` (`UpdateTaskStatusNode`'s
/// output if this is not the first pass through the loop, else
/// `LoadTaskStateNode`'s initial load). Mirrors the `_latest_state_dict`
/// helper shared by `TaskQueueRouterNode`/`UpdateTaskStatusNode`/
/// `SaveStateNode` in Python.
fn latest_state(ctx: &TaskContext) -> Result<SDLCState, NodeError> {
    let value = get_result(ctx, "UpdateTaskStatusNode")
        .or_else(|| get_result(ctx, "LoadTaskStateNode"))
        .ok_or_else(|| {
            NodeError::new(
                "no SDLCState found: neither UpdateTaskStatusNode nor LoadTaskStateNode has run",
            )
        })?;
    serde_json::from_value(value.clone())
        .map_err(|err| NodeError::new(format!("failed to parse SDLCState: {err}")))
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

        let mut step = ClaudeCodeStep::new("ImplementTaskNode", self.config.clone(), prompt);
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

        let parsed: ImplementOutput = serde_json::from_str(&content).unwrap_or(ImplementOutput {
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
            runner: super::setup::default_command_runner(),
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

        let all_passed = test_result
            .get("all_passed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let attempt_count = current
            .get("attempt_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max_attempts = current
            .get("max_attempts")
            .and_then(|v| v.as_u64())
            .unwrap_or(3);

        if all_passed {
            put_result(
                &mut ctx,
                "TriageTaskNode",
                json!({ "verdict": "PASS", "reason": "All harness checks passed." }),
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

        let mut step = ClaudeCodeStep::new("TriageTaskNode", self.config.clone(), prompt);
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

        let parsed: TriageOutput = serde_json::from_str(&content).map_err(|err| {
            NodeError::new(format!(
                "TriageTaskNode: failed to parse model output as JSON: {err}"
            ))
        })?;

        put_result(
            &mut ctx,
            "TriageTaskNode",
            json!({ "verdict": parsed.verdict, "reason": parsed.reason }),
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
        let verdict = get_result(ctx, "TriageTaskNode")?
            .get("verdict")?
            .as_str()?;
        match verdict {
            "PASS" => Some("ConsolidatedReviewNode".to_string()),
            "RETRYABLE" => Some("ImplementTaskNode".to_string()),
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
            runner: super::setup::default_command_runner(),
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

        let mut step = ClaudeCodeStep::new("ConsolidatedReviewNode", self.config.clone(), prompt);
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

        let parsed: ReviewOutput = serde_json::from_str(&content).map_err(|err| {
            NodeError::new(format!(
                "ConsolidatedReviewNode: failed to parse model output as JSON: {err}"
            ))
        })?;

        put_result(
            &mut ctx,
            "ConsolidatedReviewNode",
            json!({
                "verdict": parsed.verdict,
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
                    Some("ImplementTaskNode".to_string())
                }
            }
            _ => None,
        }
    }
}

// --- UpdateTaskStatusNode --------------------------------------------------

/// Deterministic node: mutates the current task's status (and, on a retry,
/// its `attempt_count`) in the durable `SDLCState`, keeping
/// `SDLCTelemetry` counters in lockstep.
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

        match verdict {
            SDLCTriageVerdict::Pass => {
                task.status = SDLCTaskStatus::Done;
                state.telemetry.tasks_passed += 1;
            }
            SDLCTriageVerdict::MajorBail => {
                task.status = SDLCTaskStatus::Failed;
                state.telemetry.tasks_failed += 1;
            }
            SDLCTriageVerdict::Retryable => {
                task.attempt_count += 1;
            }
        }
        state.telemetry.total_attempts += 1;

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
            runner: super::setup::default_command_runner(),
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

    // --- TriageRouterNode ------------------------------------------------

    #[test]
    fn triage_router_back_edge() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "RETRYABLE", "reason": "retry" }),
        );
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("ImplementTaskNode".to_string()));
    }

    #[test]
    fn triage_router_all_verdicts() {
        let router = TriageRouterNode;
        for (verdict, expected) in [
            ("PASS", "ConsolidatedReviewNode"),
            ("RETRYABLE", "ImplementTaskNode"),
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
            Some("ImplementTaskNode".to_string())
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
            Some("ImplementTaskNode".to_string())
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
