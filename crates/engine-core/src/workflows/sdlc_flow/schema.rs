//! SDLC Flow schema types — a Rust port of
//! `orchestrator/app/schemas/sdlc_schema.py`.
//!
//! Field names, defaults, and serde shapes match the Python `model_dump()` /
//! `model_dump_json()` output byte-for-byte (semantic JSON): the `StrEnum`
//! variants serialize as their lowercase/UPPERCASE string values exactly as
//! written in the Python source, and every `Option`/`Vec`/numeric default
//! mirrors the corresponding Pydantic `Field(default=...)`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::node::NodeError;

use super::policy::{PartialPolicy, SdlcPolicy};

/// Lifecycle states for a single SDLC task (`SDLCTaskStatus` in Python).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SDLCTaskStatus {
    #[default]
    Pending,
    InProgress,
    Done,
    Failed,
    Skipped,
}

/// Classification produced by `TriageTaskNode` for a test-failure
/// (`SDLCTriageVerdict` in Python).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SDLCTriageVerdict {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "RETRYABLE")]
    Retryable,
    #[serde(rename = "MAJOR_BAIL")]
    MajorBail,
}

/// Verdict produced by `ConsolidatedReviewNode` (`SDLCReviewVerdict` in
/// Python).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SDLCReviewVerdict {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "FAIL")]
    Fail,
    #[serde(rename = "PARTIAL")]
    Partial,
}

fn default_max_attempts() -> u32 {
    3
}

/// A single task within an SDLC spec's task list (`SDLCTask` in Python).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SDLCTask {
    /// 1-indexed task number within the spec.
    pub task_id: u32,
    /// Short human-readable task title.
    pub title: String,
    /// Full task description / implementation notes.
    pub description: String,
    /// Observable acceptance criteria for this task.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// Current lifecycle status.
    #[serde(default)]
    pub status: SDLCTaskStatus,
    /// Shell commands used to validate this task's implementation.
    #[serde(default)]
    pub validation_commands: Vec<String>,
    /// Number of implement -> test attempts made so far.
    #[serde(default)]
    pub attempt_count: u32,
    /// Maximum number of attempts before a MAJOR_BAIL.
    ///
    /// **Precedence: task-declared > policy > built-in default.** A task
    /// whose own JSON declares `max_attempts` always keeps that value; a
    /// task that omits the field is seeded from the run's resolved
    /// `SdlcPolicy::max_attempts` at task-creation/load time
    /// (`setup.rs`'s `GenerateTasksNode` and `LoadTaskStateNode`), which
    /// itself falls back to this field's `#[serde(default)]` of `3` only
    /// when nothing set the policy either. The two same-named fields
    /// (this one and `SdlcPolicy::max_attempts`) exist for different
    /// audiences — this one is per-task, the policy one is the run-wide
    /// seed for tasks that do not opt out of it — and the two must never
    /// be conflated.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

impl SDLCTask {
    /// Construct a new pending task with the Python-side defaults
    /// (`status = PENDING`, `attempt_count = 0`, `max_attempts = 3`).
    pub fn new(task_id: u32, title: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            task_id,
            title: title.into(),
            description: description.into(),
            acceptance_criteria: Vec::new(),
            status: SDLCTaskStatus::default(),
            validation_commands: Vec::new(),
            attempt_count: 0,
            max_attempts: default_max_attempts(),
        }
    }
}

fn default_auto_pr() -> bool {
    true
}

/// Inbound event schema for the `SDLC_FLOW` workflow
/// (`SDLCFlowEventSchema` in Python).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SDLCFlowEventSchema {
    /// Slug identifying the target spec directory.
    pub spec_slug: String,
    /// Optional target-repository **registry slug** (e.g. `"bastion"`,
    /// `"mev"`, `"engine-rs"`) — resolved through the process-wide
    /// `RepoRegistry` (`brain.toml`'s `[[repos]]` entries) into an absolute
    /// filesystem root. This field is a **slug and never a path**: a path on
    /// the wire would let anything holding the API key aim an autonomous
    /// `dangerously_skip_permissions` agent (`graph.rs`'s
    /// `agentic_write_config`) at any directory on the machine, whereas a
    /// slug keeps the reachable set a deliberate, reviewable list — the
    /// blast radius of a leaked `X-API-Key`, a typo, or a compromised caller
    /// becomes "one of the ~20 repos in `brain.toml`" instead of "the
    /// filesystem". `None`/absent preserves today's behavior exactly: the
    /// target root resolves to the serving process's `current_dir()`. Never
    /// accept, join, or canonicalize a caller-supplied path here or anywhere
    /// downstream of this field.
    #[serde(default)]
    pub repo: Option<String>,
    /// Optional task-range filter, e.g. `"1-3,5"` (1-indexed, inclusive).
    #[serde(default)]
    pub task_range: Option<String>,
    /// Whether to reattach to an existing worktree/state.
    #[serde(default)]
    pub resume: bool,
    /// Whether to open a PR automatically once the run completes.
    #[serde(default = "default_auto_pr")]
    pub auto_pr: bool,
    /// Override for the git branch name; derived from `spec_slug` if unset.
    #[serde(default)]
    pub branch_name: Option<String>,
    /// Whether `TriageTaskNode` should invoke the LLM classifier for a
    /// failing-but-under-budget task. When `false` (default), such a task is
    /// deterministically classified `RETRYABLE` and the attempt counter
    /// remains the sole bail gate; when `true`, the model decides
    /// `RETRYABLE` vs `MAJOR_BAIL` (early-bail heuristic).
    #[serde(default)]
    pub llm_triage: bool,
    /// Whether to use a git worktree (in `trees/{branch}`) or just checkout
    /// the branch in the current directory.
    #[serde(default)]
    pub use_worktree: bool,
    /// Optional per-run policy override (EN.3.C) — the highest-precedence of
    /// the three `SdlcPolicy` resolution layers (event override >
    /// `harness.json` `sdlc.policy` defaults > built-in default). Additive:
    /// every existing field above is untouched byte-for-byte. `None`/absent
    /// means "no per-run override", falling through to the next layer down.
    #[serde(default)]
    pub policy: Option<PartialPolicy>,
    /// Optional name of a built-in or `harness.json`-defined policy profile
    /// bundle (e.g. `"cheap-fast"`) to apply for this run. Resolved between
    /// the `harness.json` `sdlc.policy` defaults layer and the event-inline
    /// `policy` override layer: `policy` (this event) > `profile` (this
    /// event) > `sdlc.policy` (harness defaults) > built-in default.
    /// Additive: every existing field above is untouched byte-for-byte.
    /// `None`/absent means "no named profile", falling through to the next
    /// layer down.
    #[serde(default)]
    pub profile: Option<String>,
    /// Optional owning program block identifier (e.g. `"EN.3.A"`), carried
    /// onto the created [`SDLCState`] (`LoadTaskStateNode`) so a later
    /// `CloseBlockNode` (EN.ticket.wrap-up-closes-the-block) knows which
    /// block this run is for without re-deriving it from the spec
    /// directory name. `None`/absent preserves today's behavior exactly:
    /// `SDLCState::block_id` stays `None`, same as before this field
    /// existed. Only consulted when a state file does not already exist for
    /// this spec — a resumed run keeps whatever `block_id` was already
    /// committed to disk rather than letting a differing event value
    /// silently rewrite it.
    #[serde(default)]
    pub block_id: Option<String>,
    /// Optional owning phase identifier (e.g. `"EN.3"`), carried onto the
    /// created [`SDLCState`] the same way and under the same rules as
    /// [`Self::block_id`].
    #[serde(default)]
    pub phase_id: Option<String>,
}

/// Parse a task-range string like `"1-3,5"` into a sorted, deduplicated list
/// of task ids. Returns `Ok(None)` if `task_range` is `None` (meaning: all
/// tasks). Mirrors `SDLCFlowEventSchema.parse_task_range` in Python,
/// including its `end < start` rejection.
pub fn parse_task_range(task_range: Option<&str>) -> Result<Option<Vec<u32>>, String> {
    let Some(task_range) = task_range else {
        return Ok(None);
    };

    let mut task_ids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for chunk in task_range.split(',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if let Some((start_str, end_str)) = chunk.split_once('-') {
            let start: u32 = start_str
                .trim()
                .parse()
                .map_err(|_| format!("Invalid task range chunk: {chunk:?}"))?;
            let end: u32 = end_str
                .trim()
                .parse()
                .map_err(|_| format!("Invalid task range chunk: {chunk:?}"))?;
            if end < start {
                return Err(format!("Invalid task range chunk (end < start): {chunk:?}"));
            }
            task_ids.extend(start..=end);
        } else {
            let id: u32 = chunk
                .parse()
                .map_err(|_| format!("Invalid task range chunk: {chunk:?}"))?;
            task_ids.insert(id);
        }
    }
    Ok(Some(task_ids.into_iter().collect()))
}

/// Aggregate telemetry counters for an SDLC flow run (`SDLCTelemetry` in
/// Python).
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SDLCTelemetry {
    /// Total implement -> test attempts made.
    #[serde(default)]
    pub total_attempts: u32,
    /// Cumulative token/dollar cost spent.
    #[serde(default)]
    pub budget_spent: f64,
    /// Number of tasks that reached DONE.
    #[serde(default)]
    pub tasks_passed: u32,
    /// Number of tasks that reached FAILED.
    #[serde(default)]
    pub tasks_failed: u32,
    /// Number of `ConsolidatedReviewNode` verdicts produced so far
    /// (EN.ticket.review-retry-loop-unbounded task 2) — the durable counter
    /// `ReviewRouterNode` bounds against, independent of `total_attempts`.
    ///
    /// **Scoped per-run, not per-task**, matching JS's `reviewAttempts`
    /// (`base-template/.claude/workflows/sdlc-flow.js:252-253`), a single
    /// loop counter for the run's review cycle — not per-task like
    /// `SDLCTask::attempt_count`. This also matches where the field
    /// physically lives: `SDLCTelemetry` is a whole-run aggregate on
    /// `SDLCState`, not a per-`SDLCTask` field, so a fresh task does not
    /// reset it. Deliberately NOT folded into `total_attempts`/
    /// `attempt_count` (both bumped by `IncrementAttemptNode` on both
    /// back-edges and consumed by `RunTelemetry`/`PolicyAggregate` to mean
    /// "an implement/test attempt ran") — overloading them would make a
    /// task that burned test retries arrive at review with a reduced review
    /// budget, which is not the bound this ticket asks for, and would
    /// corrupt existing attempt metrics. Same reasoning `wrap_up.rs` gives
    /// for not letting bails increment `tasks_failed`.
    #[serde(default)]
    pub review_attempts: u32,
}

fn default_global_status() -> String {
    SDLCTaskStatus::Pending.as_str().to_string()
}

/// Snapshot of a completed (or bailed) run's outcome metrics (EN.3.C task
/// 6), written once at the run tail (`WrapUpNode`) so `(policy -> outcome)`
/// pairs can be tabulated across runs by a later cross-run aggregator. All
/// fields default to zero/empty so a run that never reaches the tail (or a
/// unit test driving a node in isolation) still round-trips cleanly.
///
/// **EN.4.0:** this shape is field-for-field identical to the generic
/// `crate::policy::RunTelemetry` (see the `From` impls below), which
/// `wrap_up::finalize_outcomes` now uses to compute the ctx-derived fields
/// via `crate::policy::telemetry::harvest` rather than re-deriving them
/// locally. The struct itself is kept flat (not `#[serde(flatten)]`-wrapped)
/// so its JSON shape — and every existing direct field access across this
/// crate's tests — stays byte-identical to pre-EN.4.0.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunOutcomes {
    /// Wall-clock seconds from `SetupWorktreeNode`'s `started_at` to the
    /// moment this snapshot was computed. `0.0` when the start time is
    /// unavailable (e.g. a unit test that never ran `SetupWorktreeNode`).
    #[serde(default)]
    pub wall_clock_secs: f64,
    /// Total implement -> test attempts made across every task (mirrors
    /// `SDLCTelemetry::total_attempts`).
    #[serde(default)]
    pub total_attempts: u32,
    /// Sum, across every task, of attempts beyond its first (i.e. total
    /// retries triggered by `RETRYABLE`/minor-`FAIL` back-edges).
    #[serde(default)]
    pub total_retries: u32,
    /// Number of tasks that reached `DONE` (mirrors
    /// `SDLCTelemetry::tasks_passed`).
    #[serde(default)]
    pub tasks_passed: u32,
    /// Number of tasks that reached `FAILED` (mirrors
    /// `SDLCTelemetry::tasks_failed`).
    #[serde(default)]
    pub tasks_failed: u32,
    /// Review/triage verdicts observed by the time this snapshot was taken
    /// (e.g. `"TriageTaskNode:RETRYABLE"`, `"ConsolidatedReviewNode:PASS"`);
    /// empty if neither stage has run.
    #[serde(default)]
    pub review_verdicts: Vec<String>,
    /// Total input tokens summed across every model node's last recorded
    /// usage in `ctx.node_runs`.
    #[serde(default)]
    pub total_input_tokens: u64,
    /// Total output tokens summed across every model node's last recorded
    /// usage in `ctx.node_runs`.
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Total dollar cost summed across every model node's last recorded
    /// `cost_usd` in `ctx.nodes`.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Per-stage model tier actually used this run, keyed by the resolved
    /// policy's `ModelTiers` field names (`"implement"`, `"triage"`,
    /// `"review"`, `"implement_simple"`, `"generate"`) — so
    /// local-vs-cloud quality is measurable across runs.
    #[serde(default)]
    pub model_tier_used: BTreeMap<String, String>,
}

impl From<crate::policy::RunTelemetry> for RunOutcomes {
    fn from(telemetry: crate::policy::RunTelemetry) -> Self {
        Self {
            wall_clock_secs: telemetry.wall_clock_secs,
            total_attempts: telemetry.total_attempts,
            total_retries: telemetry.total_retries,
            tasks_passed: telemetry.tasks_passed,
            tasks_failed: telemetry.tasks_failed,
            review_verdicts: telemetry.review_verdicts,
            total_input_tokens: telemetry.total_input_tokens,
            total_output_tokens: telemetry.total_output_tokens,
            total_cost_usd: telemetry.total_cost_usd,
            model_tier_used: telemetry.model_tier_used,
        }
    }
}

impl From<RunOutcomes> for crate::policy::RunTelemetry {
    fn from(outcomes: RunOutcomes) -> Self {
        Self {
            wall_clock_secs: outcomes.wall_clock_secs,
            total_attempts: outcomes.total_attempts,
            total_retries: outcomes.total_retries,
            tasks_passed: outcomes.tasks_passed,
            tasks_failed: outcomes.tasks_failed,
            review_verdicts: outcomes.review_verdicts,
            total_input_tokens: outcomes.total_input_tokens,
            total_output_tokens: outcomes.total_output_tokens,
            total_cost_usd: outcomes.total_cost_usd,
            model_tier_used: outcomes.model_tier_used,
        }
    }
}

impl SDLCTaskStatus {
    /// The exact string this status serializes to (Python `StrEnum` value).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// The full durable state for an in-flight or completed SDLC flow run
/// (`SDLCState` in Python).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SDLCState {
    /// Slug identifying the target spec directory.
    pub spec_slug: String,
    /// Owning phase identifier, if known.
    #[serde(default)]
    pub phase_id: Option<String>,
    /// Owning program block identifier, if known.
    #[serde(default)]
    pub block_id: Option<String>,
    /// Overall run status (mirrors `SDLCTaskStatus` values).
    #[serde(default = "default_global_status")]
    pub global_status: String,
    /// Ordered list of tasks in this spec.
    #[serde(default)]
    pub tasks: Vec<SDLCTask>,
    /// Aggregate telemetry for this run.
    #[serde(default)]
    pub telemetry: SDLCTelemetry,
    /// The three-layer-resolved `SdlcPolicy` this run executed under
    /// (EN.3.C task 6). `None` until `WrapUpNode` stamps it at the run
    /// tail (or for states from before EN.3.C).
    #[serde(default)]
    pub policy: Option<SdlcPolicy>,
    /// The run's outcome-metrics snapshot (EN.3.C task 6). `None` until
    /// `WrapUpNode` finalizes it at the run tail.
    #[serde(default)]
    pub outcomes: Option<RunOutcomes>,
    /// D31-committed-state field (EN.4.1): the git branch this run executed
    /// on, stamped by `SetupWorktreeNode`. `None` for states from before this
    /// fix, or for a state that never went through `SetupWorktreeNode`.
    #[serde(default)]
    pub branch: Option<String>,
    /// D31-committed-state field (EN.4.1): the worktree path this run
    /// executed in, stamped by `SetupWorktreeNode`. Same absence rules as
    /// [`Self::branch`].
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// D31-committed-state field (EN.4.1): ISO-8601 timestamp of the run's
    /// first `SaveStateNode`/`WrapUpNode` write, preserved across resumes
    /// (see `task_loop::SaveStateNode::process`'s resume handling).
    #[serde(default)]
    pub started_at: Option<String>,
    /// D31-committed-state field (EN.4.1): ISO-8601 timestamp of the most
    /// recent write to this state, recomputed fresh on every save.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// D31-committed-state field (EN.4.1): human-readable reason the run
    /// terminated early (`MAJOR_BAIL` / structural review failure), or
    /// `None` on the happy path. Derived by [`derive_bail_reason`] from a
    /// [`TerminalSignal`] at the point of writing, not maintained
    /// incrementally.
    #[serde(default)]
    pub bail_reason: Option<String>,
    /// D31-committed-state field (EN.6.J): the engine's `events.id` run UUID
    /// that produced this write, stamped into `ctx.metadata` by
    /// `Workflow::run_with`/`run_from` and read back out by each writer's
    /// `build_run_meta`. `None` for states from before this fix, for any
    /// state written by base-template's JS `sdlc-flow.js` engine (which never
    /// sets it), or for a run that had no run_id stamped.
    #[serde(default)]
    pub run_id: Option<String>,
}

impl SDLCState {
    /// Construct a new state for `spec_slug` with no tasks yet and default
    /// telemetry/global-status, mirroring the Python model's defaults.
    pub fn new(spec_slug: impl Into<String>) -> Self {
        Self {
            spec_slug: spec_slug.into(),
            phase_id: None,
            block_id: None,
            global_status: default_global_status(),
            tasks: Vec::new(),
            telemetry: SDLCTelemetry::default(),
            policy: None,
            outcomes: None,
            branch: None,
            worktree_path: None,
            started_at: None,
            updated_at: None,
            bail_reason: None,
            run_id: None,
        }
    }

    /// Reshape this state into the D31-committed on-disk JSON shape shared
    /// with base-template's `.claude/workflows/sdlc-flow.js` (governed by
    /// `D31-committed-authoritative-state.md`, aligned here by
    /// `D10-committed-state-path-schema-alignment.md`): `tasks` becomes a
    /// JSON object keyed by each task's `task_id` (as a string) rather than
    /// an array, `status`/`current_task`/`bail_reason` are derived rather
    /// than stored, and `branch`/`worktree_path`/`started_at`/`updated_at`
    /// come from the caller's [`RunMeta`] (the authoritative values for
    /// *this* write) rather than `self`'s own same-named fields (those exist
    /// only so [`Self::from_committed_state_json`] can round-trip them back
    /// onto a resumed `SDLCState`, e.g. so `started_at` survives a resume —
    /// see `task_loop::SaveStateNode`).
    ///
    /// `review`/`docs`/`pr` are accepted as explicit parameters rather than
    /// stored as `SDLCState` fields: they are transient, per-write outputs
    /// (`ConsolidatedReviewNode`/`PatchDocsNode`/`PullRequestNode`'s results)
    /// that only exist at specific points in the run, and storing them on
    /// the long-lived `SDLCState` risks a resumed load replaying a *stale*
    /// review/docs/pr block from a previous save rather than reflecting
    /// "not available yet". Passing them as parameters means each call site
    /// supplies exactly what it currently has (or `None`).
    ///
    /// `telemetry`/`policy`/`outcomes`/`phase_id`/`block_id`/`final_validation`
    /// ride along as additive top-level keys — every real consumer of this
    /// file (`bastion`'s `flow.rs`, `run-sdlc-flow.sh`'s `jq` queries) ignores
    /// unknown keys (no `deny_unknown_fields` anywhere in this codebase, and
    /// D37 already set this additive-schema-growth precedent for `tokens`).
    /// `final_validation` (EN.3.E) follows the exact same precedent:
    /// base-template's JS `sdlc-flow.js` engine will never emit this key, so
    /// a file it wrote round-trips through [`Self::from_committed_state_json`]
    /// reading it back as `None`.
    #[must_use]
    pub fn to_committed_state_json(
        &self,
        run_meta: &RunMeta,
        review: Option<&CommittedReview>,
        docs: Option<&CommittedDocs>,
        pr: Option<&CommittedPr>,
        final_validation: Option<&CommittedFinalValidation>,
        terminal_signal: Option<&TerminalSignal>,
    ) -> serde_json::Value {
        let mut tasks_obj = serde_json::Map::with_capacity(self.tasks.len());
        for task in &self.tasks {
            tasks_obj.insert(
                task.task_id.to_string(),
                serde_json::to_value(task).unwrap_or(serde_json::Value::Null),
            );
        }

        let status = derive_committed_status(self, terminal_signal);
        let current_task = derive_current_task(&self.tasks);
        let bail_reason = derive_bail_reason(terminal_signal);

        serde_json::json!({
            "spec_slug": self.spec_slug,
            "branch": run_meta.branch,
            "worktree_path": run_meta.worktree_path,
            "started_at": run_meta.started_at,
            "updated_at": run_meta.updated_at,
            "run_id": run_meta.run_id,
            "status": status,
            "current_task": current_task,
            "tasks": serde_json::Value::Object(tasks_obj),
            "review": review,
            "docs": docs,
            "bail_reason": bail_reason,
            "pr": pr,
            "telemetry": self.telemetry,
            "policy": self.policy,
            "outcomes": self.outcomes,
            "phase_id": self.phase_id,
            "block_id": self.block_id,
            "final_validation": final_validation,
        })
    }

    /// Reverse of [`Self::to_committed_state_json`]: parse a D31-committed
    /// JSON `Value` (as read off disk) back into an `SDLCState`. `tasks` is
    /// read out of its object-keyed-by-`task_id` shape and sorted
    /// numerically by the object's own string keys back into
    /// `Vec<SDLCTask>`. `review`/`docs`/`pr` are intentionally NOT
    /// reconstructed onto the returned `SDLCState` (see the field-vs-param
    /// design note on [`Self::to_committed_state_json`]) — callers that need
    /// them should read the parsed `Value` directly.
    pub fn from_committed_state_json(value: &serde_json::Value) -> Result<Self, NodeError> {
        let spec_slug = value
            .get("spec_slug")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| NodeError::new("committed state JSON missing \"spec_slug\""))?
            .to_string();

        let tasks = match value.get("tasks") {
            Some(serde_json::Value::Object(map)) => {
                let mut entries: Vec<(u32, SDLCTask)> = Vec::with_capacity(map.len());
                for (key, task_value) in map {
                    let task_id: u32 = key.parse().map_err(|_| {
                        NodeError::new(format!("committed state JSON: invalid task id key {key:?}"))
                    })?;
                    let task: SDLCTask =
                        serde_json::from_value(task_value.clone()).map_err(|err| {
                            NodeError::new(format!(
                                "committed state JSON: failed to parse task {key:?}: {err}"
                            ))
                        })?;
                    entries.push((task_id, task));
                }
                entries.sort_by_key(|(task_id, _)| *task_id);
                entries.into_iter().map(|(_, task)| task).collect()
            }
            // Absent, null, or (defensively) a legacy array — no tasks
            // recoverable from this shape; callers that need the array form
            // never produced this file in the first place post-EN.4.1.
            _ => Vec::new(),
        };

        let telemetry = value
            .get("telemetry")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let policy = value
            .get("policy")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let outcomes = value
            .get("outcomes")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let phase_id = value
            .get("phase_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let block_id = value
            .get("block_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let branch = value
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let worktree_path = value
            .get("worktree_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let started_at = value
            .get("started_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let updated_at = value
            .get("updated_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let bail_reason = value
            .get("bail_reason")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        // Absent key or explicit JSON `null` both fall through to `None` —
        // covers both a state written before this fix and base-template's
        // JS engine's shape, which never emits this key at all.
        let run_id = value
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let global_status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(default_global_status);

        Ok(Self {
            spec_slug,
            phase_id,
            block_id,
            global_status,
            tasks,
            telemetry,
            policy,
            outcomes,
            branch,
            worktree_path,
            started_at,
            updated_at,
            bail_reason,
            run_id,
        })
    }
}

/// Run-level metadata supplied fresh at every
/// [`SDLCState::to_committed_state_json`] call — the D31 fields that live
/// outside `SDLCState`'s own long-lived data (see that method's doc comment
/// for why `branch`/`worktree_path`/`started_at`/`updated_at` are parameters
/// rather than read from `self`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMeta {
    /// The git branch this run executed on.
    pub branch: String,
    /// The worktree (or plain checkout) path this run executed in.
    pub worktree_path: String,
    /// ISO-8601 timestamp of this run's first state write. Callers preserve
    /// this across resumes rather than recomputing it fresh.
    pub started_at: String,
    /// ISO-8601 timestamp of this specific write. Always recomputed fresh.
    pub updated_at: String,
    /// The engine's `events.id` run UUID (as a hyphenated string) that
    /// produced this write, if this run has one. `None` for states written
    /// by base-template's JS `sdlc-flow.js` engine, which never sets it, and
    /// for any engine-rs run that (for whatever reason) never had a run_id
    /// stamped into `ctx.metadata` (see `Workflow::run_with`/`run_from`).
    pub run_id: Option<String>,
}

/// The `ConsolidatedReviewNode` verdict block, reshaped into the D31
/// committed-state `review` key (`{verdict, findings, attempts}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedReview {
    /// The review verdict string (`"PASS"` / `"FAIL"` / `"PARTIAL"`).
    pub verdict: String,
    /// Issues raised by the review, if any.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Number of review attempts made.
    #[serde(default)]
    pub attempts: u32,
}

/// The `PatchDocsNode` output, reshaped into the D31 committed-state `docs`
/// key (`{changed, created}`). `PatchDocsNode` only ever patches existing
/// docs (`files_patched`) — it has no concept of newly-created files — so
/// `created` is always empty from this engine today; the field exists for
/// D31-shape parity and forward-compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommittedDocs {
    /// Doc files patched this run.
    #[serde(default)]
    pub changed: Vec<String>,
    /// Doc files newly created this run (always empty; see struct doc).
    #[serde(default)]
    pub created: Vec<String>,
}

/// The `PullRequestNode` output, reshaped into the D31 committed-state `pr`
/// key (`{url, number}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedPr {
    /// The opened PR's URL.
    pub url: String,
    /// The opened PR's number.
    pub number: u64,
}

/// The `FinalValidationNode` output, reshaped into the additive D31
/// committed-state `final_validation` key (EN.3.E task 3):
/// `{all_passed, failure_summary, check_results}`. Reuses
/// [`super::task_loop::CheckResult`] verbatim for `check_results` rather than
/// inventing a parallel shape — `TestTaskNode` and `FinalValidationNode`
/// stamp the exact same per-check result type into `ctx.nodes`.
///
/// Transient like [`CommittedReview`]/[`CommittedDocs`]/[`CommittedPr`]: it
/// is not stored as an `SDLCState` field, only accepted as a fresh parameter
/// on each [`SDLCState::to_committed_state_json`] call — `WrapUpNode` is the
/// only run-level site that ever has a `FinalValidationNode` result to
/// reshape, so there is nothing to preserve across a resume the way
/// `telemetry`/`policy`/`outcomes` are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedFinalValidation {
    /// Whether every gating check in the final-validation run passed.
    pub all_passed: bool,
    /// Human-readable summary of the failing check names, empty when
    /// `all_passed` is `true`.
    #[serde(default)]
    pub failure_summary: String,
    /// Per-check results from the full-depth, unfiltered harness run.
    #[serde(default)]
    pub check_results: Vec<super::task_loop::CheckResult>,
}

/// Why an SDLC Flow run ended before every task completed cleanly — the one
/// shared seam [`derive_committed_status`] and [`derive_bail_reason`] both
/// key off of, so a run's `status`/`bail_reason` can never independently
/// disagree about *whether* it bailed (only about the message). `None`
/// (absent, not a variant) means the happy path: still running, or done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalSignal {
    /// `TriageTaskNode` classified a task's repeated test failure as
    /// unrecoverable (`SDLCTriageVerdict::MajorBail`, typically "max
    /// attempts reached").
    MajorBail(String),
    /// `ConsolidatedReviewNode` returned a definitive `FAIL` verdict that
    /// `ReviewRouterNode` routed straight to `WrapUpNode` (0 issues, or more
    /// issues than the structural threshold, `task_loop::
    /// STRUCTURAL_ISSUE_THRESHOLD`) rather than back through the retry loop.
    ReviewFail(String),
    /// `ConsolidatedReviewNode` returned `PARTIAL` but `ReviewRouterNode`
    /// still routed to `WrapUpNode` because the issue count was structural
    /// (0, or over threshold) rather than a small fixable set — the same
    /// routing gate as [`Self::ReviewFail`], distinguished only by the
    /// upstream verdict string.
    StructuralFail(String),
    /// `FinalValidationNode` stamped `all_passed: false` on the run-level,
    /// full-depth harness gate. Unlike the other three variants, this one is
    /// not a model judgment about a single task or review pass — it is
    /// derived from a RUN-LEVEL gate that runs once, after every task's own
    /// loop already reported success, and it is the only variant that can
    /// fire on a run where every task passed. The carried `String` is the
    /// gate's `failure_summary` (the failing check names).
    FinalValidationFailed(String),
    /// `EndReviewNode` (`EN.ticket.review-mode-endonly-reviews-nothing`)
    /// returned a `FAIL`/`PARTIAL` verdict over the whole run's diff against
    /// the complete Acceptance Criteria, and `EndReviewRouterNode` routed
    /// straight to `WrapUpNode` rather than `PatchDocsNode`. Only reachable
    /// under `ReviewMode::EndOnly` — `EndReviewNode` is a zero-call
    /// pass-through under `PerTask`/`TrivialSkip`, so this variant never
    /// fires for those modes. An explicit [`Self::MajorBail`] (checked
    /// first in `wrap_up::derive_terminal_signal`) still wins over this one.
    /// The carried `String` names the unmet criteria (the review's summary,
    /// plus its issue list when non-empty).
    EndReviewFail(String),
    /// `ReviewRouterNode`'s minor-issue back-edge (`FAIL`/`PARTIAL`, 1..=
    /// `STRUCTURAL_ISSUE_THRESHOLD` issues) hit `policy.max_review_attempts`
    /// (EN.ticket.review-retry-loop-unbounded task 3) — the run's per-run,
    /// durable `telemetry.review_attempts` counter reached the bound before
    /// the reviewer returned a clean `PASS`. Distinguished from
    /// [`Self::ReviewFail`]/[`Self::StructuralFail`], which fire on the
    /// FIRST structural verdict regardless of attempt count: this variant
    /// only fires after the retry budget itself is exhausted. An explicit
    /// [`Self::MajorBail`] or a structural review `FAIL` (checked first in
    /// `wrap_up::derive_terminal_signal`, same precedence as today) still
    /// wins over this one. The carried `String` is the last review verdict's
    /// `summary`.
    ReviewExhausted(String),
}

impl TerminalSignal {
    /// The human-readable message carried by whichever variant this is.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::MajorBail(message)
            | Self::ReviewFail(message)
            | Self::StructuralFail(message)
            | Self::FinalValidationFailed(message)
            | Self::EndReviewFail(message)
            | Self::ReviewExhausted(message) => message,
        }
    }
}

/// Derive the D31 committed-state `status` string
/// (`"running"|"review"|"docs"|"wrapup"|"blocked"|"done"`) from a state
/// snapshot and an optional [`TerminalSignal`].
///
/// A `Some` terminal signal (of any variant) always yields `"blocked"` — the
/// one real terminal-failure value this engine ever writes (there is no
/// `"bailed"` anywhere in the codebase; see `run-sdlc-flow.sh`). Absent a
/// terminal signal, every task reaching `Done`/`Skipped` yields `"done"`;
/// anything else yields `"running"`.
///
/// `"review"`/`"docs"`/`"wrapup"` are base-template JS-engine-only
/// intermediate-phase markers: that engine writes committed state at each of
/// those distinct phase boundaries. This Rust engine has only two state-write
/// call sites — `task_loop::SaveStateNode` (mid task-loop, always
/// `"running"` unless a terminal signal is threaded in) and
/// `wrap_up::WrapUpNode` (the run's tail, `"done"` or `"blocked"`) — so those
/// three literal values are currently unreachable outputs; the return type
/// still allows for them for D31-shape parity and forward-compatibility if a
/// future call site ever writes state mid-docs/mid-review.
#[must_use]
pub fn derive_committed_status(
    state: &SDLCState,
    terminal_signal: Option<&TerminalSignal>,
) -> &'static str {
    if terminal_signal.is_some() {
        return "blocked";
    }
    let all_terminal = !state.tasks.is_empty()
        && state
            .tasks
            .iter()
            .all(|task| matches!(task.status, SDLCTaskStatus::Done | SDLCTaskStatus::Skipped));
    if all_terminal {
        "done"
    } else {
        "running"
    }
}

/// Derive the D31 committed-state `current_task` value: the lowest
/// `task_id` among tasks not yet `Done`/`Skipped`, or (if every task is
/// terminal) the highest `task_id`, or `0` if `tasks` is empty.
#[must_use]
pub fn derive_current_task(tasks: &[SDLCTask]) -> u32 {
    let next_pending = tasks
        .iter()
        .filter(|task| !matches!(task.status, SDLCTaskStatus::Done | SDLCTaskStatus::Skipped))
        .map(|task| task.task_id)
        .min();
    if let Some(task_id) = next_pending {
        return task_id;
    }
    tasks.iter().map(|task| task.task_id).max().unwrap_or(0)
}

/// Derive the D31 committed-state `bail_reason`: `Some(message)` for a
/// `Some` [`TerminalSignal`] of any variant, `None` on the happy path.
///
/// [`TerminalSignal::FinalValidationFailed`] is formatted as
/// `Final validation gate FAILED: <failure_summary>` — the same wording
/// [`super::wrap_up::WrapUpNode`]'s `gate_note` already renders, so the
/// prose and this machine-readable field cannot disagree.
#[must_use]
pub fn derive_bail_reason(terminal_signal: Option<&TerminalSignal>) -> Option<String> {
    terminal_signal.map(|signal| match signal {
        TerminalSignal::FinalValidationFailed(failure_summary) => {
            format!("Final validation gate FAILED: {failure_summary}")
        }
        TerminalSignal::ReviewExhausted(summary) => {
            format!("Review attempts exhausted: {summary}")
        }
        other => other.message().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::test_support::assert_json_subset;

    #[test]
    fn task_status_serializes_to_python_strenum_values() {
        assert_eq!(
            serde_json::to_value(SDLCTaskStatus::Pending).unwrap(),
            serde_json::json!("pending")
        );
        assert_eq!(
            serde_json::to_value(SDLCTaskStatus::InProgress).unwrap(),
            serde_json::json!("in_progress")
        );
        assert_eq!(
            serde_json::to_value(SDLCTaskStatus::Done).unwrap(),
            serde_json::json!("done")
        );
        assert_eq!(
            serde_json::to_value(SDLCTaskStatus::Failed).unwrap(),
            serde_json::json!("failed")
        );
        assert_eq!(
            serde_json::to_value(SDLCTaskStatus::Skipped).unwrap(),
            serde_json::json!("skipped")
        );
    }

    #[test]
    fn triage_verdict_serializes_to_python_strenum_values() {
        assert_eq!(
            serde_json::to_value(SDLCTriageVerdict::Pass).unwrap(),
            serde_json::json!("PASS")
        );
        assert_eq!(
            serde_json::to_value(SDLCTriageVerdict::Retryable).unwrap(),
            serde_json::json!("RETRYABLE")
        );
        assert_eq!(
            serde_json::to_value(SDLCTriageVerdict::MajorBail).unwrap(),
            serde_json::json!("MAJOR_BAIL")
        );
    }

    #[test]
    fn review_verdict_serializes_to_python_strenum_values() {
        assert_eq!(
            serde_json::to_value(SDLCReviewVerdict::Pass).unwrap(),
            serde_json::json!("PASS")
        );
        assert_eq!(
            serde_json::to_value(SDLCReviewVerdict::Fail).unwrap(),
            serde_json::json!("FAIL")
        );
        assert_eq!(
            serde_json::to_value(SDLCReviewVerdict::Partial).unwrap(),
            serde_json::json!("PARTIAL")
        );
    }

    #[test]
    fn sdlc_task_defaults_match_python() {
        let json = serde_json::json!({
            "task_id": 1,
            "title": "Do the thing",
            "description": "Full description",
        });
        let task: SDLCTask = serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(task.acceptance_criteria, Vec::<String>::new());
        assert_eq!(task.status, SDLCTaskStatus::Pending);
        assert_eq!(task.validation_commands, Vec::<String>::new());
        assert_eq!(task.attempt_count, 0);
        assert_eq!(task.max_attempts, 3);
    }

    #[test]
    fn sdlc_flow_event_schema_defaults_match_python() {
        let json = serde_json::json!({ "spec_slug": "EN.3.A" });
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json).expect("deserializes with defaults");
        assert_eq!(event.repo, None);
        assert_eq!(event.task_range, None);
        assert!(!event.resume);
        assert!(event.auto_pr);
        assert_eq!(event.branch_name, None);
        assert!(!event.llm_triage);
        assert!(!event.use_worktree);
        assert_eq!(event.policy, None);
        assert_eq!(event.profile, None);
    }

    #[test]
    fn sdlc_flow_event_schema_deserializes_repo_slug() {
        let json = serde_json::json!({
            "spec_slug": "EN.3.K",
            "repo": "bastion",
        });
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json).expect("deserializes with repo slug");
        assert_eq!(event.repo, Some("bastion".to_string()));
    }

    #[test]
    fn sdlc_flow_event_schema_deserializes_profile_name() {
        let json = serde_json::json!({
            "spec_slug": "x",
            "profile": "cheap-fast",
        });
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json).expect("deserializes with profile name");
        assert_eq!(event.profile, Some("cheap-fast".to_string()));
    }

    #[test]
    fn sdlc_flow_event_schema_profile_absent_is_none() {
        let json = serde_json::json!({ "spec_slug": "x" });
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json).expect("deserializes without profile");
        assert_eq!(event.profile, None);
    }

    #[test]
    fn sdlc_flow_event_schema_deserializes_policy_override() {
        let json = serde_json::json!({
            "spec_slug": "EN.3.C",
            "policy": { "output_verbosity": "terse", "max_attempts": 5 },
        });
        let event: SDLCFlowEventSchema =
            serde_json::from_value(json).expect("deserializes with policy override");
        let policy = event.policy.expect("policy override present");
        assert_eq!(
            policy.output_verbosity,
            Some(super::super::policy::OutputVerbosity::Terse)
        );
        assert_eq!(policy.max_attempts, Some(5));
    }

    #[test]
    fn sdlc_state_defaults_have_no_policy_or_outcomes() {
        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        assert_eq!(state.policy, None);
        assert_eq!(state.outcomes, None);
    }

    #[test]
    fn sdlc_state_round_trips_with_populated_policy_and_outcomes() {
        let mut state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        state.policy = Some(super::super::policy::SdlcPolicy::default());
        state.outcomes = Some(RunOutcomes {
            wall_clock_secs: 12.5,
            total_attempts: 3,
            total_retries: 1,
            tasks_passed: 2,
            tasks_failed: 0,
            review_verdicts: vec!["ConsolidatedReviewNode:PASS".to_string()],
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_cost_usd: 0.02,
            model_tier_used: BTreeMap::from([("implement".to_string(), "sonnet".to_string())]),
        });

        let json = serde_json::to_string(&state).expect("serializes");
        let round_tripped: SDLCState = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(state, round_tripped);
        assert_eq!(
            round_tripped.policy.unwrap(),
            super::super::policy::SdlcPolicy::default()
        );
        assert_eq!(round_tripped.outcomes.unwrap().total_cost_usd, 0.02);
    }

    #[test]
    fn sdlc_state_round_trips_through_serde_json() {
        let mut state = SDLCState::new("EN.3.A-sdlc-flow-setup-task-loop");
        state.phase_id = Some("EN.3".to_string());
        state.block_id = Some("EN.3.A".to_string());
        state
            .tasks
            .push(SDLCTask::new(1, "Task one", "Description one"));
        state.telemetry.total_attempts = 2;
        state.telemetry.budget_spent = 1.5;
        state.telemetry.tasks_passed = 1;

        let json = serde_json::to_string(&state).expect("serializes");
        let round_tripped: SDLCState = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(state, round_tripped);
    }

    #[test]
    fn parse_task_range_handles_ranges_and_singletons() {
        assert_eq!(
            parse_task_range(Some("1-3,5")).unwrap(),
            Some(vec![1, 2, 3, 5])
        );
        assert_eq!(parse_task_range(Some("5")).unwrap(), Some(vec![5]));
        assert_eq!(parse_task_range(Some("2,1")).unwrap(), Some(vec![1, 2]));
    }

    #[test]
    fn parse_task_range_none_means_all_tasks() {
        assert_eq!(parse_task_range(None).unwrap(), None);
    }

    #[test]
    fn parse_task_range_rejects_end_before_start() {
        let err = parse_task_range(Some("5-3")).unwrap_err();
        assert!(err.contains("end < start"));
    }

    /// EN.4.0 task 5 step 5.2 guard: converting a populated `RunOutcomes`
    /// into the generic `RunTelemetry` and back must round-trip exactly, and
    /// the two types must serialize to byte-identical JSON (same field
    /// names, same shape) — the proof that `RunOutcomes` staying flat
    /// (rather than `#[serde(flatten)]`-wrapped) doesn't diverge from the
    /// generic shape it's "expressed via".
    #[test]
    fn run_outcomes_round_trips_through_run_telemetry_byte_identically() {
        let outcomes = RunOutcomes {
            wall_clock_secs: 12.5,
            total_attempts: 3,
            total_retries: 1,
            tasks_passed: 2,
            tasks_failed: 0,
            review_verdicts: vec!["ConsolidatedReviewNode:PASS".to_string()],
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_cost_usd: 0.02,
            model_tier_used: BTreeMap::from([("implement".to_string(), "sonnet".to_string())]),
        };

        let telemetry: crate::policy::RunTelemetry = outcomes.clone().into();
        let round_tripped: RunOutcomes = telemetry.clone().into();
        assert_eq!(outcomes, round_tripped);

        assert_eq!(
            serde_json::to_string(&outcomes).unwrap(),
            serde_json::to_string(&telemetry).unwrap()
        );
    }

    // --- D31 committed-state adapter (EN.4.1) ------------------------------

    fn run_meta() -> RunMeta {
        RunMeta {
            branch: "sdlc/EN.4.1-fixture".to_string(),
            worktree_path: "trees/sdlc/EN.4.1-fixture".to_string(),
            started_at: "2026-07-25T00:00:00Z".to_string(),
            updated_at: "2026-07-25T00:10:00Z".to_string(),
            run_id: None,
        }
    }

    fn task(task_id: u32, status: SDLCTaskStatus) -> SDLCTask {
        let mut task = SDLCTask::new(task_id, format!("Task {task_id}"), "description");
        task.status = status;
        task
    }

    #[test]
    fn to_committed_state_json_reshapes_tasks_into_an_object_keyed_by_id() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::InProgress),
        ];

        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        let tasks = json["tasks"].as_object().expect("tasks is an object");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks["1"]["status"], serde_json::json!("done"));
        assert_eq!(tasks["2"]["status"], serde_json::json!("in_progress"));
    }

    #[test]
    fn to_committed_state_json_empty_tasks_round_trip_as_empty_object() {
        let state = SDLCState::new("EN.4.1-fixture");
        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        assert_eq!(json["tasks"], serde_json::json!({}));
        assert_eq!(json["status"], serde_json::json!("running"));
        assert_eq!(json["current_task"], serde_json::json!(0));
        assert_eq!(json["bail_reason"], serde_json::Value::Null);
        assert_eq!(json["review"], serde_json::Value::Null);
        assert_eq!(json["docs"], serde_json::Value::Null);
        assert_eq!(json["pr"], serde_json::Value::Null);
    }

    #[test]
    fn to_committed_state_json_carries_run_meta_fields() {
        let state = SDLCState::new("EN.4.1-fixture");
        let meta = run_meta();
        let json = state.to_committed_state_json(&meta, None, None, None, None, None);
        assert_eq!(json["spec_slug"], serde_json::json!("EN.4.1-fixture"));
        assert_eq!(json["branch"], serde_json::json!(meta.branch));
        assert_eq!(json["worktree_path"], serde_json::json!(meta.worktree_path));
        assert_eq!(json["started_at"], serde_json::json!(meta.started_at));
        assert_eq!(json["updated_at"], serde_json::json!(meta.updated_at));
    }

    #[test]
    fn to_committed_state_json_carries_a_some_run_id() {
        let state = SDLCState::new("EN.4.1-fixture");
        let mut meta = run_meta();
        meta.run_id = Some("11111111-2222-3333-4444-555555555555".to_string());
        let json = state.to_committed_state_json(&meta, None, None, None, None, None);
        assert_eq!(
            json["run_id"],
            serde_json::json!("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn to_committed_state_json_emits_null_run_id_when_absent() {
        let state = SDLCState::new("EN.4.1-fixture");
        let meta = run_meta();
        assert_eq!(meta.run_id, None);
        let json = state.to_committed_state_json(&meta, None, None, None, None, None);
        assert_eq!(json["run_id"], serde_json::Value::Null);
    }

    #[test]
    fn run_meta_run_id_round_trips_through_committed_json() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![task(1, SDLCTaskStatus::Done)];
        let mut meta = run_meta();
        meta.run_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string());

        let json = state.to_committed_state_json(&meta, None, None, None, None, None);
        let round_tripped =
            SDLCState::from_committed_state_json(&json).expect("round-trip should parse");

        assert_eq!(
            round_tripped.run_id,
            Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string())
        );
    }

    #[test]
    fn from_committed_state_json_no_run_id_key_reports_none() {
        // The exact shape base-template's JS `sdlc-flow.js` engine writes:
        // no `run_id` key at all.
        let value = serde_json::json!({
            "spec_slug": "js-engine-fixture",
            "branch": "sdlc/js-engine-fixture",
            "worktree_path": "trees/sdlc/js-engine-fixture",
            "started_at": "2026-07-25T00:00:00Z",
            "updated_at": "2026-07-25T00:10:00Z",
            "status": "running",
            "current_task": 1,
            "tasks": {},
        });

        let state = SDLCState::from_committed_state_json(&value)
            .expect("state with no run_id key should still parse");
        assert_eq!(state.run_id, None);
    }

    #[test]
    fn to_committed_state_json_includes_review_docs_pr_when_supplied() {
        let state = SDLCState::new("EN.4.1-fixture");
        let review = CommittedReview {
            verdict: "PASS".to_string(),
            findings: vec![],
            attempts: 1,
        };
        let docs = CommittedDocs {
            changed: vec!["docs/architecture.md".to_string()],
            created: vec![],
        };
        let pr = CommittedPr {
            url: "https://github.com/bredmond1019/engine-rs/pull/1".to_string(),
            number: 1,
        };

        let json = state.to_committed_state_json(
            &run_meta(),
            Some(&review),
            Some(&docs),
            Some(&pr),
            None,
            None,
        );
        assert_eq!(json["review"]["verdict"], serde_json::json!("PASS"));
        assert_eq!(json["review"]["attempts"], serde_json::json!(1));
        assert_eq!(
            json["docs"]["changed"],
            serde_json::json!(["docs/architecture.md"])
        );
        assert_eq!(json["docs"]["created"], serde_json::json!([]));
        assert_eq!(json["pr"]["number"], serde_json::json!(1));
    }

    // -- EN.3.E task 3: `final_validation` committed-state key -------------

    fn sample_final_validation(all_passed: bool) -> CommittedFinalValidation {
        let json = serde_json::json!({
            "all_passed": all_passed,
            "failure_summary": if all_passed { "" } else { "Failed checks: build" },
            "check_results": [
                { "name": "build", "kind": "command", "passed": all_passed },
            ],
        });
        serde_json::from_value(json).expect("CommittedFinalValidation shape parses")
    }

    #[test]
    fn to_committed_state_json_carries_a_some_final_validation() {
        let state = SDLCState::new("EN.3.E-fixture");
        let final_validation = sample_final_validation(true);

        let json = state.to_committed_state_json(
            &run_meta(),
            None,
            None,
            None,
            Some(&final_validation),
            None,
        );

        assert_eq!(
            json["final_validation"]["all_passed"],
            serde_json::json!(true)
        );
        assert_eq!(
            json["final_validation"]["failure_summary"],
            serde_json::json!("")
        );
        assert_eq!(
            json["final_validation"]["check_results"][0]["name"],
            serde_json::json!("build")
        );
    }

    #[test]
    fn to_committed_state_json_emits_null_final_validation_when_absent() {
        let state = SDLCState::new("EN.3.E-fixture");
        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        assert!(json["final_validation"].is_null());
    }

    #[test]
    fn final_validation_round_trips_through_from_committed_state_json() {
        let state = SDLCState::new("EN.3.E-fixture");
        let final_validation = sample_final_validation(false);

        let json = state.to_committed_state_json(
            &run_meta(),
            None,
            None,
            None,
            Some(&final_validation),
            None,
        );

        // `from_committed_state_json` must not choke on the new key — it
        // does not reconstruct `final_validation` onto `SDLCState` (it is a
        // transient per-write param, same as `review`/`docs`/`pr`), but the
        // written JSON itself preserves the full shape, which is what
        // `wrap_up::committed_final_validation` reads back on a resumed
        // failure-path write.
        let round_tripped =
            SDLCState::from_committed_state_json(&json).expect("parses with final_validation set");
        assert_eq!(round_tripped.spec_slug, "EN.3.E-fixture");
        assert_eq!(
            json["final_validation"]["all_passed"],
            serde_json::json!(false)
        );
        assert_eq!(
            json["final_validation"]["failure_summary"],
            serde_json::json!("Failed checks: build")
        );
    }

    #[test]
    fn from_committed_state_json_missing_final_validation_key_still_parses() {
        // Base-template's JS `sdlc-flow.js` engine's shape: no
        // `final_validation` key at all (it never emits one).
        let value = serde_json::json!({
            "spec_slug": "js-engine-fixture",
            "branch": "sdlc/js-engine-fixture",
            "worktree_path": "trees/sdlc/js-engine-fixture",
            "started_at": "2026-07-25T00:00:00Z",
            "updated_at": "2026-07-25T00:10:00Z",
            "status": "running",
            "current_task": 1,
            "tasks": {},
        });

        let state = SDLCState::from_committed_state_json(&value)
            .expect("state with no final_validation key should still parse");
        assert_eq!(state.spec_slug, "js-engine-fixture");
    }

    #[test]
    fn to_committed_state_json_includes_additive_top_level_keys() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.phase_id = Some("EN.4".to_string());
        state.block_id = Some("EN.4.1".to_string());
        state.telemetry.total_attempts = 2;
        state.policy = Some(SdlcPolicy::default());
        state.outcomes = Some(RunOutcomes::default());

        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        assert_eq!(json["phase_id"], serde_json::json!("EN.4"));
        assert_eq!(json["block_id"], serde_json::json!("EN.4.1"));
        assert_eq!(json["telemetry"]["total_attempts"], serde_json::json!(2));
        assert!(json["policy"].is_object());
        assert!(json["outcomes"].is_object());
    }

    #[test]
    fn committed_state_json_round_trips_with_empty_tasks() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        let meta = run_meta();
        state.started_at = Some(meta.started_at.clone());
        let json = state.to_committed_state_json(&meta, None, None, None, None, None);

        let round_tripped = SDLCState::from_committed_state_json(&json).expect("parses");
        assert_eq!(round_tripped.spec_slug, "EN.4.1-fixture");
        assert_eq!(round_tripped.tasks, Vec::<SDLCTask>::new());
        assert_eq!(round_tripped.branch, Some(meta.branch));
        assert_eq!(round_tripped.worktree_path, Some(meta.worktree_path));
        assert_eq!(round_tripped.started_at, Some(meta.started_at));
        assert_eq!(round_tripped.updated_at, Some(meta.updated_at));
    }

    #[test]
    fn committed_state_json_round_trips_with_mixed_task_states() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::Failed),
            task(3, SDLCTaskStatus::Pending),
        ];
        state.telemetry.total_attempts = 5;
        state.telemetry.tasks_passed = 1;
        state.telemetry.tasks_failed = 1;

        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        let round_tripped = SDLCState::from_committed_state_json(&json).expect("parses");

        assert_eq!(round_tripped.tasks.len(), 3);
        assert_eq!(round_tripped.tasks[0].task_id, 1);
        assert_eq!(round_tripped.tasks[0].status, SDLCTaskStatus::Done);
        assert_eq!(round_tripped.tasks[1].task_id, 2);
        assert_eq!(round_tripped.tasks[1].status, SDLCTaskStatus::Failed);
        assert_eq!(round_tripped.tasks[2].task_id, 3);
        assert_eq!(round_tripped.tasks[2].status, SDLCTaskStatus::Pending);
        assert_eq!(round_tripped.telemetry.total_attempts, 5);
    }

    #[test]
    fn committed_state_json_round_trips_out_of_order_task_keys() {
        // Build the JSON directly (not via `to_committed_state_json`) with
        // task keys inserted out of numeric order, to prove the numeric
        // (not insertion-order or lexical-string) sort in
        // `from_committed_state_json`.
        let json = serde_json::json!({
            "spec_slug": "EN.4.1-fixture",
            "branch": "b",
            "worktree_path": "w",
            "started_at": "s",
            "updated_at": "u",
            "status": "running",
            "current_task": 2,
            "tasks": {
                "10": { "task_id": 10, "title": "Ten", "description": "d" },
                "2": { "task_id": 2, "title": "Two", "description": "d" },
            },
            "bail_reason": null,
        });

        let round_tripped = SDLCState::from_committed_state_json(&json).expect("parses");
        assert_eq!(
            round_tripped
                .tasks
                .iter()
                .map(|t| t.task_id)
                .collect::<Vec<_>>(),
            vec![2, 10]
        );
    }

    #[test]
    fn committed_state_json_round_trips_with_policy_outcomes_telemetry() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.telemetry.total_attempts = 3;
        state.telemetry.budget_spent = 1.25;
        state.policy = Some(SdlcPolicy::default());
        state.outcomes = Some(RunOutcomes {
            wall_clock_secs: 10.0,
            ..RunOutcomes::default()
        });

        let json = state.to_committed_state_json(&run_meta(), None, None, None, None, None);
        let round_tripped = SDLCState::from_committed_state_json(&json).expect("parses");

        assert_eq!(round_tripped.telemetry.total_attempts, 3);
        assert_eq!(round_tripped.telemetry.budget_spent, 1.25);
        assert_eq!(round_tripped.policy, Some(SdlcPolicy::default()));
        assert_eq!(
            round_tripped.outcomes.map(|o| o.wall_clock_secs),
            Some(10.0)
        );
    }

    #[test]
    fn from_committed_state_json_missing_spec_slug_errors() {
        let json = serde_json::json!({ "status": "running" });
        let err = SDLCState::from_committed_state_json(&json).unwrap_err();
        assert!(err.to_string().contains("spec_slug"));
    }

    #[test]
    fn derive_committed_status_blocked_for_every_terminal_signal_variant() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![task(1, SDLCTaskStatus::InProgress)];

        for signal in [
            TerminalSignal::MajorBail("max attempts reached".to_string()),
            TerminalSignal::ReviewFail("review FAIL, 0 issues".to_string()),
            TerminalSignal::StructuralFail("review PARTIAL, 12 issues".to_string()),
            TerminalSignal::FinalValidationFailed("cargo clippy".to_string()),
            TerminalSignal::EndReviewFail("criterion X not met".to_string()),
        ] {
            assert_eq!(derive_committed_status(&state, Some(&signal)), "blocked");
        }

        // Blocked even if every task happens to already be terminal — a
        // terminal signal always wins over task-state.
        state.tasks = vec![task(1, SDLCTaskStatus::Done)];
        let signal = TerminalSignal::MajorBail("bail".to_string());
        assert_eq!(derive_committed_status(&state, Some(&signal)), "blocked");
    }

    #[test]
    fn derive_committed_status_done_when_all_tasks_terminal_and_no_signal() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::Skipped),
        ];
        assert_eq!(derive_committed_status(&state, None), "done");
    }

    #[test]
    fn derive_committed_status_running_when_any_task_pending_and_no_signal() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::Pending),
        ];
        assert_eq!(derive_committed_status(&state, None), "running");
    }

    #[test]
    fn derive_committed_status_running_when_no_tasks_and_no_signal() {
        let state = SDLCState::new("EN.4.1-fixture");
        assert_eq!(derive_committed_status(&state, None), "running");
    }

    #[test]
    fn derive_current_task_empty_is_zero() {
        assert_eq!(derive_current_task(&[]), 0);
    }

    #[test]
    fn derive_current_task_all_pending_is_lowest_id() {
        let tasks = vec![
            task(3, SDLCTaskStatus::Pending),
            task(1, SDLCTaskStatus::Pending),
            task(2, SDLCTaskStatus::Pending),
        ];
        assert_eq!(derive_current_task(&tasks), 1);
    }

    #[test]
    fn derive_current_task_mixed_is_lowest_non_terminal_id() {
        let tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::Failed),
            task(3, SDLCTaskStatus::Pending),
        ];
        assert_eq!(derive_current_task(&tasks), 2);
    }

    #[test]
    fn derive_current_task_all_done_is_last_id() {
        let tasks = vec![
            task(1, SDLCTaskStatus::Done),
            task(2, SDLCTaskStatus::Skipped),
            task(3, SDLCTaskStatus::Done),
        ];
        assert_eq!(derive_current_task(&tasks), 3);
    }

    #[test]
    fn derive_bail_reason_per_variant() {
        assert_eq!(
            derive_bail_reason(Some(&TerminalSignal::MajorBail("m".to_string()))),
            Some("m".to_string())
        );
        assert_eq!(
            derive_bail_reason(Some(&TerminalSignal::ReviewFail("r".to_string()))),
            Some("r".to_string())
        );
        assert_eq!(
            derive_bail_reason(Some(&TerminalSignal::StructuralFail("s".to_string()))),
            Some("s".to_string())
        );
        assert_eq!(
            derive_bail_reason(Some(&TerminalSignal::EndReviewFail("e".to_string()))),
            Some("e".to_string())
        );
    }

    #[test]
    fn derive_bail_reason_final_validation_failed_matches_gate_note_wording() {
        assert_eq!(
            derive_bail_reason(Some(&TerminalSignal::FinalValidationFailed(
                "cargo clippy, cargo nextest".to_string()
            ))),
            Some("Final validation gate FAILED: cargo clippy, cargo nextest".to_string())
        );
    }

    #[test]
    fn derive_committed_status_blocked_for_final_validation_failed() {
        let mut state = SDLCState::new("EN.4.1-fixture");
        state.tasks = vec![task(1, SDLCTaskStatus::Done)];
        let signal = TerminalSignal::FinalValidationFailed("cargo build --release".to_string());
        assert_eq!(derive_committed_status(&state, Some(&signal)), "blocked");
    }

    #[test]
    fn derive_bail_reason_none_for_happy_path() {
        assert_eq!(derive_bail_reason(None), None);
    }

    #[test]
    fn to_committed_state_json_matches_golden_fixture() {
        let mut state = SDLCState::new("EN.4.1-fixture-golden");
        state.phase_id = Some("EN.4".to_string());
        state.block_id = Some("EN.4.1".to_string());
        let mut t1 = task(1, SDLCTaskStatus::Done);
        t1.attempt_count = 1;
        let mut t2 = task(2, SDLCTaskStatus::Done);
        t2.attempt_count = 2;
        state.tasks = vec![t1, t2];
        state.telemetry = SDLCTelemetry {
            total_attempts: 3,
            budget_spent: 0.05,
            tasks_passed: 2,
            tasks_failed: 0,
            review_attempts: 1,
        };
        state.policy = Some(SdlcPolicy::default());
        state.outcomes = Some(RunOutcomes {
            wall_clock_secs: 42.0,
            total_attempts: 3,
            total_retries: 1,
            tasks_passed: 2,
            tasks_failed: 0,
            review_verdicts: vec!["ConsolidatedReviewNode:PASS".to_string()],
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_cost_usd: 0.02,
            model_tier_used: BTreeMap::from([("implement".to_string(), "sonnet".to_string())]),
        });

        let meta = RunMeta {
            branch: "sdlc/EN.4.1-fixture-golden".to_string(),
            worktree_path: "trees/sdlc/EN.4.1-fixture-golden".to_string(),
            started_at: "2026-07-25T00:00:00Z".to_string(),
            updated_at: "2026-07-25T00:10:00Z".to_string(),
            run_id: None,
        };
        let review = CommittedReview {
            verdict: "PASS".to_string(),
            findings: vec![],
            attempts: 1,
        };
        let docs = CommittedDocs {
            changed: vec!["docs/architecture.md".to_string()],
            created: vec![],
        };
        let pr = CommittedPr {
            url: "https://github.com/bredmond1019/engine-rs/pull/1".to_string(),
            number: 1,
        };

        let actual =
            state.to_committed_state_json(&meta, Some(&review), Some(&docs), Some(&pr), None, None);
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/committed_state_expected.json"))
                .expect("golden fixture parses as JSON");

        // Additive-tolerant: the fixture embeds a full `SdlcPolicy::default()`
        // snapshot under `"policy"`, so strict equality made every new policy
        // knob require a fixture edit. Subset comparison keeps every value the
        // fixture pins (and still fails on a missing field or a changed list),
        // while a newly-added knob is simply ignored.
        assert_json_subset(&expected, &actual);
    }
}
