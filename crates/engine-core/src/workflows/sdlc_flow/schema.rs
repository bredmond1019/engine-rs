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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
