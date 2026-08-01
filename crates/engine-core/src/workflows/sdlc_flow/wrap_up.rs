//! `WrapUpNode` — deterministic wrap-up template render (bottom-half, EN.3.B).
//!
//! Ported from `orchestrator/app/workflows/sdlc_flow_workflow_nodes/wrap_up_node.py`:
//! a deterministic `Node` (no model call) that reads the latest durable
//! `SDLCState`, computes a `PASS`/`PARTIAL/FAIL` outcome from its telemetry,
//! and renders three text artifacts — `log_entry`, `report`,
//! `status_suggestion` — from Rust string templates. This node is a
//! MAJOR_BAIL / structural-fail terminal target of `TriageRouterNode` and
//! `ReviewRouterNode` as well as the natural end of a fully-passing run, so
//! it must tolerate being reached with any telemetry shape.
//!
//! Renders its three text artifacts for a later step/human (mirrors the
//! Python node), and additionally persists the resolved-policy + outcomes
//! snapshot it stamps into `SDLCState` to the on-disk
//! `planning/{spec_slug}/sdlc-flow-state.json` (EN.3.C task 6 fix) — the
//! same file `task_loop::SaveStateNode` writes, since `SaveStateNode` never
//! runs again after `WrapUpNode` in the assembled graph.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::policy::telemetry::RunTelemetryInputs;
#[cfg(test)]
use crate::policy::RESOLVED_POLICY_IDENTITY;

use super::policy::SdlcPolicy;
use super::schema::{
    CommittedDocs, CommittedFinalValidation, CommittedPr, CommittedReview, RunMeta, RunOutcomes,
    SDLCState, TerminalSignal,
};
use super::task_loop::{latest_state, STRUCTURAL_ISSUE_THRESHOLD};
use super::{commit_all, get_result, put_result, CommandRunner};

/// Injectable "today" clock seam so the rendered date is deterministic
/// under test. Defaults to the real current UTC date
/// (`YYYY-MM-DD`); tests substitute a fixed date.
pub type ClockFn = Arc<dyn Fn() -> String + Send + Sync>;

/// The default [`ClockFn`]: today's real date in `YYYY-MM-DD` (UTC),
/// computed without pulling in a chrono dependency — days-since-epoch civil
/// conversion (Howard Hinnant's algorithm).
#[must_use]
pub fn default_clock() -> ClockFn {
    Arc::new(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let days = now.as_secs() as i64 / 86_400;
        civil_from_days(days)
    })
}

/// Convert a days-since-1970-01-01 count into a `YYYY-MM-DD` string.
/// Standard proleptic-Gregorian civil-from-days algorithm.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Read the worktree path stamped by `SetupWorktreeNode`, if the run went
/// through it. Absent in unit tests that drive `WrapUpNode` directly (no
/// `SetupWorktreeNode` in `ctx`) — those skip the on-disk persist below,
/// mirroring how `task_loop.rs`'s node tests operate without a worktree.
fn worktree_path(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("worktree_path"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Read the `branch_name` stamped by `SetupWorktreeNode`, if present.
/// Mirrors [`worktree_path`]/`task_loop::branch_name`.
fn branch_name(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("branch_name"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Read `started_at` back out of an already-committed D31 state file at
/// `state_path`, if one exists and parses — preserves the run's original
/// start time when `WrapUpNode` is the *second* (or later) write to this
/// file (`task_loop::SaveStateNode` already wrote at least once during the
/// task loop for any run with at least one task). Not hoisted into a shared
/// helper with `task_loop::existing_started_at` (a byte-identical copy):
/// unlike `latest_state` (EN.3.G task 2 unified that one because the two
/// copies had silently diverged), this pair of functions is still
/// byte-identical today, so there is no correctness reason to hoist it —
/// only do so if a future edit makes them diverge in behavior, not just in
/// location.
fn existing_started_at(state_path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(state_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    SDLCState::from_committed_state_json(&value)
        .ok()?
        .started_at
}

/// Build this write's [`RunMeta`]: `branch`/`worktree_path` from
/// `SetupWorktreeNode`'s output, `started_at` preserved from an existing
/// on-disk committed file if present (else stamped fresh — a run with zero
/// tasks never went through `SaveStateNode`), `updated_at` always fresh, and
/// `run_id` read back out of `ctx.metadata` via [`crate::read_run_id`] — the
/// stamp `Workflow::run_with`/`run_from` write before the walk starts
/// (`None` when the run carried no `RunOptions::run_id`, e.g. any run driven
/// through base-template's JS engine or a unit test with empty metadata).
fn build_run_meta(ctx: &TaskContext, worktree: &str, state_path: &std::path::Path) -> RunMeta {
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

/// Reshape `ConsolidatedReviewNode`'s last output (if the run reached
/// review at least once) into the D31 committed-state `review` block.
/// `attempts` is stamped `1`: this engine's `ctx` retains only the *latest*
/// `ConsolidatedReviewNode` result (see [`latest_state`]'s "most recently
/// mutated" pattern used throughout this workflow), not a running count of
/// how many times review itself was attempted, so there is no real
/// per-review attempt counter to report here today — `1` reflects "review
/// ran and this is its outcome" rather than a meaningful retry count.
fn committed_review(ctx: &TaskContext) -> Option<CommittedReview> {
    let review = get_result(ctx, "ConsolidatedReviewNode")?;
    let verdict = review.get("verdict")?.as_str()?.to_string();
    let findings = review
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(CommittedReview {
        verdict,
        findings,
        attempts: 1,
    })
}

/// Reshape `PatchDocsNode`'s output (if the run reached docs-patching) into
/// the D31 committed-state `docs` block. `PatchDocsNode` only ever patches
/// existing docs (its output is `files_patched`, no notion of new files), so
/// `created` is always empty — see `schema::CommittedDocs`'s doc comment.
fn committed_docs(ctx: &TaskContext) -> Option<CommittedDocs> {
    let docs = get_result(ctx, "PatchDocsNode")?;
    let changed = docs
        .get("files_patched")
        .and_then(|value| value.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(CommittedDocs {
        changed,
        created: Vec::new(),
    })
}

/// The D31 committed-state `pr` block is always `None` from this call site:
/// the declared graph runs `WrapUpNode -> PullRequestNode` (see
/// `graph.rs`'s schema), so `PullRequestNode` has not run — and cannot have
/// a result in `ctx` — by the time `WrapUpNode::process` executes. `graph.rs`
/// is NOT reordered to fix this (the PR must follow docs and wrap-up); the
/// `pr` block is instead patched into the already-written state file by
/// `EmitStateNode` (EN.3.G task 6), which runs last in the declared graph
/// and already holds the `CommandRunner` seam needed to read/rewrite/commit
/// the file — see `emit_state::patch_pr_into_state`. This function keeps
/// returning `None`: `WrapUpNode` genuinely still cannot see the PR result
/// at the time it runs. See `D10-committed-state-path-schema-alignment.md`'s
/// Consequences section.
fn committed_pr(_ctx: &TaskContext) -> Option<CommittedPr> {
    None
}

/// Reshape `FinalValidationNode`'s output (if the run reached the drain
/// branch's full-suite gate) into the D31 committed-state `final_validation`
/// block (EN.3.E task 3). The stamped result already carries exactly
/// `{all_passed, check_results, failure_summary}` — the same shape
/// [`CommittedFinalValidation`] declares — so a direct `serde_json`
/// deserialize is all that's needed; no field-by-field reshaping like
/// [`committed_review`]/[`committed_docs`] requires. `None` when
/// `FinalValidationNode` never ran this walk (a bailed run routes straight
/// to `WrapUpNode` from a router and never reaches the drain branch) or if
/// its stamped result somehow fails to parse as [`CommittedFinalValidation`].
fn committed_final_validation(ctx: &TaskContext) -> Option<CommittedFinalValidation> {
    let result = get_result(ctx, "FinalValidationNode")?;
    serde_json::from_value(result.clone()).ok()
}

/// Derive this run's [`TerminalSignal`], if any, by inspecting the two
/// model-judgment stages that can route straight to `WrapUpNode` rather
/// than back through a retry loop: `TriageTaskNode`'s `MAJOR_BAIL` verdict,
/// and `ConsolidatedReviewNode`'s `FAIL`/`PARTIAL` verdict when
/// `ReviewRouterNode` classified the issue count as structural (`0`, or
/// over `task_loop::STRUCTURAL_ISSUE_THRESHOLD` — the exact same gate
/// `ReviewRouterNode::route` uses, so this can never disagree with the
/// router's own decision about *whether* this was a structural stop).
/// `None` on the happy path (every task passed cleanly, or `ctx` has
/// neither stage's output — e.g. a unit test driving `WrapUpNode` directly).
///
/// An explicit `MAJOR_BAIL` / structural review fail is checked first and
/// wins over an `unrecognized_verdict` where both are somehow present, so a
/// real bail signal is never masked by the router-fallback bookkeeping.
fn derive_terminal_signal(ctx: &TaskContext) -> Option<TerminalSignal> {
    if let Some(triage) = get_result(ctx, "TriageTaskNode") {
        if triage.get("verdict").and_then(|v| v.as_str()) == Some("MAJOR_BAIL") {
            let reason = triage
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("Max attempts reached without a passing run.")
                .to_string();
            return Some(TerminalSignal::MajorBail(reason));
        }
    }

    if let Some(review) = get_result(ctx, "ConsolidatedReviewNode") {
        let verdict = review.get("verdict").and_then(|v| v.as_str());
        let issue_count = review
            .get("issues")
            .and_then(|v| v.as_array())
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        let structural = issue_count == 0 || issue_count > STRUCTURAL_ISSUE_THRESHOLD;
        if structural {
            let summary = review
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match verdict {
                Some("FAIL") => return Some(TerminalSignal::ReviewFail(summary)),
                Some("PARTIAL") => return Some(TerminalSignal::StructuralFail(summary)),
                _ => {}
            }
        }
    }

    // Router-fallback safety net (EN.3.G task 1): an unrecognized verdict
    // from either model-judgment stage routes straight to `WrapUpNode`
    // rather than halting the walk (see `TriageRouterNode::route` /
    // `ReviewRouterNode::route`'s catch-all arms). Surface the exact string
    // the model produced in the terminal `bail_reason` so it is diagnosable
    // rather than silently swallowed as a `done` run.
    if let Some(triage) = get_result(ctx, "TriageTaskNode") {
        if let Some(verdict) = triage.get("unrecognized_verdict").and_then(|v| v.as_str()) {
            return Some(TerminalSignal::MajorBail(format!(
                "unrecognized triage verdict: {verdict}"
            )));
        }
    }
    if let Some(review) = get_result(ctx, "ConsolidatedReviewNode") {
        if let Some(verdict) = review.get("unrecognized_verdict").and_then(|v| v.as_str()) {
            return Some(TerminalSignal::MajorBail(format!(
                "unrecognized review verdict: {verdict}"
            )));
        }
    }

    None
}

/// `planning/{spec_slug}/sdlc/sdlc-flow-state.json` inside `worktree` — the
/// D31-committed path (see `D10-committed-state-path-schema-alignment.md`)
/// `task_loop::SaveStateNode` also writes to. Creates the `sdlc/`
/// intermediate directory if needed.
fn state_path_for(worktree: &str, spec_slug: &str) -> Result<std::path::PathBuf, NodeError> {
    let state_dir = std::path::Path::new(worktree)
        .join("planning")
        .join(spec_slug)
        .join("sdlc");
    std::fs::create_dir_all(&state_dir).map_err(|err| {
        NodeError::new(format!("failed to create {}: {err}", state_dir.display()))
    })?;
    Ok(state_dir.join("sdlc-flow-state.json"))
}

/// Persist `state` (already stamped with `policy`/`outcomes`) to
/// `state_path` in the D31-committed JSON shape. This is the only point in
/// the run where the resolved policy + outcome summary, and (when present)
/// `review`/`docs`/a derived `bail_reason`, reach the durable on-disk state
/// file: `SaveStateNode` runs earlier in the graph (inside the task loop)
/// and never re-runs after `WrapUpNode`, so without this write those blocks
/// are computed here and then discarded when the run ends.
#[allow(clippy::too_many_arguments)]
fn persist_state(
    state_path: &std::path::Path,
    state: &SDLCState,
    run_meta: &RunMeta,
    review: Option<&CommittedReview>,
    docs: Option<&CommittedDocs>,
    pr: Option<&CommittedPr>,
    final_validation: Option<&CommittedFinalValidation>,
    terminal_signal: Option<&TerminalSignal>,
) -> Result<String, NodeError> {
    let committed = state.to_committed_state_json(
        run_meta,
        review,
        docs,
        pr,
        final_validation,
        terminal_signal,
    );
    let json = serde_json::to_string_pretty(&committed)
        .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;
    std::fs::write(state_path, json).map_err(|err| {
        NodeError::new(format!("failed to write {}: {err}", state_path.display()))
    })?;
    Ok(state_path.to_string_lossy().to_string())
}

/// Best-effort failure-path terminal writer (EN.6.J task 4).
///
/// A node returning `Err` halts the workflow walk before `WrapUpNode` ever
/// runs — that node's own `Err` short-circuits `Workflow::run_with`/
/// `run_from` before the graph reaches `WrapUpNode`, so nothing normally
/// writes a terminal status for that run and its
/// `planning/{spec_slug}/sdlc/sdlc-flow-state.json` rots at whatever
/// non-terminal `"running"` status the last `SaveStateNode` write left it
/// at. This function is the failure-path call site: `engine-serve`'s
/// post-walk cleanup calls it directly over the returned (failed) `ctx`,
/// outside of any node, once a walk is known to have errored out.
///
/// Resolves the worktree and the latest `SDLCState` from `ctx` exactly as
/// [`WrapUpNode::process`] does, then persists a `MajorBail(reason)`
/// terminal signal through the same [`persist_state`] seam
/// [`WrapUpNode`] uses — the on-disk file ends up with `status: "blocked"`
/// (via `schema::derive_committed_status`) and `bail_reason: reason` (via
/// `schema::derive_bail_reason`), and carries this run's `run_id` if `ctx`
/// carries one.
///
/// Returns `None` (writes nothing, never panics, never surfaces an error to
/// the caller) when:
/// - `ctx` has no `SetupWorktreeNode` stamp (not an SDLC flow run that ever
///   set up a worktree — e.g. a bare unit-test `ctx`, or a different
///   workflow entirely);
/// - `ctx` has no loaded `SDLCState` (neither `UpdateTaskStatusNode` nor
///   `LoadTaskStateNode` ran before the failure);
/// - the state file's parent directory does not already exist on disk —
///   unlike [`state_path_for`] (which `WrapUpNode`'s happy path uses and
///   which creates that directory), this failure-path writer never creates
///   directories: a state directory that isn't already there means no
///   `SaveStateNode`/`LoadTaskStateNode` ever ran for this run against this
///   worktree, so there is nothing meaningful to terminate.
///
/// Writes the file only — it does NOT git-commit (see this spec's
/// `tasks.md` Notes for the rationale: this runs from `engine-serve`'s
/// post-walk cleanup, outside any node, where there is no `CommandRunner`
/// seam).
#[must_use]
pub fn write_terminal_blocked_state(ctx: &TaskContext, reason: &str) -> Option<String> {
    let worktree = worktree_path(ctx)?;
    let state = latest_state(ctx).ok()?;
    let spec_slug = state.spec_slug.clone();

    let state_dir = std::path::Path::new(&worktree)
        .join("planning")
        .join(&spec_slug)
        .join("sdlc");
    if !state_dir.is_dir() {
        return None;
    }
    let state_path = state_dir.join("sdlc-flow-state.json");

    let run_meta = build_run_meta(ctx, &worktree, &state_path);
    let review = committed_review(ctx);
    let docs = committed_docs(ctx);
    let pr = committed_pr(ctx);
    let final_validation = committed_final_validation(ctx);
    let terminal_signal = TerminalSignal::MajorBail(reason.to_string());

    persist_state(
        &state_path,
        &state,
        &run_meta,
        review.as_ref(),
        docs.as_ref(),
        pr.as_ref(),
        final_validation.as_ref(),
        Some(&terminal_signal),
    )
    .ok()
}

/// Read the resolved `SdlcPolicy` stamped by dispatch or by
/// `SetupWorktreeNode` (`setup::RESOLVED_POLICY_IDENTITY`). Fails loudly —
/// `Err` — when the stamp is absent or unparsable, rather than silently
/// falling back to a built-in default (task 8): a ctx driven directly in a
/// unit test must now seed a policy explicitly. Delegates to the generic
/// `crate::policy::resolved_policy_strict::<SdlcPolicy>` (EN.4.0/EN.5.D).
fn resolved_policy(ctx: &TaskContext) -> Result<SdlcPolicy, NodeError> {
    crate::policy::resolved_policy_strict::<SdlcPolicy>(ctx)
}

/// Sum of every task's attempts beyond its first — total retries triggered
/// by `RETRYABLE`/minor-`FAIL` back-edges.
fn total_retries(state: &SDLCState) -> u32 {
    state
        .tasks
        .iter()
        .map(|task| task.attempt_count.saturating_sub(1))
        .sum()
}

/// The verdict-bearing model-judgment stages this snapshot inspects, in
/// declared run order. Passed to the generic
/// `crate::policy::telemetry::harvest` as `RunTelemetryInputs::verdict_stages`
/// (EN.4.0).
const VERDICT_STAGES: [&str; 2] = ["TriageTaskNode", "ConsolidatedReviewNode"];

/// The model-node identities whose `ctx.nodes` output may carry a
/// `"cost_usd"` field (`ClaudeCodeStep`'s output shape). Passed to the
/// generic `crate::policy::telemetry::harvest` as
/// `RunTelemetryInputs::cost_bearing_stages` (EN.4.0).
const COST_BEARING_STAGES: [&str; 4] = [
    "ImplementTaskNode",
    "TriageTaskNode",
    "ConsolidatedReviewNode",
    "GenerateTasksNode",
];

/// The resolved policy's per-stage tier, keyed by `ModelTiers` field name.
///
/// **What "used" means, per stage.** `triage` and `review` are the two
/// stages `registry_for_policy` can rewire by tier (a `local` assignment
/// swaps their transport; a local-endpoint failure falls back to cloud
/// per-call without changing this snapshot). Every other stage consumes its
/// tier through `task_loop::apply_policy`/`apply_policy_config`, which sets
/// `Config.model` from it directly — `implement` (`ImplementTaskNode`) and
/// `docs` (`PatchDocsNode`).
///
/// Two entries here are still reported without a consumer, and are listed
/// deliberately rather than hidden:
///   * `generate` — `GenerateTasksNode` has not been onboarded to
///     `apply_policy` yet, so it runs `ModelTiers::default().generate`'s
///     model string by hardcode rather than by resolution. The default was
///     corrected to `Opus` so this line is at least TRUE today; it becomes
///     load-bearing when that node is onboarded.
///   * `implement_simple` — belongs to a simple-task path that does not
///     exist yet; nothing reads it.
///
/// (The previous version of this comment claimed `registry_for_policy` wires
/// EVERY stage's transport from this assignment. It never did.)
fn model_tier_used(policy: &SdlcPolicy) -> BTreeMap<String, String> {
    let tiers = &policy.model_tiers;
    let tier_str = |tier: super::policy::ModelTier| {
        serde_json::to_value(tier)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    BTreeMap::from([
        ("implement".to_string(), tier_str(tiers.implement)),
        (
            "implement_simple".to_string(),
            tier_str(tiers.implement_simple),
        ),
        ("triage".to_string(), tier_str(tiers.triage)),
        ("review".to_string(), tier_str(tiers.review)),
        ("generate".to_string(), tier_str(tiers.generate)),
        ("docs".to_string(), tier_str(tiers.docs)),
    ])
}

/// Finalize the resolved-policy snapshot + outcome-metrics block for a
/// completed (or bailed) run (EN.3.C task 6): deterministic — reads only
/// `ctx`'s already-accumulated `SDLCState`/`node_runs`/`nodes`, spends no
/// model tokens, and stays a pure function of `ctx` + `now` so tests can
/// pin the clock. Delegates the ctx-derived fields (wall-clock, tokens,
/// cost, verdicts) to the generic `crate::policy::telemetry::harvest`
/// (EN.4.0), supplying the SDLC-specific `total_attempts`/`total_retries`/
/// `tasks_passed`/`tasks_failed` (state-derived) and `model_tier_used`
/// (policy-derived) via `RunTelemetryInputs`.
fn finalize_outcomes(
    ctx: &TaskContext,
    state: &SDLCState,
    policy: &SdlcPolicy,
    now: chrono::DateTime<Utc>,
) -> RunOutcomes {
    let inputs = RunTelemetryInputs {
        start_node_identity: "SetupWorktreeNode",
        verdict_stages: &VERDICT_STAGES,
        cost_bearing_stages: &COST_BEARING_STAGES,
        total_attempts: state.telemetry.total_attempts,
        total_retries: total_retries(state),
        tasks_passed: state.telemetry.tasks_passed,
        tasks_failed: state.telemetry.tasks_failed,
        model_tier_used: model_tier_used(policy),
        model_stages: &COST_BEARING_STAGES,
    };
    crate::policy::harvest_telemetry(ctx, now, inputs).into()
}

/// Deterministic node: renders the wrap-up artifacts for a completed (or
/// bailed) SDLC flow run, and (EN.3.G task 3) git-commits its terminal state
/// write through the same [`super::commit_all`] seam
/// `task_loop::SaveStateNode` uses — without this, the final `done`/
/// `blocked` state landed on disk but was never committed, so a
/// `--worktree` run's PR did not contain it.
///
/// Since that seam widened to `git add -A`, this commit is also the one that
/// lands `PatchDocsNode`'s doc edits (and, on a MAJOR_BAIL, the bailed task's
/// partial work) on the branch before `PullRequestNode` pushes — see the
/// comment at the `commit_all` call site.
pub struct WrapUpNode {
    clock: ClockFn,
    runner: CommandRunner,
}

impl WrapUpNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: default_clock(),
            runner: super::default_command_runner(),
        }
    }

    /// Override the "today" clock. Tests use this to render against a fixed
    /// date.
    #[must_use]
    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }

    /// Override the command runner used for the terminal state's
    /// git add/commit invocation. Tests use this to stub the subprocess —
    /// mirrors `task_loop::SaveStateNode::with_runner`.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for WrapUpNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for WrapUpNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut state = latest_state(&ctx)?;
        let date = (self.clock)();

        let policy = resolved_policy(&ctx)?;
        let outcomes = finalize_outcomes(&ctx, &state, &policy, Utc::now());
        state.policy = Some(policy);
        state.outcomes = Some(outcomes);

        let terminal_signal = derive_terminal_signal(&ctx);
        state.bail_reason = super::schema::derive_bail_reason(terminal_signal.as_ref());
        state.global_status =
            super::schema::derive_committed_status(&state, terminal_signal.as_ref()).to_string();

        // `FinalValidationNode`'s outcome (if the run reached the drain
        // branch — a bailed run never does) is reported, never converted
        // into a bail: the tasks themselves already passed their own
        // tripwires and reviews, so a failed run-level gate is a degraded
        // terminal status, not a `TerminalSignal::MajorBail` (see this
        // spec's `tasks.md` Notes and `D12-per-task-vs-final-check-depth.md`
        // for why `derive_terminal_signal`/`derive_committed_status` above
        // deliberately never consult it).
        let final_validation = committed_final_validation(&ctx);

        let spec_slug = &state.spec_slug;
        let tasks_passed = state.telemetry.tasks_passed;
        let tasks_failed = state.telemetry.tasks_failed;
        let total_attempts = state.telemetry.total_attempts;
        let outcome = if tasks_failed == 0 {
            "PASS"
        } else {
            "PARTIAL/FAIL"
        };
        let gate_failed = final_validation.as_ref().is_some_and(|fv| !fv.all_passed);
        let gate_note = final_validation
            .as_ref()
            .filter(|fv| !fv.all_passed)
            .map(|fv| format!(" Final validation gate FAILED: {}", fv.failure_summary))
            .unwrap_or_default();

        let log_entry = format!(
            "## {date} — {spec_slug}\n\n\
             Outcome: {outcome}. {tasks_passed} task(s) passed, {tasks_failed} failed, \
             {total_attempts} total implement/test attempt(s).{gate_note}"
        );

        let report = format!(
            "# SDLC Flow Report — {spec_slug}\n\n\
             - Date: {date}\n\
             - Outcome: {outcome}\n\
             - Tasks passed: {tasks_passed}\n\
             - Tasks failed: {tasks_failed}\n\
             - Total attempts: {total_attempts}\n\
             {gate_note}"
        );

        let status_suggestion = if outcome == "PASS" && !gate_failed {
            format!(
                "{spec_slug} completed successfully on {date} \
                 ({tasks_passed} task(s) passed, {total_attempts} total attempt(s)). \
                 Ready for review/merge."
            )
        } else if gate_failed {
            format!(
                "{spec_slug} finished its task loop on {date} \
                 ({tasks_passed} passed / {tasks_failed} failed, {total_attempts} total \
                 attempt(s)) but the final validation gate FAILED:{gate_note} \
                 Needs follow-up before merge."
            )
        } else {
            format!(
                "{spec_slug} did not complete cleanly on {date} \
                 ({tasks_passed} passed / {tasks_failed} failed, {total_attempts} total \
                 attempt(s)). Needs follow-up before merge."
            )
        };

        let state_value = serde_json::to_value(&state)
            .map_err(|err| NodeError::new(format!("failed to serialize SDLCState: {err}")))?;

        // Persist the policy/outcomes-stamped state to the durable on-disk
        // file (EN.3.C task 6 fix; D31-committed path/schema, EN.4.1) when
        // this run went through `SetupWorktreeNode`. Absent in unit tests
        // that drive `WrapUpNode` directly with no worktree — that's not a
        // real run, so there is nothing to persist to.
        let saved_to = match worktree_path(&ctx) {
            Some(worktree) => {
                let state_path = state_path_for(&worktree, spec_slug)?;
                let run_meta = build_run_meta(&ctx, &worktree, &state_path);
                let review = committed_review(&ctx);
                let docs = committed_docs(&ctx);
                let pr = committed_pr(&ctx);
                let saved = persist_state(
                    &state_path,
                    &state,
                    &run_meta,
                    review.as_ref(),
                    docs.as_ref(),
                    pr.as_ref(),
                    final_validation.as_ref(),
                    terminal_signal.as_ref(),
                )?;
                // The widened staging here is what finally commits
                // `PatchDocsNode`'s doc edits. The drain order is
                // `FinalValidationNode → PatchDocsNode → WrapUpNode →
                // PullRequestNode` (`graph.rs`), and `docs.rs` makes no git
                // calls of its own, so those edits sit uncommitted in the
                // working tree until this line — which runs BEFORE
                // `PullRequestNode`'s `git push`, so they reach the branch.
                //
                // On a MAJOR_BAIL this also sweeps up the bailed task's
                // partial, unvalidated work (that task never reached
                // `SaveStateNode`). That is deliberate: visible attempted
                // work on a draft-PR handoff beats silently discarding it.
                // Do not narrow this back to a file list to "keep the bail
                // clean" — the discarded-work failure mode is worse.
                commit_all(
                    &self.runner,
                    std::path::Path::new(&worktree),
                    "chore(sdlc): wrap-up — docs patch + terminal state",
                );
                Some(saved)
            }
            None => None,
        };

        let mut output = json!({
            "log_entry": log_entry,
            "report": report,
            "status_suggestion": status_suggestion,
            "state": state_value,
        });
        if let Some(saved_to) = saved_to {
            output["saved_to"] = json!(saved_to);
        }

        put_result(&mut ctx, "WrapUpNode", output);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "WrapUpNode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::schema::{SDLCState, SDLCTask, SDLCTaskStatus};
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn fixed_clock(date: &'static str) -> ClockFn {
        Arc::new(move || date.to_string())
    }

    /// Builds a `WrapUpNode`-ready `ctx` and stamps a default [`SdlcPolicy`]
    /// under [`RESOLVED_POLICY_IDENTITY`] — required since task 8's strict
    /// `resolved_policy_strict` read (no more silent `Default` fallback for
    /// an unstamped ctx). Tests wanting a non-default policy insert their
    /// own `RESOLVED_POLICY_IDENTITY` entry afterwards, overwriting this
    /// stamp.
    fn ctx_with_state(state: &SDLCState) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({ "spec_slug": state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(state).unwrap(),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );
        ctx
    }

    #[tokio::test]
    async fn wrap_up_renders_pass_outcome() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 3;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 4;

        let ctx = ctx_with_state(&state);
        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let log_entry = result["log_entry"].as_str().unwrap();
        let report = result["report"].as_str().unwrap();
        let status_suggestion = result["status_suggestion"].as_str().unwrap();

        assert!(log_entry.contains("2026-07-18"));
        assert!(log_entry.contains("EN.3.B-sdlc-flow-docs-wrapup-pr"));
        assert!(log_entry.contains("PASS"));
        assert!(log_entry.contains("3 task(s) passed"));
        assert!(log_entry.contains("0 failed"));
        assert!(log_entry.contains("4 total"));

        assert!(report.contains("Outcome: PASS"));
        assert!(report.contains("Tasks passed: 3"));
        assert!(report.contains("Tasks failed: 0"));
        assert!(report.contains("Total attempts: 4"));

        assert!(status_suggestion.contains("completed successfully"));
        assert!(status_suggestion.contains("2026-07-18"));
    }

    #[tokio::test]
    async fn wrap_up_renders_partial_fail_outcome() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        state.telemetry.tasks_passed = 2;
        state.telemetry.tasks_failed = 1;
        state.telemetry.total_attempts = 5;

        let ctx = ctx_with_state(&state);
        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-19"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let log_entry = result["log_entry"].as_str().unwrap();
        let report = result["report"].as_str().unwrap();
        let status_suggestion = result["status_suggestion"].as_str().unwrap();

        assert!(log_entry.contains("PARTIAL/FAIL"));
        assert!(report.contains("Outcome: PARTIAL/FAIL"));
        assert!(report.contains("Tasks passed: 2"));
        assert!(report.contains("Tasks failed: 1"));
        assert!(report.contains("Total attempts: 5"));
        assert!(status_suggestion.contains("did not complete cleanly"));
        assert!(status_suggestion.contains("Needs follow-up"));
    }

    #[tokio::test]
    async fn wrap_up_falls_back_to_load_task_state_node() {
        let mut state = SDLCState::new("EN.3.B-sdlc-flow-docs-wrapup-pr");
        state.telemetry.tasks_passed = 0;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 0;

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&state).unwrap(),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(crate::workflows::sdlc_flow::policy::SdlcPolicy::default())
                .expect("SdlcPolicy serializes"),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-01-01"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        assert!(result["log_entry"].as_str().unwrap().contains("PASS"));
    }

    /// EN.3.G task 2 (proof test): before the unified `latest_state`,
    /// `WrapUpNode`'s local copy considered only `UpdateTaskStatusNode` /
    /// `LoadTaskStateNode` and never `IncrementAttemptNode`, so a
    /// `MAJOR_BAIL` reached after retries (where `IncrementAttemptNode`
    /// holds the newest, post-increment state and `UpdateTaskStatusNode`
    /// holds a stale, pre-retry snapshot) made `WrapUpNode` under-report
    /// `attempt_count` / `total_retries`. This test fails before the fix
    /// (it would resolve to the stale `UpdateTaskStatusNode` state) and
    /// passes after `wrap_up.rs` calls `task_loop::latest_state`.
    #[tokio::test]
    async fn wrap_up_uses_post_increment_state_from_increment_attempt_node() {
        // Stale snapshot: as it looked before the retry that bailed.
        let mut stale_state = SDLCState::new("EN.3.G-terminal-path-robustness");
        let mut stale_task = SDLCTask::new(1, "One", "d1");
        stale_task.attempt_count = 1;
        stale_state.tasks.push(stale_task);
        stale_state.telemetry.tasks_passed = 0;
        stale_state.telemetry.tasks_failed = 0;
        stale_state.telemetry.total_attempts = 3;

        // Newest state: after `IncrementAttemptNode` bumped the retry
        // counter for a task that then MAJOR_BAILed.
        let mut incremented_state = SDLCState::new("EN.3.G-terminal-path-robustness");
        let mut incremented_task = SDLCTask::new(1, "One", "d1");
        incremented_task.attempt_count = 3;
        incremented_state.tasks.push(incremented_task);
        incremented_state.telemetry.tasks_passed = 0;
        incremented_state.telemetry.tasks_failed = 1;
        incremented_state.telemetry.total_attempts = 5;

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": incremented_state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(&stale_state).unwrap(),
        );
        ctx.nodes.insert(
            "IncrementAttemptNode".to_string(),
            serde_json::to_value(&incremented_state).unwrap(),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-31"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone())
            .expect("WrapUpNode output carries a parseable SDLCState");

        let outcomes = stamped_state.outcomes.expect("outcomes block populated");
        assert_eq!(
            outcomes.total_attempts, 5,
            "must report the post-increment total_attempts, not the stale UpdateTaskStatusNode value"
        );
        assert_eq!(
            outcomes.total_retries, 2,
            "must report the post-increment attempt_count (3 - 1 = 2 retries)"
        );

        let report = result["report"].as_str().unwrap();
        assert!(report.contains("Total attempts: 5"));
    }

    /// Regression guard: when `UpdateTaskStatusNode` genuinely holds the
    /// newest state (the common happy-path case — no retry back-edge taken
    /// after it), it must still win.
    #[tokio::test]
    async fn wrap_up_still_resolves_to_update_task_status_node_when_newest() {
        let mut older_increment_state = SDLCState::new("EN.3.G-terminal-path-robustness");
        older_increment_state.telemetry.total_attempts = 2;

        let mut newest_state = SDLCState::new("EN.3.G-terminal-path-robustness");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        newest_state.tasks.push(task);
        newest_state.telemetry.tasks_passed = 1;
        newest_state.telemetry.tasks_failed = 0;
        newest_state.telemetry.total_attempts = 4;

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": newest_state.spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "IncrementAttemptNode".to_string(),
            serde_json::to_value(&older_increment_state).unwrap(),
        );
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(&newest_state).unwrap(),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-31"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone())
            .expect("WrapUpNode output carries a parseable SDLCState");

        let outcomes = stamped_state.outcomes.expect("outcomes block populated");
        assert_eq!(outcomes.total_attempts, 4);
        assert_eq!(outcomes.tasks_passed, 1);
    }

    /// `write_terminal_blocked_state` shares the same unified `latest_state`
    /// helper, so it must resolve the same post-increment counters as
    /// `WrapUpNode::process` over the same retry-bailed ctx.
    #[test]
    fn write_terminal_blocked_state_uses_post_increment_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let worktree = tmp.path();
        let spec_slug = "EN.3.G-terminal-path-robustness";
        let state_dir = worktree.join("planning").join(spec_slug).join("sdlc");
        std::fs::create_dir_all(&state_dir).expect("create state dir");

        let mut stale_state = SDLCState::new(spec_slug);
        stale_state.telemetry.total_attempts = 3;

        let mut incremented_state = SDLCState::new(spec_slug);
        let mut task = SDLCTask::new(1, "One", "d1");
        task.attempt_count = 3;
        incremented_state.tasks.push(task);
        incremented_state.telemetry.total_attempts = 5;

        let mut ctx = TaskContext {
            event: json!({ "spec_slug": spec_slug }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy().to_string() }),
        );
        ctx.nodes.insert(
            "UpdateTaskStatusNode".to_string(),
            serde_json::to_value(&stale_state).unwrap(),
        );
        ctx.nodes.insert(
            "IncrementAttemptNode".to_string(),
            serde_json::to_value(&incremented_state).unwrap(),
        );

        let written_path = write_terminal_blocked_state(&ctx, "MAJOR_BAIL: too many retries")
            .expect("should write a terminal state");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(written_path).unwrap()).unwrap();

        assert_eq!(
            written["telemetry"]["total_attempts"], 5,
            "write_terminal_blocked_state must report the post-increment total_attempts"
        );
        assert_eq!(
            written["tasks"]["1"]["attempt_count"], 3,
            "write_terminal_blocked_state must report the post-increment attempt_count"
        );
    }

    #[tokio::test]
    async fn wrap_up_stamps_resolved_policy_and_outcomes_into_state() {
        use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, SdlcPolicy};

        let mut state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        task.attempt_count = 2;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 2;

        let mut ctx = ctx_with_state(&state);
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                triage: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).unwrap(),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "PASS", "summary": "s", "issues": [] }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone())
            .expect("WrapUpNode output carries a parseable SDLCState");

        let stamped_policy = stamped_state.policy.expect("policy block populated");
        assert_eq!(stamped_policy, policy);

        let outcomes = stamped_state.outcomes.expect("outcomes block populated");
        assert_eq!(outcomes.total_attempts, 2);
        assert_eq!(outcomes.tasks_passed, 1);
        assert_eq!(outcomes.tasks_failed, 0);
        assert_eq!(outcomes.total_retries, 1);
        assert_eq!(
            outcomes.review_verdicts,
            vec!["ConsolidatedReviewNode:PASS".to_string()]
        );
        assert_eq!(outcomes.model_tier_used["triage"], "haiku");
        assert_eq!(outcomes.model_tier_used["implement"], "sonnet");
        // The two stages onboarded to the policy path report the tiers the
        // nodes actually consume; `generate`'s default is the Opus tier
        // `GenerateTasksNode` really runs.
        assert_eq!(outcomes.model_tier_used["docs"], "sonnet");
        assert_eq!(outcomes.model_tier_used["generate"], "opus");
    }

    /// Task 8 deleted the lenient `Default`-fallback read — every `ctx`
    /// (including `ctx_with_state`'s own fixture stamp) now carries an
    /// explicit policy. This still exercises the built-in-default *values*
    /// end to end, just via an explicit stamp rather than a silent
    /// fallback.
    #[tokio::test]
    async fn wrap_up_uses_explicitly_stamped_default_policy() {
        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let ctx = ctx_with_state(&state);

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone()).unwrap();
        assert_eq!(
            stamped_state.policy,
            Some(crate::workflows::sdlc_flow::policy::SdlcPolicy::default())
        );
        assert_eq!(stamped_state.outcomes.unwrap().wall_clock_secs, 0.0);
    }

    #[tokio::test]
    async fn wrap_up_sums_tokens_and_cost_across_model_nodes() {
        use engine_contract::{NodeRun, NodeRunStatus, Usage};

        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let mut ctx = ctx_with_state(&state);

        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "content": "x", "cost_usd": 0.01, "model": "claude-haiku-4-5" }),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({
                "content": "x",
                "cost_usd": 0.02,
                "model": "claude-sonnet-4-5",
                "verdict": "PASS",
            }),
        );
        ctx.node_runs.insert(
            "TriageTaskNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    model: "claude-haiku-4-5".to_string(),
                }),
            },
        );
        ctx.node_runs.insert(
            "ConsolidatedReviewNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(Usage {
                    input_tokens: Some(20),
                    output_tokens: Some(8),
                    model: "claude-sonnet-4-5".to_string(),
                }),
            },
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let stamped_state: SDLCState = serde_json::from_value(result["state"].clone()).unwrap();
        let outcomes = stamped_state.outcomes.unwrap();

        assert_eq!(outcomes.total_input_tokens, 30);
        assert_eq!(outcomes.total_output_tokens, 13);
        assert!((outcomes.total_cost_usd - 0.03).abs() < 1e-9);
    }

    #[tokio::test]
    async fn two_different_policies_yield_different_recorded_tiers() {
        use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, SdlcPolicy};

        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");

        let mut ctx_a = ctx_with_state(&state);
        ctx_a.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(SdlcPolicy::default()).unwrap(),
        );

        let mut ctx_b = ctx_with_state(&state);
        let policy_b = SdlcPolicy {
            model_tiers: ModelTiers {
                review: ModelTier::Local,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        ctx_b.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy_b).unwrap(),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out_a = node
            .process(ctx_a)
            .await
            .expect("process should succeed")
            .nodes["WrapUpNode"]
            .clone();
        let out_b = WrapUpNode::new()
            .with_clock(fixed_clock("2026-07-18"))
            .process(ctx_b)
            .await
            .expect("process should succeed")
            .nodes["WrapUpNode"]
            .clone();

        let state_a: SDLCState = serde_json::from_value(out_a["state"].clone()).unwrap();
        let state_b: SDLCState = serde_json::from_value(out_b["state"].clone()).unwrap();

        assert_eq!(
            state_a.outcomes.unwrap().model_tier_used["review"],
            "sonnet"
        );
        assert_eq!(state_b.outcomes.unwrap().model_tier_used["review"], "local");
    }

    fn temp_worktree() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-sdlc-flow-wrap-up-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `setup.rs`'s `temp_dir_named` doc comment for
        // why PID-recycling makes this removal necessary, not optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn wrap_up_persists_policy_and_outcomes_to_on_disk_state_file() {
        use crate::workflows::sdlc_flow::policy::{ModelTier, ModelTiers, SdlcPolicy};

        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        task.attempt_count = 2;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;
        state.telemetry.tasks_failed = 0;
        state.telemetry.total_attempts = 2;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        let policy = SdlcPolicy {
            model_tiers: ModelTiers {
                triage: ModelTier::Haiku,
                ..ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).unwrap(),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "PASS", "summary": "s", "issues": [] }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        let saved_to = result["saved_to"]
            .as_str()
            .expect("saved_to present when a worktree is set");
        assert!(saved_to.ends_with("sdlc/sdlc-flow-state.json"));

        // Re-read the actual on-disk file the aggregator (task 7) reads
        // from — not the transient ctx output — to confirm the policy +
        // outcomes blocks are populated there, in the D31-committed shape.
        let on_disk = std::fs::read_to_string(saved_to).expect("state file exists on disk");
        let on_disk_value: serde_json::Value =
            serde_json::from_str(&on_disk).expect("on-disk file parses as JSON");
        assert!(on_disk_value["tasks"].is_object());
        assert_eq!(on_disk_value["status"], json!("done"));
        assert_eq!(on_disk_value["review"]["verdict"], json!("PASS"));
        assert!(on_disk_value["bail_reason"].is_null());

        let on_disk_state = SDLCState::from_committed_state_json(&on_disk_value)
            .expect("on-disk file parses as SDLCState");

        let stamped_policy = on_disk_state
            .policy
            .expect("policy block populated on disk");
        assert_eq!(stamped_policy, policy);

        let outcomes = on_disk_state
            .outcomes
            .expect("outcomes block populated on disk");
        assert_eq!(outcomes.total_attempts, 2);
        assert_eq!(outcomes.tasks_passed, 1);
        assert_eq!(outcomes.tasks_failed, 0);
        assert_eq!(outcomes.total_retries, 1);
        assert_eq!(
            outcomes.review_verdicts,
            vec!["ConsolidatedReviewNode:PASS".to_string()]
        );
        assert_eq!(outcomes.model_tier_used["triage"], "haiku");

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_populates_bail_reason_and_blocked_status_on_major_bail() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.4.1-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Failed;
        task.attempt_count = 3;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 0;
        state.telemetry.tasks_failed = 1;
        state.telemetry.total_attempts = 3;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy(), "branch_name": "sdlc/x" }),
        );
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({
                "verdict": "MAJOR_BAIL",
                "reason": "Max attempts (3) reached without a passing run.",
            }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let saved_to = result["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["status"], json!("blocked"));
        assert_eq!(
            value["bail_reason"],
            json!("Max attempts (3) reached without a passing run.")
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_populates_bail_reason_on_structural_review_fail() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.4.1-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Failed;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 0;
        state.telemetry.tasks_failed = 1;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy(), "branch_name": "sdlc/x" }),
        );
        ctx.nodes.insert(
            "ConsolidatedReviewNode".to_string(),
            json!({ "verdict": "FAIL", "summary": "structural issues", "issues": [] }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let saved_to = result["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["status"], json!("blocked"));
        assert_eq!(value["bail_reason"], json!("structural issues"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// EN.3.G task 1: a ctx carrying `unrecognized_verdict` (stamped by
    /// `TriageTaskNode`/`ConsolidatedReviewNode` when the model's reply is
    /// garbage) still yields a `MajorBail` terminal signal whose message
    /// names the offending string — the router-fallback safety net's other
    /// half, proving the run's `bail_reason` on disk is diagnosable rather
    /// than silently swallowed as `done`.
    #[tokio::test]
    async fn wrap_up_populates_bail_reason_on_unrecognized_triage_verdict() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.G-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Failed;
        state.tasks.push(task);

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy(), "branch_name": "sdlc/x" }),
        );
        ctx.nodes.insert(
            "TriageTaskNode".to_string(),
            json!({ "verdict": "WAT", "reason": "unclear", "unrecognized_verdict": "WAT" }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];
        let saved_to = result["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["status"], json!("blocked"));
        let bail_reason = value["bail_reason"].as_str().unwrap();
        assert!(bail_reason.contains("WAT"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    // -- EN.3.G task 3: WrapUpNode commits its terminal state write --------

    /// `WrapUpNode` over a real worktree, with a recording stub runner,
    /// issues exactly the same `git add` then `git commit` invocation
    /// `task_loop::SaveStateNode` does — proving the terminal `done`/
    /// `blocked` state is committed, not left dangling in the working tree
    /// (without this a `--worktree` run's PR would not contain its own
    /// final state).
    #[tokio::test]
    async fn wrap_up_commits_its_terminal_state_write() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.G-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        type RecordedCall = (String, Vec<String>);
        let calls: Arc<Mutex<Vec<RecordedCall>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = calls.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            calls_clone.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
            ));
            Ok(crate::workflows::sdlc_flow::CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = WrapUpNode::new()
            .with_clock(fixed_clock("2026-07-31"))
            .with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");
        let _saved_to = out.nodes["WrapUpNode"]["saved_to"].as_str().unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "git");
        // `add -A`, not `add <state_path>`: the wrap-up commit is what
        // finally lands `PatchDocsNode`'s doc edits on the branch.
        assert_eq!(recorded[0].1, vec!["add".to_string(), "-A".to_string()]);
        assert_eq!(recorded[1].0, "git");
        assert_eq!(recorded[1].1[0], "commit");
        assert_eq!(
            recorded[1].1[2],
            "chore(sdlc): wrap-up — docs patch + terminal state"
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// A non-zero `git commit` outcome (e.g. "nothing to commit") must not
    /// fail the node — mirrors `SaveStateNode`'s non-fatal treatment.
    #[tokio::test]
    async fn wrap_up_tolerates_a_non_zero_commit_outcome() {
        let worktree = temp_worktree();

        let state = SDLCState::new("EN.3.G-fixture");
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let runner: CommandRunner = Arc::new(|_program, args, _cwd| {
            if args.first() == Some(&"commit") {
                Ok(crate::workflows::sdlc_flow::CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "nothing to commit, working tree clean".to_string(),
                })
            } else {
                Ok(crate::workflows::sdlc_flow::CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        });

        let node = WrapUpNode::new()
            .with_clock(fixed_clock("2026-07-31"))
            .with_runner(runner);
        let out = node
            .process(ctx)
            .await
            .expect("non-zero commit must not fail the node");
        assert!(out.nodes["WrapUpNode"]["saved_to"].as_str().is_some());

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_skips_disk_persist_without_a_worktree() {
        let state = SDLCState::new("EN.3.C-tunable-run-policy-telemetry");
        let ctx = ctx_with_state(&state);

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");

        let result = &out.nodes["WrapUpNode"];
        assert!(result.get("saved_to").is_none());
    }

    // -- EN.3.E task 3: `final_validation` folded into wrap-up ------------

    #[tokio::test]
    async fn wrap_up_writes_passing_final_validation_to_disk() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.E-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            "FinalValidationNode".to_string(),
            json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["WrapUpNode"]["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["final_validation"]["all_passed"], json!(true));
        assert_eq!(value["status"], json!("done"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_writes_failing_final_validation_with_degraded_wording_but_non_blocked_status()
    {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.E-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );
        ctx.nodes.insert(
            "FinalValidationNode".to_string(),
            json!({
                "all_passed": false,
                "check_results": [
                    { "name": "build", "kind": "command", "passed": false },
                ],
                "failure_summary": "Failed checks: build",
            }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let result = &out.nodes["WrapUpNode"];

        // Reported, not converted into a bail: no task itself failed, so
        // `status` stays out of `"blocked"` even though the run-level gate
        // failed (see `derive_terminal_signal`/`derive_committed_status`,
        // which never consult `FinalValidationNode`).
        let status_suggestion = result["status_suggestion"].as_str().unwrap();
        assert!(status_suggestion.contains("FAILED"));
        assert!(status_suggestion.contains("Failed checks: build"));

        let saved_to = result["saved_to"].as_str().unwrap();
        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["final_validation"]["all_passed"], json!(false));
        assert_eq!(
            value["final_validation"]["failure_summary"],
            json!("Failed checks: build")
        );
        assert_ne!(value["status"], json!("blocked"));
        assert_eq!(value["status"], json!("done"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_writes_null_final_validation_when_node_never_ran() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.3.E-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;

        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["WrapUpNode"]["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert!(value["final_validation"].is_null());
        assert_eq!(value["status"], json!("done"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    // -- EN.6.J task 4: run_id stamp + failure-path terminal writer --------

    #[tokio::test]
    async fn wrap_up_stamps_run_id_from_context_metadata() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.6.J-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Done;
        state.tasks.push(task);
        state.telemetry.tasks_passed = 1;

        let run_id = uuid::Uuid::new_v4();
        let mut ctx = ctx_with_state(&state);
        ctx.metadata = json!({ crate::RUN_ID_METADATA_KEY: run_id.to_string() });
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["WrapUpNode"]["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["run_id"], json!(run_id.to_string()));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[tokio::test]
    async fn wrap_up_writes_null_run_id_for_empty_metadata() {
        let worktree = temp_worktree();

        let state = SDLCState::new("EN.6.J-fixture");
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let node = WrapUpNode::new().with_clock(fixed_clock("2026-07-18"));
        let out = node.process(ctx).await.expect("process should succeed");
        let saved_to = out.nodes["WrapUpNode"]["saved_to"].as_str().unwrap();

        let on_disk = std::fs::read_to_string(saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert!(value["run_id"].is_null());

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// Sets up an on-disk state file the way `SaveStateNode`/`WrapUpNode`
    /// would (parent `sdlc/` directory already exists) so
    /// [`write_terminal_blocked_state`] finds it and writes.
    fn seed_state_file(worktree: &std::path::Path, spec_slug: &str, state: &SDLCState) {
        let state_dir = worktree.join("planning").join(spec_slug).join("sdlc");
        std::fs::create_dir_all(&state_dir).unwrap();
        let run_meta = RunMeta {
            branch: "sdlc/x".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-07-01T00:00:00Z".to_string(),
            updated_at: "2026-07-01T00:00:00Z".to_string(),
            run_id: None,
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        let json = serde_json::to_string_pretty(&committed).unwrap();
        std::fs::write(state_dir.join("sdlc-flow-state.json"), json).unwrap();
    }

    #[test]
    fn write_terminal_blocked_state_writes_blocked_status_and_preserves_started_at() {
        let worktree = temp_worktree();

        let mut state = SDLCState::new("EN.6.J-terminal-fixture");
        let mut task = SDLCTask::new(1, "One", "d1");
        task.status = SDLCTaskStatus::Failed;
        state.tasks.push(task);
        seed_state_file(&worktree, &state.spec_slug, &state);

        let run_id = uuid::Uuid::new_v4();
        let mut ctx = ctx_with_state(&state);
        ctx.metadata = json!({ crate::RUN_ID_METADATA_KEY: run_id.to_string() });
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        let saved_to = write_terminal_blocked_state(&ctx, "node ImplementTaskNode failed: boom")
            .expect("worktree + loaded state present, parent dir exists");

        let on_disk = std::fs::read_to_string(&saved_to).unwrap();
        let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(value["status"], json!("blocked"));
        assert_eq!(
            value["bail_reason"],
            json!("node ImplementTaskNode failed: boom")
        );
        assert_eq!(value["run_id"], json!(run_id.to_string()));
        // started_at preserved from the file seeded above, not re-stamped.
        assert_eq!(value["started_at"], json!("2026-07-01T00:00:00Z"));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[test]
    fn write_terminal_blocked_state_returns_none_without_a_worktree() {
        let state = SDLCState::new("EN.6.J-terminal-fixture");
        let ctx = ctx_with_state(&state);

        assert_eq!(write_terminal_blocked_state(&ctx, "boom"), None);
    }

    #[test]
    fn write_terminal_blocked_state_returns_none_without_loaded_state() {
        let worktree = temp_worktree();

        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        assert_eq!(write_terminal_blocked_state(&ctx, "boom"), None);

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[test]
    fn write_terminal_blocked_state_returns_none_when_state_dir_missing() {
        // Worktree exists but the `planning/{spec_slug}/sdlc` directory was
        // never created (no prior SaveStateNode/WrapUpNode write) — the
        // writer must not create it itself.
        let worktree = temp_worktree();

        let state = SDLCState::new("EN.6.J-terminal-fixture");
        let mut ctx = ctx_with_state(&state);
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": worktree.to_string_lossy() }),
        );

        assert_eq!(write_terminal_blocked_state(&ctx, "boom"), None);

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[test]
    fn default_clock_produces_a_plausible_date_string() {
        let clock = default_clock();
        let date = clock();
        assert_eq!(date.len(), 10);
        assert_eq!(date.chars().nth(4), Some('-'));
        assert_eq!(date.chars().nth(7), Some('-'));
    }
}
