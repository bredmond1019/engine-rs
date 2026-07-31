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

#[cfg(test)]
use super::policy::{ModelTier, OutputVerbosity};
use super::policy::{ReviewMode, SdlcPolicy, TestDepth};
use super::schema::{RunMeta, SDLCState, SDLCTask, SDLCTaskStatus, SDLCTriageVerdict};
use super::{
    get_result, parse_structured_or_fenced, put_result, CommandOutput, CommandRunner,
    ModelTransport,
};
#[cfg(test)]
use crate::policy::RESOLVED_POLICY_IDENTITY;

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

/// Read the resolved [`SdlcPolicy`] stamped by dispatch (`engine-serve`'s
/// `seed_resolved_policy`) or by `SetupWorktreeNode`
/// (`setup::RESOLVED_POLICY_IDENTITY`). Fails loudly — `Err` — when the
/// stamp is absent or unparsable, rather than silently falling back to a
/// built-in default (task 8): a ctx driven directly in a unit test must now
/// seed a policy explicitly (`ctx_with_policy`/`ctx_with_current_task`).
/// Delegates to the generic `crate::policy::resolved_policy_strict::<SdlcPolicy>`
/// (EN.4.0/EN.5.D).
fn resolved_policy(ctx: &TaskContext) -> Result<SdlcPolicy, NodeError> {
    crate::policy::resolved_policy_strict::<SdlcPolicy>(ctx)
}

/// Apply the resolved policy's model-tier + prompt-cache knobs to a stage's
/// `Config`, then append the `output_verbosity` directive to `prompt`.
/// Returns `(config, prompt)`. Delegates to the generic
/// `crate::policy::shaping::{apply_model_tier, apply_prompt_cache,
/// apply_verbosity_directive}` (EN.4.0).
fn apply_policy(
    config: Config,
    prompt: String,
    policy: &SdlcPolicy,
    stage: Stage,
) -> (Config, String) {
    let tier = match stage {
        Stage::Implement => policy.model_tiers.implement,
        Stage::Triage => policy.model_tiers.triage,
        Stage::Review => policy.model_tiers.review,
    };
    let config = crate::policy::apply_model_tier(config, tier, &policy.local.model);
    let config =
        crate::policy::apply_prompt_cache(config, policy.prompt_cache, STABLE_SYSTEM_PROMPT);
    let prompt = crate::policy::apply_verbosity_directive(prompt, policy.output_verbosity);

    (config, prompt)
}

/// Deterministically classify the current task's diff as "trivial" against
/// the resolved policy's `review_skip_max_files`/`review_skip_max_diff_lines`
/// thresholds (lever #3a, `trivial_skip` mode) — zero model tokens spent.
/// Reads `git diff --numstat <base_sha>..HEAD` via the injectable
/// [`CommandRunner`] seam: one line per changed file,
/// `<added>\t<deleted>\t<path>`. The diff base is the SHA `SetupWorktreeNode`
/// captured at worktree-setup time (see [`base_sha`]), falling back to
/// `main..HEAD` when absent (e.g. unit tests with no `SetupWorktreeNode`
/// output). Any unparsable line (e.g. a binary file's `-\t-\tpath`) is
/// treated conservatively as non-trivial. Falls back to non-trivial
/// (`false`) when the worktree path or the `git diff` invocation is
/// unavailable, so this never turns a `process` failure into an error path
/// — trivial-skip is an optimization, not a correctness requirement.
fn classify_trivial(ctx: &TaskContext, runner: &CommandRunner, policy: &SdlcPolicy) -> bool {
    let Ok(worktree) = worktree_path(ctx) else {
        return false;
    };
    let range = diff_range(ctx);
    let Ok(output) = runner("git", &["diff", "--numstat", &range], Path::new(&worktree)) else {
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
///
/// `pub(crate)` (not private) because `wrap_up::derive_terminal_signal` also
/// needs this exact threshold to reconstruct, post hoc, whether a
/// `ConsolidatedReviewNode` verdict that reached `WrapUpNode` did so via the
/// structural branch (this same gate `ReviewRouterNode::route` uses) rather
/// than some other path — the two must never independently drift.
pub(crate) const STRUCTURAL_ISSUE_THRESHOLD: usize = 5;

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
///
/// `pub(crate)` (not private): this is the SINGLE `latest_state`
/// implementation for the whole `sdlc_flow` module (EN.3.G task 2).
/// `wrap_up.rs` (`WrapUpNode::process` and `write_terminal_blocked_state`)
/// calls this one directly rather than keeping a local copy that omits
/// `IncrementAttemptNode` — that omission under-reported `attempt_count` /
/// `telemetry.total_attempts` on a `MAJOR_BAIL` reached after retries,
/// because the retry-incremented state was never considered as a candidate.
pub(crate) fn latest_state(ctx: &TaskContext) -> Result<SDLCState, NodeError> {
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

pub(crate) fn worktree_path(ctx: &TaskContext) -> Result<String, NodeError> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| NodeError::new("SetupWorktreeNode output missing worktree_path"))
}

/// Read the base commit SHA captured by `SetupWorktreeNode` at worktree-setup
/// time, if present. Mirrors [`worktree_path`] but returns `None` rather than
/// an error when absent (best-effort: `SetupWorktreeNode` omits `base_sha`
/// when `git rev-parse HEAD` failed or the runner is a no-op stub, and
/// callers here fall back to `main..HEAD` in that case).
fn base_sha(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("base_sha"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Build the `git diff` range argument (`<base_sha>..HEAD` when a base SHA
/// was captured at worktree-setup time, `main..HEAD` otherwise) shared by
/// [`classify_trivial`] and `ConsolidatedReviewNode`.
fn diff_range(ctx: &TaskContext) -> String {
    match base_sha(ctx) {
        Some(sha) => format!("{sha}..HEAD"),
        None => "main..HEAD".to_string(),
    }
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
            // Drain branch: route through the run-level `FinalValidationNode`
            // gate (EN.3.E) before `PatchDocsNode`, not directly to it.
            Some("FinalValidationNode".to_string())
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

/// JSON schema matching [`ImplementOutput`], passed as `Config.json_schema`
/// so `claude-code-rs` requests (and pre-parses) a schema-constrained reply
/// via `Outcome.structured_output` instead of relying solely on prompt text.
fn implement_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "modified_files": { "type": "array", "items": { "type": "string" } },
            "tests_added": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["summary"],
    })
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

        let policy = resolved_policy(&ctx)?;
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

        config.json_schema = Some(implement_output_schema());

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
            parse_structured_or_fenced(&ctx, "ImplementTaskNode", &content).unwrap_or(
                ImplementOutput {
                    summary: content.clone(),
                    modified_files: Vec::new(),
                    tests_added: Vec::new(),
                },
            );

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
///
/// `pub` (not `pub(crate)`) so [`super::final_validation::FinalValidationNode`]
/// — which shares [`TestTaskNode::run_checks`] rather than forking a second
/// check-kind dispatch — can name the type its stamped result carries, and
/// so [`super::schema::CommittedFinalValidation`] (EN.3.E task 3), itself a
/// `pub` type, can reuse this exact shape for the committed-state
/// `final_validation.check_results` array rather than inventing a parallel
/// one — a `pub(crate)` field type inside a `pub` struct is a
/// `private_interfaces` warning (denied under `clippy -- -D warnings`).
/// `Deserialize`/`PartialEq`/`Eq` are needed for that reuse: the
/// committed-state round trip deserializes `check_results` back out of the
/// on-disk JSON. Fields stay module-private — nothing outside this crate
/// constructs one directly; only the *type name* needs to be nameable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
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
/// Every check `kind` from the Python port
/// (`orchestrator/app/workflows/sdlc_flow_workflow_nodes/test_task_node.py`)
/// is supported: `command` (the default), `forbidden-pattern-scan`,
/// `baseline-diff`, `count-delta`, `warning-scan`. Any *other* kind still
/// fails closed via [`Self::run_unsupported_kind`], so a harness.json typo
/// or a genuinely new kind never silently passes.
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

    /// Runs `command` (`sh -c <command>`) via the injectable runner,
    /// returning stdout+stderr — the shared shell-out primitive every check
    /// kind below builds on.
    fn shell_out(&self, command: &str, worktree: &Path) -> CommandOutput {
        (self.runner)("sh", &["-c", command], worktree).unwrap_or(CommandOutput {
            status: -1,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// `forbidden-pattern-scan`: grep for each rule's pattern under its
    /// paths, drop matches covered by that rule's `allowlistPattern`, fail
    /// on any match left.
    ///
    /// The pattern is passed as its own argv entry to a directly-invoked
    /// `grep`, never interpolated into an `sh -c` string (EN.3.G task 5) —
    /// a pattern containing `'`, `"`, `$(...)`, or `;` used to terminate the
    /// shell quoting or inject a second command. **Glob carve-out:** the
    /// shell used to expand glob metacharacters (`*`, `?`, `[`) in `paths`;
    /// a direct `grep` invocation does not, so a rule whose `paths` contains
    /// one stays on the `sh -c` route for that rule only, with the pattern
    /// escaped as `'\''` so it still cannot break out of quoting.
    fn run_forbidden_pattern_scan(
        &self,
        check: &serde_json::Value,
        worktree: &Path,
    ) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();

        let mut violations: Vec<String> = Vec::new();
        let mut output_parts: Vec<String> = Vec::new();
        for rule in check
            .get("rules")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let pattern = rule.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let paths = rule.get("paths").and_then(|v| v.as_str()).unwrap_or("");
            let path_entries: Vec<&str> = paths.split_whitespace().collect();
            if path_entries.is_empty() {
                // No path operand would make `grep` read stdin and hang;
                // skip the rule and record nothing.
                continue;
            }

            let stdout = if path_entries.iter().any(|p| p.contains(['*', '?', '['])) {
                // Glob carve-out: keep the `sh -c` route so the shell still
                // expands the glob, but escape every `'` in the pattern as
                // `'\''` so it remains inert as shell syntax.
                let escaped_pattern = pattern.replace('\'', r"'\''");
                let grep_command = format!("grep -rnE '{escaped_pattern}' {paths}");
                self.shell_out(&grep_command, worktree).stdout
            } else {
                let mut args: Vec<&str> = vec!["-rnE", pattern];
                args.extend(path_entries.iter().copied());
                (self.runner)("grep", &args, worktree)
                    .map(|out| out.stdout)
                    .unwrap_or_default()
            };

            let mut matches: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
            if let Some(allowlist) = rule.get("allowlistPattern").and_then(|v| v.as_str()) {
                if let Ok(re) = regex::Regex::new(allowlist) {
                    matches.retain(|line| !re.is_match(line));
                }
            }
            violations.extend(matches.into_iter().map(str::to_string));
            output_parts.push(stdout);
        }

        let passed = violations.is_empty();
        let message = if passed {
            String::new()
        } else {
            format!("{} forbidden-pattern match(es)", violations.len())
        };
        CheckResult {
            name,
            kind: "forbidden-pattern-scan".to_string(),
            passed,
            output: output_parts.join("\n"),
            message,
        }
    }

    /// `baseline-diff`: run `baselineCommand` and `command`, both expected
    /// to emit a JSON array; fail on any `command` entry whose `compareKeys`
    /// projection isn't present in the baseline's.
    fn run_baseline_diff(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let compare_keys: Vec<String> = check
            .get("compareKeys")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        let baseline_command = check
            .get("baselineCommand")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let baseline_stdout = self.shell_out(baseline_command, worktree).stdout;
        let current_stdout = self.shell_out(command, worktree).stdout;

        let baseline_entries: Vec<serde_json::Value> =
            serde_json::from_str(&baseline_stdout).unwrap_or_default();
        let current_entries: Vec<serde_json::Value> =
            serde_json::from_str(&current_stdout).unwrap_or_default();

        let key = |entry: &serde_json::Value| -> Vec<Option<String>> {
            compare_keys
                .iter()
                .map(|k| entry.get(k).map(|v| v.to_string()))
                .collect()
        };
        let baseline_keys: std::collections::HashSet<Vec<Option<String>>> =
            baseline_entries.iter().map(key).collect();
        let new_entries: usize = current_entries
            .iter()
            .filter(|entry| !baseline_keys.contains(&key(entry)))
            .count();

        let passed = new_entries == 0;
        let message = if passed {
            String::new()
        } else {
            format!("{new_entries} net-new violation(s)")
        };
        CheckResult {
            name,
            kind: "baseline-diff".to_string(),
            passed,
            output: current_stdout,
            message,
        }
    }

    /// `count-delta`: extract a count from `command`'s stdout via
    /// `countPattern`, comparing it to `baseline` in the `failOn` direction
    /// (`"decrease"` fails when the count dropped; anything else fails when
    /// it rose).
    fn run_count_delta(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let baseline_count = check.get("baseline").and_then(|v| v.as_i64()).unwrap_or(0);
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let count_pattern = check
            .get("countPattern")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let stdout = self.shell_out(command, worktree).stdout;
        let current_count = regex::Regex::new(count_pattern)
            .ok()
            .and_then(|re| re.find(&stdout))
            .and_then(|m| m.as_str().split_whitespace().next())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);

        let fail_on = check
            .get("failOn")
            .and_then(|v| v.as_str())
            .unwrap_or("decrease");
        let passed = if fail_on == "decrease" {
            current_count >= baseline_count
        } else {
            current_count <= baseline_count
        };
        let message = if passed {
            String::new()
        } else {
            format!("count {current_count} vs baseline {baseline_count} ({fail_on})")
        };
        CheckResult {
            name,
            kind: "count-delta".to_string(),
            passed,
            output: stdout,
            message,
        }
    }

    /// `warning-scan`: run `command`, scan combined stdout+stderr for every
    /// `warningPatterns` entry. Only fails the check itself when `gates` is
    /// true (default `false` for this kind — matches the Python port).
    fn run_warning_scan(&self, check: &serde_json::Value, worktree: &Path) -> CheckResult {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = self.shell_out(command, worktree);
        let combined = format!("{}{}", outcome.stdout, outcome.stderr);

        let found: Vec<String> = check
            .get("warningPatterns")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .filter(|pattern| {
                regex::Regex::new(pattern)
                    .map(|re| re.is_match(&combined))
                    .unwrap_or(false)
            })
            .map(str::to_string)
            .collect();

        let gates = check
            .get("gates")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let passed = !gates || found.is_empty();
        let message = if found.is_empty() {
            String::new()
        } else {
            format!("warning pattern(s) matched: {found:?}")
        };
        CheckResult {
            name,
            kind: "warning-scan".to_string(),
            passed,
            output: combined,
            message,
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

    /// List every path `git status --porcelain` reports as changed
    /// (modified, added, deleted, renamed, or untracked) in `worktree`, via
    /// the injectable [`CommandRunner`] seam. Each porcelain line is either
    /// `XY path` or, for a rename, `XY orig -> new` — this returns the
    /// right-hand path in the rename case (the file's current location) and
    /// the single path otherwise. Returns an empty list (never errors) when
    /// the runner invocation itself fails, so a spawn failure here degrades
    /// to "nothing looks changed" rather than aborting the node.
    fn changed_files(&self, worktree: &Path) -> Vec<String> {
        let output =
            (self.runner)("git", &["status", "--porcelain"], worktree).unwrap_or(CommandOutput {
                status: -1,
                stdout: String::new(),
                stderr: String::new(),
            });

        output
            .stdout
            .lines()
            .filter_map(|line| {
                if line.len() <= 3 {
                    return None;
                }
                let rest = line[3..].trim();
                if rest.is_empty() {
                    return None;
                }
                // Rename/copy lines look like `orig -> new`; keep the
                // destination path.
                let path = rest.rsplit(" -> ").next().unwrap_or(rest).trim();
                if path.is_empty() {
                    None
                } else {
                    Some(path.trim_matches('"').to_string())
                }
            })
            .collect()
    }

    /// Write-verification guard: checks [`Self::changed_files`]'s report of
    /// what actually changed in the worktree against `ImplementTaskNode`'s
    /// claimed `modified_files` from ctx.
    ///
    /// The pass condition is deliberately "the worktree shows ANY change at
    /// all", not "the specific claimed paths changed": a real (non-stubbed)
    /// `claude` call was observed live to leave `modified_files` empty even
    /// on a genuinely successful write (the model's own JSON self-report is
    /// not a reliable enumeration of what it touched — see
    /// `sdlc_flow_live.rs`'s `live_full_workflow_real_implement_and_review`).
    /// Gating on exact claim-vs-disk matching would false-fail a real,
    /// useful write whose self-reported paths are incomplete or off by a
    /// prefix/suffix quirk; gating on "did anything change" is robust to
    /// that unreliability while still catching the guard's actual target —
    /// the original bug this exists for (`planning/decisions/
    /// D8-autonomous-node-write-permission.md`): a claimed, narrated write
    /// that never touched disk at all.
    ///
    /// Only trips (a failed [`CheckResult`], routed through the normal
    /// triage/retry machinery exactly like a harness-check failure) when
    /// the worktree shows ZERO changes AND the claim was non-empty — the
    /// model asserted specific writes but nothing happened anywhere. A
    /// claim-empty-and-nothing-changed pair passes: a genuinely no-op task
    /// (e.g. investigation-only) is legitimate and indistinguishable from
    /// this state without a task-level "expected to write" signal this
    /// node doesn't have.
    fn verify_claimed_writes(&self, ctx: &TaskContext, worktree: &Path) -> Option<CheckResult> {
        let modified_files: Vec<String> = get_result(ctx, "ImplementTaskNode")
            .and_then(|value| value.get("modified_files"))
            .and_then(|value| value.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        // Short-circuit BEFORE invoking `changed_files` (which calls the
        // injected runner): an empty claim never trips the guard, so there
        // is no need to spend a `git status` call on it. This also keeps
        // this guard's runner-call count at zero for empty claims, matching
        // callers (tests and otherwise) that assume `TestTaskNode`'s runner
        // is only invoked for actual harness-check commands when
        // `ImplementTaskNode` made no write claim.
        if modified_files.is_empty() {
            return None;
        }

        let changed = self.changed_files(worktree);
        if !changed.is_empty() {
            return None;
        }

        Some(CheckResult {
            name: "write-verification".to_string(),
            kind: "write-verification".to_string(),
            passed: false,
            output: changed.join("\n"),
            message: format!(
                "ImplementTaskNode claimed modified_files {modified_files:?} but the worktree \
                 shows no changes at all (git status --porcelain: {changed:?})"
            ),
        })
    }

    /// `pub(crate)` so [`super::final_validation::FinalValidationNode`] can
    /// share this exact executor (check-kind dispatch, the `enabled: false`
    /// skip, `gates` semantics) instead of forking a second copy — it
    /// constructs a throwaway `TestTaskNode` carrying its own runner purely
    /// as a handle onto this method.
    pub(crate) fn run_checks(
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
            let result = match kind.as_str() {
                "command" => self.run_command_check(check, worktree),
                "forbidden-pattern-scan" => self.run_forbidden_pattern_scan(check, worktree),
                "baseline-diff" => self.run_baseline_diff(check, worktree),
                "count-delta" => self.run_count_delta(check, worktree),
                "warning-scan" => self.run_warning_scan(check, worktree),
                _ => self.run_unsupported_kind(check, &kind),
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

/// Telemetry record for [`select_task_checks`]: which source produced the
/// checks that ran and what got dropped. Exists PURELY for standing rule 6's
/// "stamp the resolved value" requirement — `RunTelemetry`/`PolicyAggregate`
/// can attribute an observed cost/latency delta to the setting that caused
/// it. Nothing downstream branches on this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckSelection {
    /// Exactly one of `"task_validation_commands"` or `"harness"`.
    source: &'static str,
    /// The depth this selection was resolved at (recorded even when the
    /// `task_validation_commands` branch ignored it, so the telemetry
    /// record always reflects the run's resolved policy).
    depth: TestDepth,
    /// Names of harness checks dropped from the run (`enabled: false`, or
    /// `perTask: false` when `apply_per_task_filter` is true). Always empty
    /// on the `task_validation_commands` branch, since nothing is dropped
    /// there.
    excluded: Vec<String>,
}

/// Pure selection of which checks `run_checks` should execute this attempt.
/// Mirrors `.claude/workflows/sdlc-flow.js` (commit `a21a95e`) exactly:
///
/// 1. If `task_validation_commands` is non-empty, it wins VERBATIM and
///    `depth` is ignored entirely — a self-validating task needs no harness
///    and no fast/full substitution. Each command is synthesized into a
///    `command`-kind check (`{"name": "task-validation-<i>", "kind":
///    "command", "command": "<cmd>", "gates": true}`, 1-indexed) so
///    `run_checks` needs no new executor branch.
/// 2. Otherwise, start from `harness_checks`, drop any check with
///    `enabled: false` (belt-and-braces — `run_checks` also drops these,
///    but excluding them here too keeps `excluded` a truthful, complete
///    record) and, when `apply_per_task_filter` is true, any check with
///    `perTask: false` (the JS filter at `sdlc-flow.js:548`). When
///    `depth == TestDepth::Fast` and a surviving check declares a non-empty
///    string `fastCommand`, return a clone with `command` replaced by that
///    `fastCommand` (the JS substitution at `sdlc-flow.js:624`) — falling
///    back to the check's own `command` when `fastCommand` is absent or not
///    a non-empty string. No other field (`gates`, `kind`, `purpose`,
///    `baselineCommand`, `compareKeys`, `_comment`, `fastCommand` itself,
///    ...) is touched, so `run_checks` and its kind-specific readers see the
///    check otherwise byte-identical.
///
/// `apply_per_task_filter` is the ONE extra parameter this function carries
/// for [`super::final_validation::FinalValidationNode`] (`EN.3.E`): rather
/// than duplicating this whole selection function for the run-level gate,
/// the run-level gate is expressed as one more boolean input. `TestTaskNode`
/// passes `true` (today's behavior, byte-identical); `FinalValidationNode`
/// passes `false` so a `"perTask": false` check — `planning/harness.json`'s
/// `build` check (`cargo build --release`) — IS included, because the
/// per-task tripwire's cost-saving exclusion has no bearing on a
/// once-per-run authoritative gate.
///
/// Pure by design: no runner, no filesystem, no policy stamp — the whole
/// precedence table is unit-testable by constructing `serde_json::Value`
/// arrays directly.
///
pub(crate) fn select_task_checks(
    harness_checks: &[serde_json::Value],
    task_validation_commands: &[String],
    depth: TestDepth,
    apply_per_task_filter: bool,
) -> (Vec<serde_json::Value>, CheckSelection) {
    if !task_validation_commands.is_empty() {
        let synthesized = task_validation_commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                json!({
                    "name": format!("task-validation-{}", i + 1),
                    "kind": "command",
                    "command": cmd,
                    "gates": true,
                })
            })
            .collect();
        return (
            synthesized,
            CheckSelection {
                source: "task_validation_commands",
                depth,
                excluded: Vec::new(),
            },
        );
    }

    let mut selected = Vec::new();
    let mut excluded = Vec::new();

    for check in harness_checks {
        let name = check
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        if check.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            excluded.push(name);
            continue;
        }
        if apply_per_task_filter && check.get("perTask").and_then(|v| v.as_bool()) == Some(false) {
            excluded.push(name);
            continue;
        }

        if depth == TestDepth::Fast {
            let fast_command = check
                .get("fastCommand")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if let Some(fast_command) = fast_command {
                let mut substituted = check.clone();
                if let Some(obj) = substituted.as_object_mut() {
                    obj.insert(
                        "command".to_string(),
                        serde_json::Value::String(fast_command.to_string()),
                    );
                }
                selected.push(substituted);
                continue;
            }
        }

        selected.push(check.clone());
    }

    (
        selected,
        CheckSelection {
            source: "harness",
            depth,
            excluded,
        },
    )
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

        // Depth comes from the resolved policy (task 4 makes `TestTaskNode`
        // policy-strict for the first time — see `resolved_policy`'s doc
        // comment for why this fails loudly rather than falling back).
        let policy = resolved_policy(&ctx)?;
        let depth = policy.test_depth;

        // The CURRENT task's own `validation_commands`, read from the live
        // durable state by `current_task_id` — copying `TriageTaskNode`'s
        // exact lookup pattern (and its reason: `TaskQueueRouterNode`'s
        // output is a snapshot frozen at dispatch time, not the live state).
        let current = current_task_fields(&ctx)?.clone();
        let current_task_id = current
            .get("current_task_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::new("TaskQueueRouterNode output missing current_task_id"))?
            as u32;
        let state = latest_state(&ctx)?;
        let task = state
            .tasks
            .iter()
            .find(|task| task.task_id == current_task_id)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "TestTaskNode: no task with task_id={current_task_id} found in state"
                ))
            })?;
        let task_validation_commands = task.validation_commands.clone();

        // Write-verification guard runs before the harness suite so a
        // claimed-but-empty implement never gets a free pass through checks
        // that happen to already be green (e.g. no `harness.json`).
        let write_verification = self.verify_claimed_writes(&ctx, worktree);

        let harness_path = worktree.join("planning").join("harness.json");
        let harness_exists = harness_path.exists();
        let harness_checks: Vec<serde_json::Value> = if harness_exists {
            let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
                NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
            })?;
            let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
                NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
            })?;
            harness
                .get("validation")
                .and_then(|v| v.get("checks"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // No harness AND no self-validating `validation_commands`: there is
        // nothing to check. Previously this silently produced
        // `all_passed: true` with zero checks; now it is a gating
        // `harness-missing` failure instead.
        let (mut check_results, mut failed_names, selection) =
            if !harness_exists && task_validation_commands.is_empty() {
                let result = CheckResult {
                    name: "harness-missing".to_string(),
                    kind: "harness-missing".to_string(),
                    passed: false,
                    output: String::new(),
                    message: format!(
                        "no planning/harness.json found at {} and task {current_task_id} \
                         declares no validation_commands: nothing to validate against, so this \
                         is a gating failure rather than a silent pass",
                        harness_path.display()
                    ),
                };
                (
                    vec![result.clone()],
                    vec![result.name.clone()],
                    CheckSelection {
                        source: "harness",
                        depth,
                        excluded: Vec::new(),
                    },
                )
            } else {
                let (selected_checks, selection) =
                    select_task_checks(&harness_checks, &task_validation_commands, depth, true);
                let (results, failed) = self.run_checks(&selected_checks, worktree);
                (results, failed, selection)
            };

        if let Some(guard_result) = write_verification {
            failed_names.insert(0, guard_result.name.clone());
            check_results.insert(0, guard_result);
        }

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
                "test_depth": serde_json::to_value(selection.depth)
                    .unwrap_or(serde_json::Value::Null),
                "check_source": selection.source,
                "excluded_checks": selection.excluded,
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

/// JSON schema matching [`TriageOutput`].
fn triage_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string" },
            "reason": { "type": "string" },
        },
        "required": ["verdict", "reason"],
    })
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
            let policy = resolved_policy(&ctx)?;
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

        let policy = resolved_policy(&ctx)?;
        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Triage);
        config.json_schema = Some(triage_output_schema());

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

        let parsed: TriageOutput = parse_structured_or_fenced(&ctx, "TriageTaskNode", &content)
            .map_err(|err| {
                NodeError::new(format!(
                    "TriageTaskNode: failed to parse model output as JSON: {err}"
                ))
            })?;

        let normalized_verdict = parsed.verdict.trim().to_uppercase();
        let mut result = json!({
            // Same normalization as `ConsolidatedReviewNode`'s, and for the
            // same reason — `TriageRouterNode` exact-matches this string.
            // Normalization narrows the hole (see the observed-live `"pass"`
            // reply that motivated it); it does not close it — an
            // unrecognized value still needs `TriageRouterNode`'s fallback
            // arm below to guarantee the walk reaches `WrapUpNode`.
            "verdict": normalized_verdict,
            "reason": parsed.reason,
        });
        if !matches!(
            normalized_verdict.as_str(),
            "PASS" | "RETRYABLE" | "MAJOR_BAIL"
        ) {
            result["unrecognized_verdict"] = json!(normalized_verdict);
        }
        put_result(&mut ctx, "TriageTaskNode", result);

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
                let policy = resolved_policy(ctx).ok()?;
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
            // An unrecognized verdict must never silently halt the walk
            // mid-graph — `WrapUpNode` is already a declared connection from
            // this router (see `graph.rs`), so routing here is a no-op on
            // the graph shape. `TriageTaskNode::process` stamps
            // `unrecognized_verdict` alongside the (unchanged) `verdict` key
            // so `derive_terminal_signal` can surface the offending string
            // in the run's `bail_reason`.
            _ => Some("WrapUpNode".to_string()),
        }
    }
}

// --- ConsolidatedReviewNode ----------------------------------------------------

/// Model node (Sonnet): reviews the task's `git diff <base_sha>..HEAD`
/// (falling back to `main..HEAD` when no `base_sha` was captured) against its
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

/// JSON schema matching [`ReviewOutput`].
fn review_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string" },
            "summary": { "type": "string" },
            "issues": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["verdict", "summary"],
    })
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

        let range = diff_range(&ctx);
        let diff = (self.runner)("git", &["diff", &range], Path::new(&worktree))
            .map(|output| output.stdout)
            .unwrap_or_default();

        let prompt = format!(
            "Review this task's diff against its acceptance criteria. \
             Respond with strict JSON of the shape {{\"verdict\": str, \
             \"summary\": str, \"issues\": [str]}}.\n\nAcceptance criteria: \
             {acceptance_criteria}\n\nDiff:\n{diff}"
        );

        let policy = resolved_policy(&ctx)?;
        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Review);
        // Scope the model's session to the actual worktree, matching
        // `ImplementTaskNode`'s fix — without this, a real call that reads
        // the filesystem checks the host process's ambient cwd instead of
        // the task's worktree (observed live: the model correctly reported
        // the file it was asked to review as "missing", because it was
        // looking in the wrong directory).
        config.cwd = Some(std::path::PathBuf::from(&worktree));
        config.json_schema = Some(review_output_schema());

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
            parse_structured_or_fenced(&ctx, "ConsolidatedReviewNode", &content).map_err(
                |err| {
                    NodeError::new(format!(
                        "ConsolidatedReviewNode: failed to parse model output as JSON: {err}"
                    ))
                },
            )?;

        let normalized_verdict = parsed.verdict.trim().to_uppercase();
        let mut result = json!({
            // Normalized to the canonical uppercase form `ReviewRouterNode`
            // matches on — a real model reply doesn't reliably preserve
            // the exact casing asked for (observed live: a real Sonnet
            // reply returned "pass"). Normalization narrows the hole; it
            // does not close it — an unrecognized value still needs
            // `ReviewRouterNode`'s fallback arm below to guarantee the walk
            // reaches `WrapUpNode` instead of silently halting here.
            "verdict": normalized_verdict,
            "summary": parsed.summary,
            "issues": parsed.issues,
        });
        if !matches!(normalized_verdict.as_str(), "PASS" | "FAIL" | "PARTIAL") {
            result["unrecognized_verdict"] = json!(normalized_verdict);
        }
        put_result(&mut ctx, "ConsolidatedReviewNode", result);

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
            // An unrecognized verdict must never silently halt the walk
            // mid-graph — `WrapUpNode` is already a declared connection
            // from this router (see `graph.rs`). `ConsolidatedReviewNode`
            // stamps `unrecognized_verdict` alongside the (unchanged)
            // `verdict` key so `derive_terminal_signal` can surface the
            // offending string in the run's `bail_reason`.
            _ => Some("WrapUpNode".to_string()),
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

/// Read the `branch_name` stamped by `SetupWorktreeNode`, if the run went
/// through it. Absent in unit tests that drive `SaveStateNode` directly.
fn branch_name(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("branch_name"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Read `started_at` back out of an already-committed D31 state file at
/// `state_path`, if one exists and parses. Used to preserve the run's
/// original start time across a resumed run's per-task saves — every write
/// after the first must NOT stamp a fresh `started_at`, or a resumed run
/// would appear to restart its wall-clock every time `SaveStateNode` fires.
fn existing_started_at(state_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(state_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    SDLCState::from_committed_state_json(&value)
        .ok()?
        .started_at
}

/// Build this write's [`RunMeta`]: `branch`/`worktree_path` come straight
/// from `SetupWorktreeNode`'s output; `started_at` is preserved from an
/// existing on-disk committed file if one is already there (a resume),
/// otherwise stamped fresh (this run's first save); `updated_at` is always
/// stamped fresh; `run_id` is read back out of `ctx.metadata` via
/// [`crate::read_run_id`] — the stamp `Workflow::run_with`/`run_from` write
/// before the walk starts (`None` when the run carried no `RunOptions::run_id`,
/// e.g. any run driven by base-template's JS `sdlc-flow.js` engine).
fn build_run_meta(ctx: &TaskContext, worktree: &str, state_path: &Path) -> RunMeta {
    let now = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .to_string();
    let started_at = existing_started_at(state_path).unwrap_or_else(|| now.clone());
    RunMeta {
        branch: branch_name(ctx).unwrap_or_default(),
        worktree_path: worktree.to_string(),
        started_at,
        updated_at: now,
        run_id: crate::read_run_id(&ctx.metadata),
    }
}

/// Deterministic node: serializes the latest `SDLCState` to
/// `planning/{spec_slug}/sdlc/sdlc-flow-state.json` inside the worktree
/// (the D31-committed path/schema shared with base-template's JS
/// `sdlc-flow.js` engine — see `D10-committed-state-path-schema-alignment.md`)
/// and commits it via the injectable [`CommandRunner`] seam, so state
/// survives across resumed runs. A non-zero `git commit` (e.g. "nothing to
/// commit") is logged, not treated as a failure — mirrors the Python
/// behavior.
///
/// This per-task save point never has `review`/`docs`/`pr` yet (those are
/// end-of-run outputs from `ConsolidatedReviewNode`/`PatchDocsNode`/
/// `PullRequestNode`, none of which have run at this point in the loop) and
/// is never itself a terminal write (`WrapUpNode` is the only node that
/// derives a [`super::schema::TerminalSignal`]) — so it always calls
/// `to_committed_state_json` with `None` for all four.
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

        let state_dir = Path::new(&worktree)
            .join("planning")
            .join(&state.spec_slug)
            .join("sdlc");
        std::fs::create_dir_all(&state_dir).map_err(|err| {
            NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
        })?;
        let state_path = state_dir.join("sdlc-flow-state.json");
        let run_meta = build_run_meta(&ctx, &worktree, &state_path);
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        let json = serde_json::to_string_pretty(&committed)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
        std::fs::write(&state_path, json).map_err(|err| {
            NodeError::new(format!("failed to write {}: {err}", state_path.display()))
        })?;

        let state_path_str = state_path.to_string_lossy().to_string();
        super::commit_state_file(&self.runner, Path::new(&worktree), &state_path);

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

    /// Builds a task-loop-ready `ctx` and stamps a default [`SdlcPolicy`]
    /// under [`RESOLVED_POLICY_IDENTITY`] — required since task 8's strict
    /// `resolved_policy_strict` read (no more silent `Default` fallback for
    /// an unstamped ctx). Tests wanting a non-default policy call
    /// `ctx_with_policy(ctx, &policy)` afterwards to overwrite this stamp.
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
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
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
        assert_eq!(node.route(&out), Some("FinalValidationNode".to_string()));
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
                structured_output: None,
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
        // A recognized verdict must NOT carry the `unrecognized_verdict` key.
        assert!(out.nodes["TriageTaskNode"]
            .get("unrecognized_verdict")
            .is_none());
    }

    /// EN.3.G task 1: a garbage model verdict is stamped as
    /// `unrecognized_verdict` (alongside the byte-identical, unchanged
    /// `verdict` key the router still matches on) so `derive_terminal_signal`
    /// can surface it in the run's `bail_reason`.
    #[tokio::test]
    async fn triage_llm_branch_stamps_unrecognized_verdict() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome =
                canned_outcome(json!({ "verdict": "WAT", "reason": "unclear" }).to_string());
            Box::pin(async move { Ok(outcome) })
        });

        let node = TriageTaskNode::new().with_transport(transport);
        let mut ctx = ctx_with_test_result(false, &task);
        ctx.event = json!({ "spec_slug": "my-spec", "llm_triage": true });

        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "WAT");
        assert_eq!(out.nodes["TriageTaskNode"]["unrecognized_verdict"], "WAT");
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
            // The `PASS` branch reads the resolved policy's `review_mode`
            // (task 8's `resolved_policy_strict` — no more silent `Default`
            // fallback), so this ctx must carry a stamp even though the
            // other two verdicts never touch it.
            ctx.nodes.insert(
                RESOLVED_POLICY_IDENTITY.to_string(),
                serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
            );
            assert_eq!(router.route(&ctx), Some(expected.to_string()));
        }
    }

    /// EN.3.G task 1: an unrecognized verdict string must never silently
    /// halt the walk mid-graph — it routes to `WrapUpNode`, which is already
    /// a declared connection from this router (see `graph.rs`).
    #[test]
    fn triage_router_unrecognized_verdict_routes_to_wrap_up() {
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "WAT", "reason": "r", "unrecognized_verdict": "WAT" }),
        );
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// A ctx with no upstream `TriageTaskNode` result at all is a different
    /// condition from an unparseable verdict — the router must still return
    /// `None` (the walk has literally not reached this router yet), not
    /// `Some("WrapUpNode")`.
    #[test]
    fn triage_router_no_upstream_result_returns_none() {
        let ctx = empty_context(json!({}));
        let router = TriageRouterNode;
        assert_eq!(router.route(&ctx), None);
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

    /// EN.3.G task 1: an unrecognized review verdict must never silently
    /// halt the walk mid-graph — it routes to `WrapUpNode`, which is already
    /// a declared connection from this router (see `graph.rs`).
    #[test]
    fn review_router_unrecognized_verdict_routes_to_wrap_up() {
        let router = ReviewRouterNode;
        let mut ctx = empty_context(json!({}));
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "WAT", "summary": "s", "issues": [], "unrecognized_verdict": "WAT" }),
        );
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    /// A ctx with no upstream `ConsolidatedReviewNode` result at all is a
    /// different condition from an unparseable verdict — the router must
    /// still return `None`.
    #[test]
    fn review_router_no_upstream_result_returns_none() {
        let ctx = empty_context(json!({}));
        let router = ReviewRouterNode;
        assert_eq!(router.route(&ctx), None);
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
        // Guarantee-empty: see `setup.rs`'s `temp_dir_named` doc comment for
        // why PID-recycling makes this removal necessary, not optional.
        // Remove the ROOT dir before recreating the `planning` subdir.
        std::fs::remove_dir_all(&dir).ok();
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
        // Renamed intent, same coverage: task 4's harness-missing fix means
        // a worktree with no `planning/harness.json` and a task with no
        // `validation_commands` is now a GATING failure, not a silent pass
        // (see the "harness-missing fix" note in tasks.md). This is the
        // exact behavior the spec's acceptance criteria require.
        let worktree = temp_worktree();
        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "harness-missing");
    }

    #[tokio::test]
    async fn test_task_reports_gating_failure() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "always_fail", "command": "exit 1", "gates": true }]),
        );

        let ctx = ctx_for_worktree(&worktree);

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

        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new().with_runner(runner.clone());
        let out = node.process(ctx.clone()).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);

        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.unwrap();
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// Builds a `ctx` with a `SetupWorktreeNode` output plus everything
    /// `TestTaskNode` now needs to reach it (task 4 makes `TestTaskNode`
    /// policy-strict and reads the CURRENT task's `validation_commands` out
    /// of the live durable state): a single default task (no
    /// `validation_commands`), a matching `TaskQueueRouterNode`/
    /// `LoadTaskStateNode` pair, and the built-in `SdlcPolicy` default
    /// (behavior-stable per rule 6, so stamping it changes nothing these
    /// tests assert).
    fn ctx_for_worktree(worktree: &Path) -> TaskContext {
        let task = SDLCTask::new(1, "t", "d");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    fn ctx_with_implement_claim(worktree: &Path, modified_files: &[&str]) -> TaskContext {
        let mut ctx = ctx_for_worktree(worktree);
        ctx.nodes.insert(
            "ImplementTaskNode".to_string(),
            json!({
                "summary": "did the thing",
                "modified_files": modified_files,
                "tests_added": [],
            }),
        );
        ctx
    }

    fn porcelain_runner(status_lines: &'static str) -> CommandRunner {
        Arc::new(move |program, args, _cwd| {
            if program == "git" && args.first() == Some(&"status") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: status_lines.to_string(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        })
    }

    /// A non-empty `modified_files` claim with none of the claimed paths
    /// showing up in `git status --porcelain` fails the write-verification
    /// guard, even when no `harness.json` exists, and the failure routes
    /// through the normal `all_passed`/`check_results` shape (i.e. through
    /// the same triage/retry path a harness failure would).
    #[tokio::test]
    async fn write_verification_fails_when_no_claimed_file_changed() {
        let worktree = temp_worktree();
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "write-verification");
        assert_eq!(results[0]["passed"], false);
        assert!(out.nodes["TestTaskNode"]["failure_summary"]
            .as_str()
            .unwrap()
            .contains("write-verification"));
    }

    /// A claimed file that DOES show up in `git status --porcelain` passes
    /// the guard, and (with no `harness.json`) the task overall passes.
    #[tokio::test]
    async fn write_verification_passes_when_claimed_file_changed() {
        let worktree = temp_worktree();
        // An empty (but present) harness keeps this test isolated to the
        // write-verification guard: task 4's harness-missing fix only gates
        // when `planning/harness.json` is absent entirely.
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(" M src/lib.rs\n"));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results.is_empty());
    }

    /// An empty `modified_files` claim (a genuinely no-op task) never trips
    /// the guard, even when the worktree is completely clean.
    #[tokio::test]
    async fn write_verification_does_not_trip_on_empty_claim() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_with_implement_claim(&worktree, &[]);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results.is_empty());
    }

    /// No `ImplementTaskNode` output at all (e.g. a ctx driven directly in a
    /// unit test) behaves like an empty claim — the guard never trips.
    #[tokio::test]
    async fn write_verification_does_not_trip_when_implement_never_ran() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([]));
        let ctx = ctx_for_worktree(&worktree);

        let node = TestTaskNode::new().with_runner(porcelain_runner(""));
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// A guard failure and a harness-check failure both surface in
    /// `check_results`/`failure_summary` together when both are present.
    #[tokio::test]
    async fn write_verification_and_harness_failures_both_reported() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{ "name": "always_fail", "command": "exit 1", "gates": true }]),
        );
        let ctx = ctx_with_implement_claim(&worktree, &["src/lib.rs"]);

        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            if program == "git" && args.first() == Some(&"status") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["kind"], "write-verification");
        assert_eq!(results[1]["name"], "always_fail");
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_fails_on_unallowlisted_match() {
        let worktree = temp_worktree();
        std::fs::create_dir_all(worktree.join("app")).unwrap();
        std::fs::write(worktree.join("app").join("bad.py"), "open(\"x\")\n").unwrap();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "open-without-encoding",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "--include='*.py' app/" }],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["kind"], "forbidden-pattern-scan");
        assert_eq!(results[0]["passed"], false);
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_passes_when_match_is_allowlisted() {
        let worktree = temp_worktree();
        std::fs::create_dir_all(worktree.join("app")).unwrap();
        std::fs::write(
            worktree.join("app").join("ok.py"),
            "open(\"x\", encoding=\"utf-8\")\n",
        )
        .unwrap();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "open-without-encoding",
                "gates": true,
                "rules": [{
                    "id": "r1",
                    "pattern": "open\\(",
                    "paths": "--include='*.py' app/",
                    "allowlistPattern": "encoding=",
                }],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    /// Records every invocation's `(program, args)` pair — unlike
    /// [`recording_command_runner`], which assumes the `sh -c <command>`
    /// shape and only ever records `args[1]`. EN.3.G task 5's direct `grep`
    /// invocation has a different shape (`program = "grep"`,
    /// `args = ["-rnE", pattern, ...paths]`), so the forbidden-pattern-scan
    /// tests below need the full argv to assert the pattern lands as its
    /// own unmodified entry rather than being interpolated into a string.
    fn recording_argv_runner() -> (CommandRunner, Arc<Mutex<Vec<(String, Vec<String>)>>>) {
        let recorded: Arc<Mutex<Vec<(String, Vec<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            recorded_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|a| (*a).to_string()).collect(),
            ));
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        (runner, recorded)
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_passes_pattern_as_its_own_argv_entry() {
        // Patterns that would break or inject through `sh -c 'grep ... '{pattern}'
        // ...'` string interpolation must land as a single, unmodified argv
        // entry to a directly-invoked `grep` — never inside an `sh -c` string.
        for pattern in ["it's", "say \"hi\"", "$(touch /tmp/pwned)", "foo; rm -rf /"] {
            let worktree = temp_worktree();
            write_harness(
                &worktree,
                json!([{
                    "kind": "forbidden-pattern-scan",
                    "name": "scan",
                    "gates": true,
                    "rules": [{ "id": "r1", "pattern": pattern, "paths": "app/" }],
                }]),
            );

            let (runner, recorded) = recording_argv_runner();
            let node = TestTaskNode::new().with_runner(runner);
            node.process(ctx_for_worktree(&worktree))
                .await
                .expect("process should succeed");

            let recorded = recorded.lock().unwrap();
            assert_eq!(
                recorded.len(),
                1,
                "expected exactly one invocation for pattern {pattern:?}"
            );
            let (program, args) = &recorded[0];
            assert_eq!(
                program, "grep",
                "pattern {pattern:?} did not use grep directly"
            );
            assert_ne!(
                program, "sh",
                "pattern {pattern:?} leaked into an sh -c string"
            );
            assert!(
                args.iter().any(|a| a == pattern),
                "pattern {pattern:?} was not passed as its own unmodified argv entry: {args:?}"
            );
        }
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_empty_paths_issues_no_invocation() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "scan",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "" }],
            }]),
        );

        let (runner, recorded) = recording_argv_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");

        assert!(recorded.lock().unwrap().is_empty());
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn forbidden_pattern_scan_multi_path_becomes_multiple_argv_entries() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "forbidden-pattern-scan",
                "name": "scan",
                "gates": true,
                "rules": [{ "id": "r1", "pattern": "open\\(", "paths": "app/ lib/" }],
            }]),
        );

        let (runner, recorded) = recording_argv_runner();
        let node = TestTaskNode::new().with_runner(runner);
        node.process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let (program, args) = &recorded[0];
        assert_eq!(program, "grep");
        assert_eq!(args, &vec!["-rnE", "open\\(", "app/", "lib/"]);
    }

    #[tokio::test]
    async fn baseline_diff_fails_on_net_new_entry() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "baseline-diff",
                "name": "net-new-lint",
                "gates": true,
                "compareKeys": ["file", "code"],
                "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
                "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["message"], "1 net-new violation(s)");
    }

    #[tokio::test]
    async fn baseline_diff_passes_when_no_net_new_entries() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "baseline-diff",
                "name": "net-new-lint",
                "gates": true,
                "compareKeys": ["file", "code"],
                "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
                "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn count_delta_fails_when_count_decreases() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "count-delta",
                "name": "pytest-count",
                "gates": true,
                "baseline": 100,
                "countPattern": "\\d+ passed",
                "failOn": "decrease",
                "command": "echo '90 passed'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["message"], "count 90 vs baseline 100 (decrease)");
    }

    #[tokio::test]
    async fn count_delta_passes_when_count_holds_or_grows() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "count-delta",
                "name": "pytest-count",
                "gates": true,
                "baseline": 100,
                "countPattern": "\\d+ passed",
                "failOn": "decrease",
                "command": "echo '101 passed'",
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    }

    #[tokio::test]
    async fn warning_scan_does_not_gate_by_default() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "warning-scan",
                "name": "app-import",
                "gates": false,
                "command": "echo 'UserWarning: field shadows an attribute'",
                "warningPatterns": ["UserWarning"],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert_eq!(results[0]["passed"], true);
        assert!(results[0]["message"]
            .as_str()
            .unwrap()
            .contains("warning pattern(s) matched"));
    }

    #[tokio::test]
    async fn warning_scan_gates_when_check_opts_in() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([{
                "kind": "warning-scan",
                "name": "app-import",
                "gates": true,
                "command": "echo 'UserWarning: field shadows an attribute'",
                "warningPatterns": ["UserWarning"],
            }]),
        );

        let node = TestTaskNode::new();
        let out = node
            .process(ctx_for_worktree(&worktree))
            .await
            .expect("process should succeed");
        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
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
                structured_output: None,
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

    /// When `SetupWorktreeNode` stamped a `base_sha` (the SHA captured at
    /// worktree-setup time), the review diff pins to `<base_sha>..HEAD`
    /// instead of `main..HEAD` — so a `main` that advances mid-run doesn't
    /// misreport unrelated commits as reversions.
    #[tokio::test]
    async fn review_uses_base_sha_diff_range_when_present() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "base_sha": "abc1234" }),
        );

        let diff_called = Arc::new(Mutex::new(false));
        let diff_called_clone = diff_called.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            *diff_called_clone.lock().unwrap() = true;
            assert_eq!(args, ["diff", "abc1234..HEAD"]);
            Ok(CommandOutput {
                status: 0,
                stdout: "diff --git a b".to_string(),
                stderr: String::new(),
            })
        });

        let canned =
            json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(canned.clone());
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
        // A recognized verdict must NOT carry the `unrecognized_verdict` key.
        assert!(out.nodes["ConsolidatedReviewNode"]
            .get("unrecognized_verdict")
            .is_none());
    }

    /// EN.3.G task 1: a garbage model verdict is stamped as
    /// `unrecognized_verdict` (alongside the byte-identical, unchanged
    /// `verdict` key `ReviewRouterNode` still matches on) so
    /// `derive_terminal_signal` can surface it in the run's `bail_reason`.
    #[tokio::test]
    async fn review_node_stamps_unrecognized_verdict() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(
                json!({ "verdict": "WAT", "summary": "unclear", "issues": [] }).to_string(),
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
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["verdict"], "WAT");
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["unrecognized_verdict"],
            "WAT"
        );
    }

    /// A schema-tagged reply (`structured_output: Some(..)`) is consumed via
    /// the `structured` field written by `ClaudeCodeStep`, not the
    /// fence-strip path — proven by making `text` a value that would fail a
    /// strict-JSON parse (an unfenced non-JSON string) while `structured`
    /// carries the real payload.
    #[tokio::test]
    async fn review_prefers_structured_output_over_fence_parse() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let mut outcome = canned_outcome("not valid json at all".to_string());
            outcome.structured_output =
                Some(json!({ "verdict": "PASS", "summary": "from structured", "issues": [] }));
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
        assert_eq!(
            out.nodes["ConsolidatedReviewNode"]["summary"],
            "from structured"
        );
    }

    /// A fence-only reply (`structured_output: None`) still parses via the
    /// `strip_json_fence` + `serde_json::from_str` fallback.
    #[tokio::test]
    async fn review_falls_back_to_fence_parse_when_structured_absent() {
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let mut ctx = ctx_with_current_task(&state, &task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": "." }),
        );

        let fenced = format!(
            "```json\n{}\n```",
            json!({ "verdict": "PASS", "summary": "from fence", "issues": [] })
        );
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(fenced.clone());
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
        assert_eq!(out.nodes["ConsolidatedReviewNode"]["summary"], "from fence");
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
            structured_output: None,
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

    fn ctx_with_worktree_and_base_sha(mut ctx: TaskContext, base_sha: &str) -> TaskContext {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "base_sha": base_sha }),
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

    /// When `SetupWorktreeNode` stamped a `base_sha`, `classify_trivial`
    /// diffs `<base_sha>..HEAD` instead of `main..HEAD`.
    #[tokio::test]
    async fn trivial_classification_uses_base_sha_diff_range_when_present() {
        let mut task = SDLCTask::new(1, "One", "d1");
        task.max_attempts = 3;
        task.attempt_count = 0;

        let policy = SdlcPolicy {
            review_mode: ReviewMode::TrivialSkip,
            ..SdlcPolicy::default()
        };

        let ctx = ctx_with_worktree_and_base_sha(ctx_with_test_result(true, &task), "deadbee");
        let ctx = ctx_with_policy(ctx, &policy);

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            assert_eq!(args, ["diff", "--numstat", "deadbee..HEAD"]);
            Ok(CommandOutput {
                status: 0,
                stdout: "1\t1\tsrc/lib.rs\n".to_string(),
                stderr: String::new(),
            })
        });

        let node = TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        assert_eq!(out.nodes["TriageTaskNode"]["trivial"], true);
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
        assert!(saved_to.ends_with("sdlc/sdlc-flow-state.json"));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0][0], "add");
        assert_eq!(recorded[1][0], "commit");

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(value["tasks"].is_object());
        assert_eq!(value["status"], json!("running"));
        assert!(value["review"].is_null());
        assert!(value["docs"].is_null());
        assert!(value["pr"].is_null());
        assert!(value["bail_reason"].is_null());
        assert!(value["started_at"].as_str().is_some());
        assert!(value["updated_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn save_state_preserves_started_at_across_resumed_saves() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy(), "branch_name": "sdlc/x" }),
        );

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner.clone());
        let out = node.process(ctx.clone()).await.expect("first save");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"]
            .as_str()
            .unwrap()
            .to_string();
        let first_started_at = {
            let content = std::fs::read_to_string(&saved_to).unwrap();
            let value: serde_json::Value = serde_json::from_str(&content).unwrap();
            value["started_at"].as_str().unwrap().to_string()
        };

        // Simulate a resumed second save (e.g. a subsequent task's
        // `SaveStateNode` run) — `started_at` must not change, since it is
        // read back from the file `SaveStateNode` just wrote above rather
        // than recomputed.
        let node2 = SaveStateNode::new().with_runner(runner);
        let out2 = node2.process(ctx).await.expect("second save");
        let saved_to2 = out2.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();
        let content = std::fs::read_to_string(saved_to2).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["started_at"].as_str().unwrap(), first_started_at);
    }

    #[tokio::test]
    async fn save_state_stamps_run_id_from_context_metadata() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        let run_id = uuid::Uuid::new_v4();
        ctx.metadata = json!({ crate::RUN_ID_METADATA_KEY: run_id.to_string() });

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["run_id"], json!(run_id.to_string()));
    }

    #[tokio::test]
    async fn save_state_writes_null_run_id_for_empty_metadata() {
        let worktree = temp_worktree();
        let state = state_with_tasks(vec![SDLCTask::new(1, "One", "d1")]);
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        // Today-path: no `RunOptions::run_id` was ever stamped, so
        // `ctx.metadata` is the empty object `Workflow::seed_context`
        // always builds.
        assert_eq!(ctx.metadata, json!({}));

        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SaveStateNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["SaveStateNode"]["saved_to"].as_str().unwrap();

        let content = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(value["run_id"].is_null());
        // Otherwise byte-compatible with the pre-task-3 output shape.
        assert!(value["tasks"].is_object());
        assert_eq!(value["status"], json!("running"));
        assert!(value["review"].is_null());
        assert!(value["docs"].is_null());
        assert!(value["pr"].is_null());
        assert!(value["bail_reason"].is_null());
    }

    // --- select_task_checks --------------------------------------------------

    fn cmd_check(name: &str, command: &str) -> serde_json::Value {
        json!({ "name": name, "kind": "command", "command": command, "gates": true })
    }

    fn cmd_check_with_fast(name: &str, command: &str, fast_command: &str) -> serde_json::Value {
        json!({
            "name": name,
            "kind": "command",
            "command": command,
            "fastCommand": fast_command,
            "gates": true,
        })
    }

    #[test]
    fn select_task_checks_full_depth_keeps_command_even_with_fast_command() {
        let checks = vec![cmd_check_with_fast(
            "test",
            "cargo test --workspace",
            "cargo test --lib",
        )];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Full, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo test --workspace"));
        assert_eq!(selection.source, "harness");
        assert_eq!(selection.depth, TestDepth::Full);
        assert!(selection.excluded.is_empty());
    }

    #[test]
    fn select_task_checks_fast_depth_substitutes_fast_command() {
        let checks = vec![cmd_check_with_fast(
            "test",
            "cargo test --workspace",
            "cargo test --lib",
        )];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Fast, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo test --lib"));
        // No other field is disturbed by the substitution.
        assert_eq!(selected[0]["fastCommand"], json!("cargo test --lib"));
        assert_eq!(selected[0]["gates"], json!(true));
        assert_eq!(selection.source, "harness");
        assert_eq!(selection.depth, TestDepth::Fast);
    }

    #[test]
    fn select_task_checks_fast_depth_falls_back_to_command_when_no_fast_command() {
        let checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Fast, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["command"], json!("cargo fmt --check"));
        assert_eq!(selection.source, "harness");
    }

    #[test]
    fn select_task_checks_excludes_per_task_false_at_both_depths() {
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        let checks = vec![build];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) = select_task_checks(&checks, &[], depth, true);
            assert!(
                selected.is_empty(),
                "depth {depth:?} should exclude perTask:false when apply_per_task_filter is true"
            );
            assert_eq!(selection.excluded, vec!["build".to_string()]);
        }
    }

    /// `EN.3.E` acceptance criterion: `apply_per_task_filter = false` — the
    /// `FinalValidationNode` branch — keeps a `"perTask": false` check
    /// instead of dropping it, at both depths.
    #[test]
    fn select_task_checks_keeps_per_task_false_when_filter_disabled() {
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        let checks = vec![build];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) = select_task_checks(&checks, &[], depth, false);
            assert_eq!(
                selected.len(),
                1,
                "depth {depth:?} should keep perTask:false when apply_per_task_filter is false"
            );
            assert_eq!(selected[0]["name"], json!("build"));
            assert!(selection.excluded.is_empty());
        }
    }

    #[test]
    fn select_task_checks_excludes_enabled_false() {
        let mut fmt = cmd_check("fmt", "cargo fmt --check");
        fmt["enabled"] = json!(false);
        let checks = vec![fmt];

        let (selected, selection) = select_task_checks(&checks, &[], TestDepth::Full, true);
        assert!(selected.is_empty());
        assert_eq!(selection.excluded, vec!["fmt".to_string()]);
    }

    #[test]
    fn select_task_checks_task_validation_commands_replaces_everything() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let task_commands = vec![
            "test -f docs/foo.md".to_string(),
            "grep -q bar docs/foo.md".to_string(),
        ];

        for depth in [TestDepth::Full, TestDepth::Fast] {
            let (selected, selection) =
                select_task_checks(&harness_checks, &task_commands, depth, true);
            assert_eq!(
                selected,
                vec![
                    json!({
                        "name": "task-validation-1",
                        "kind": "command",
                        "command": "test -f docs/foo.md",
                        "gates": true,
                    }),
                    json!({
                        "name": "task-validation-2",
                        "kind": "command",
                        "command": "grep -q bar docs/foo.md",
                        "gates": true,
                    }),
                ],
                "depth {depth:?} should not change the synthesized shape"
            );
            assert_eq!(selection.source, "task_validation_commands");
            assert!(selection.excluded.is_empty());
        }
    }

    #[test]
    fn select_task_checks_empty_task_validation_commands_falls_through_to_harness() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (selected, selection) = select_task_checks(&harness_checks, &[], TestDepth::Full, true);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["name"], json!("fmt"));
        assert_eq!(selection.source, "harness");
    }

    #[test]
    fn select_task_checks_source_literal_matches_branch() {
        let harness_checks = vec![cmd_check("fmt", "cargo fmt --check")];
        let (_, harness_selection) =
            select_task_checks(&harness_checks, &[], TestDepth::Full, true);
        assert_eq!(harness_selection.source, "harness");

        let (_, override_selection) = select_task_checks(
            &harness_checks,
            &["echo hi".to_string()],
            TestDepth::Full,
            true,
        );
        assert_eq!(override_selection.source, "task_validation_commands");
    }

    // --- TestTaskNode::process x select_task_checks wiring (task 4) --------

    /// A recording [`CommandRunner`] that always succeeds and records every
    /// `sh -c <command>` invocation's `<command>` string, in order.
    fn recording_command_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
        let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            if let Some(command) = args.get(1) {
                recorded_clone.lock().unwrap().push((*command).to_string());
            }
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });
        (runner, recorded)
    }

    /// A [`ctx_with_current_task`]-based ctx that also carries a
    /// `SetupWorktreeNode` output pointing at `worktree`, so `TestTaskNode`
    /// can resolve both the current task (for `validation_commands`) and the
    /// worktree path (for `planning/harness.json`).
    fn ctx_with_current_task_and_worktree(
        state: &SDLCState,
        task: &SDLCTask,
        worktree: &Path,
    ) -> TaskContext {
        let mut ctx = ctx_with_current_task(state, task);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx
    }

    #[tokio::test]
    async fn test_task_full_depth_runs_command_even_with_fast_command_present() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([cmd_check_with_fast(
                "test",
                "cargo nextest run --workspace",
                "cargo nextest run --lib --workspace"
            )]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec!["cargo nextest run --workspace"]
        );
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("full"));
        assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
    }

    #[tokio::test]
    async fn test_task_fast_depth_substitutes_fast_command() {
        let worktree = temp_worktree();
        write_harness(
            &worktree,
            json!([cmd_check_with_fast(
                "test",
                "cargo nextest run --workspace",
                "cargo nextest run --lib --workspace"
            )]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);
        let ctx = ctx_with_policy(
            ctx,
            &SdlcPolicy {
                test_depth: TestDepth::Fast,
                ..SdlcPolicy::default()
            },
        );

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec!["cargo nextest run --lib --workspace"]
        );
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("fast"));
    }

    #[tokio::test]
    async fn test_task_uses_task_validation_commands_over_harness() {
        let worktree = temp_worktree();
        write_harness(&worktree, json!([cmd_check("fmt", "cargo fmt --check")]));
        let mut task = SDLCTask::new(1, "One", "d1");
        task.validation_commands = vec!["test -f docs/foo.md".to_string()];
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(*recorded.lock().unwrap(), vec!["test -f docs/foo.md"]);
        assert_eq!(
            out.nodes["TestTaskNode"]["check_source"],
            json!("task_validation_commands")
        );
        assert!(out.nodes["TestTaskNode"]["excluded_checks"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_task_result_stamps_test_depth_check_source_excluded_checks() {
        let worktree = temp_worktree();
        let mut build = cmd_check("build", "cargo build --release");
        build["perTask"] = json!(false);
        write_harness(
            &worktree,
            json!([cmd_check("fmt", "cargo fmt --check"), build]),
        );
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, _recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("full"));
        assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
        assert_eq!(
            out.nodes["TestTaskNode"]["excluded_checks"],
            json!(["build"])
        );
    }

    #[tokio::test]
    async fn test_task_no_harness_and_no_validation_commands_is_gating_failure() {
        let worktree = temp_worktree();
        // No harness.json written — `temp_worktree` only creates the
        // `planning/` directory, not the file.
        let task = SDLCTask::new(1, "One", "d1");
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let node = TestTaskNode::new();
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(results
            .iter()
            .any(|r| r["name"] == json!("harness-missing")));
        assert!(out.nodes["TestTaskNode"]["failure_summary"]
            .as_str()
            .unwrap()
            .contains("harness-missing"));
    }

    #[tokio::test]
    async fn test_task_no_harness_but_with_validation_commands_runs_them_no_harness_missing() {
        let worktree = temp_worktree();
        // No harness.json written.
        let mut task = SDLCTask::new(1, "One", "d1");
        task.validation_commands = vec!["true".to_string()];
        let state = state_with_tasks(vec![task.clone()]);
        let ctx = ctx_with_current_task_and_worktree(&state, &task, &worktree);

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(*recorded.lock().unwrap(), vec!["true"]);
        let results = out.nodes["TestTaskNode"]["check_results"]
            .as_array()
            .unwrap();
        assert!(!results
            .iter()
            .any(|r| r["name"] == json!("harness-missing")));
    }
}
