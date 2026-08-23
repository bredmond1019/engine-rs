//! Operator hold as pause-and-resume, state-write verification, and the
//! lane-log append — `EN.10.B` Task 4.
//!
//! Three independent pieces, plus the loop that ties them to
//! [`execute_step`](super::execute::execute_step) and [`gates`](super::gates):
//!
//! 1. [`HoldSource`] / [`wait_for_clearance`] — the operator hold, honoured
//!    through `EN.9.G`'s Blocked-edge bridge. **A hold is a pause, not a
//!    failure.** `EN.9.G`'s receiver re-reads a session's *current* level
//!    on every check rather than trusting a trigger's own payload (see
//!    `engine-serve::blocked_bridge`'s "THE CENTRAL RULE"); this module
//!    mirrors that discipline with its own [`HoldSource::is_held`] seam —
//!    "held right now", re-read on every poll, never cached across a
//!    wait. [`integrate_chain`] checks it before every step and, when
//!    held, simply `.await`s [`wait_for_clearance`] inline in the same
//!    sequential loop — the steps already executed are not re-run because
//!    they are never revisited; the loop only ever advances forward. That
//!    is the entire mechanism behind "pauses and resumes without
//!    restarting completed blocks": there is no separate resume path to
//!    get wrong.
//! 2. [`verify_state_write`] — after `SDLC_FLOW` returns for a block, read
//!    that block's own `planning/{block_id}/sdlc/sdlc-flow-state.json`
//!    (the file `wrap_up.rs`'s `state_path_for` writes) inside the
//!    block's repo and confirm `"status"` reads `"done"`. This is the
//!    entire point of moving orchestration into the engine: it takes the
//!    state write off the agent prompt and retires the reliability class
//!    `base-template:BT.ticket.sdlc-state-write-reliability` tracks. A
//!    mismatch is [`IntegrateError::StateWriteMismatch`] and **fails the
//!    run loudly** — this module never downgrades a mismatch to a
//!    warning, which would recreate exactly the unreliability it exists
//!    to replace. It also independently rejects a state file whose
//!    `final_validation.all_passed` reads `false` even when `"status"`
//!    reads `"done"` ([`IntegrateError::FinalValidationGateFailed`]) — the
//!    second end of `EN.ticket.final-validation-failure-must-block`'s fix,
//!    defence in depth for a state file written by an older engine build
//!    or by the JS `/sdlc-flow`, neither of which carries the in-engine
//!    guard `wrap_up.rs`'s `derive_terminal_signal` now applies.
//! 3. [`resolve_roadmap_dir`] / [`append_lane_log_line`] — resolve the
//!    roadmap directory by `/begin-orchestration`'s Step 1C rule
//!    (`planning/roadmaps/<slug>/` first, then legacy `planning/<slug>/`;
//!    both existing is an error — "an ambiguous roadmap is how a lane
//!    appends to the wrong lane log"), then append **exactly one** line
//!    per integrated block to that directory's `lane-log.jsonl`. The log
//!    is the cross-lane channel; a duplicated or missing line is how a
//!    sibling lane reads the wrong state, so [`integrate_chain`] appends
//!    the line only once, immediately after that block's state write
//!    verifies.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::repo_registry::RepoRegistry;

use crate::budget::{Budget, BudgetDecision, BudgetHaltReason, CampaignLedger};

use super::chain::ChainStep;
use super::checkpoint::{
    read_checkpoint, write_checkpoint, Checkpoint, CheckpointStep, ReadCheckpoint,
};
use super::execute::{execute_step, EngineKind, ExecuteError, ExecutionOutcome, FlowRunner};
use super::gates::{check_dependencies, AdmissionGate, DependencyEdge, GateError};
use crate::workflows::get_result;

// ── Operator hold: pause-and-resume ─────────────────────────────────────

/// The live "is this block currently held" seam, re-read on every poll —
/// never trusted from a one-shot trigger payload. See the module doc's
/// point 1. The production implementation wraps `EN.9.G`'s
/// `BlockedEdgeSource` (re-derived per repo/block from the session the
/// block's `SDLC_FLOW` run occupies); tests substitute a double that flips
/// under a shared flag, exactly like [`super::execute::FlowRunner`]'s own
/// test doubles record calls instead of spawning a real session.
pub trait HoldSource: Send + Sync {
    /// Whether `block_id` (in `repo`) is under an operator hold right now.
    fn is_held(&self, repo: &str, block_id: &str) -> bool;

    /// EN.11.L Task 2: an optional notification path. Resolves when a
    /// hold on `(repo, block_id)` may have just cleared, so
    /// [`wait_for_clearance`] can wake immediately instead of waiting out
    /// the next poll tick. The default never resolves (`future::pending`)
    /// — a `HoldSource` that doesn't override this falls back to pure
    /// poll-driven behavior, unchanged from before this knob existed.
    /// Production wiring (the `EN.9.G` Blocked-edge bridge) is expected
    /// to hold a `tokio::sync::Notify` per watched hold and call
    /// `notify_waiters()` the moment its own poll observes clearance;
    /// this trait does not mandate a `Notify` specifically, only the
    /// future shape.
    fn notified(&self, repo: &str, block_id: &str) -> futures::future::BoxFuture<'_, ()> {
        let _ = (repo, block_id);
        Box::pin(futures::future::pending())
    }
}

/// A [`HoldSource`] that is never held — the default for a chain running
/// without operator-hold wiring at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeverHeld;

impl HoldSource for NeverHeld {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        false
    }
}

/// Poll `hold_source` for `(repo, block_id)` every `poll_interval` until it
/// reports not-held, then return. Never fails — a hold is a pause, not an
/// error condition; the caller resumes its own sequential loop the moment
/// this returns, without having lost or re-run anything, because nothing
/// prior to this await point is ever touched again.
///
/// `cancellation_token`, when given, is raced against every poll tick's
/// sleep — mirroring `TerminalAwaitNode`'s own `select!` (see that node's
/// module doc). A chain parked here for hours on a held block must abort
/// within one poll interval of an abort request, not after the hold
/// eventually clears on its own; a naive "check once, then sleep the whole
/// interval" loop would miss exactly this case, since the check only runs
/// at the top of each iteration. A cancel win returns immediately without
/// re-checking `hold_source` — the caller (`integrate_chain`) is
/// responsible for observing the cancellation afterwards and stopping the
/// chain; this function itself never treats cancellation as clearance.
///
/// EN.11.L Task 2 extends this same `select!` with two more arms, added
/// alongside the existing cancellation arm rather than replacing it:
///
/// - `deadline`, when given, bounds the TOTAL time this call is willing to
///   wait (not per-poll — the clock starts once, at entry, and is not
///   reset by any individual poll tick). Exceeding it returns
///   [`IntegrateError::HoldDeadlineExceeded`], naming `repo` and
///   `block_id` so a stopped chain never reads as a silent, unexplained
///   hang — see [`IntegrateError::HoldDeadlineExceeded`]'s doc for why
///   this is a loud, addressed error rather than a downgraded warning.
///   `None` preserves the exact pre-Task-2 behavior: an unbounded wait.
/// - [`HoldSource::notified`] is raced on every iteration so a hold that
///   clears between poll ticks wakes this call immediately rather than
///   waiting out the remainder of `poll_interval`. The default
///   implementation never resolves, so a `HoldSource` with no
///   notification wiring is unaffected — this is purely additive.
///
/// A cancel win still returns `Ok(())` immediately, exactly as before —
/// cancellation is a pause point, never an error, and remains distinct
/// from a deadline win (which IS an error, by design: nobody is coming
/// back for an unanswered hold, but a cancellation is always an explicit,
/// intentional request).
pub async fn wait_for_clearance(
    hold_source: &dyn HoldSource,
    repo: &str,
    block_id: &str,
    poll_interval: Duration,
    deadline: Option<Duration>,
    cancellation_token: Option<&crate::cancellation::CancellationToken>,
) -> Result<(), IntegrateError> {
    // The clock starts once, here, at entry — never reset by a poll tick
    // or a notification wake, so `deadline` is a total budget for the
    // whole wait, not a per-poll one.
    let deadline_at = deadline.map(|d| tokio::time::Instant::now() + d);

    while hold_source.is_held(repo, block_id) {
        if let (Some(at), Some(d)) = (deadline_at, deadline) {
            if tokio::time::Instant::now() >= at {
                return Err(IntegrateError::HoldDeadlineExceeded {
                    repo: repo.to_string(),
                    block_id: block_id.to_string(),
                    deadline: d,
                });
            }
        }

        // Never sleep past the deadline even when the deadline is closer
        // than `poll_interval` — otherwise a hold held right up to the
        // deadline could sleep one whole `poll_interval` past it before
        // the next top-of-loop check ever runs.
        let tick_for = match deadline_at {
            Some(at) => {
                let remaining = at.saturating_duration_since(tokio::time::Instant::now());
                poll_interval.min(remaining)
            }
            None => poll_interval,
        };
        let tick = tokio::time::sleep(tick_for);
        let notified = hold_source.notified(repo, block_id);

        if let Some(token) = cancellation_token {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                () = tick => {}
                () = notified => {}
            }
        } else {
            tokio::select! {
                () = tick => {}
                () = notified => {}
            }
        }
    }
    Ok(())
}

// ── State-write verification ────────────────────────────────────────────

/// The status value `SDLC_FLOW`'s `WrapUpNode` writes into
/// `sdlc-flow-state.json` on a successfully completed run
/// (`wrap_up.rs`'s own `"status": "done"` convention).
const EXPECTED_STATE_STATUS: &str = "done";

/// The status value a state file reads when its run ended on
/// `TerminalSignal::ReconcileFailed` — `close_block`'s own skip check
/// (`sdlc_flow::close_block`) leaves the block genuinely un-closed, and
/// `sdlc_task::lean_bookkeep` writes this same status and SKIPS the
/// block-status flip for the identical reason. Checked before
/// [`EXPECTED_STATE_STATUS`] so a reconcile failure surfaces its own
/// specific [`IntegrateError::ReconcileFailed`] rather than the generic
/// [`IntegrateError::StateWriteMismatch`].
const RECONCILE_FAILED_STATE_STATUS: &str = "reconcile_failed";

/// `planning/{block_id}/sdlc/{filename}` inside `repo_path`, where
/// `filename` is `sdlc_flow::DEFAULT_STATE_FILENAME`
/// (`"sdlc-flow-state.json"`) for [`EngineKind::Flow`] and
/// `sdlc_task::DEFAULT_STATE_FILENAME` (`"sdlc-task-state.json"`) for
/// [`EngineKind::Task`] — mirrors `workflows::sdlc_flow::wrap_up::
/// state_path_for` (and, for the task engine, the equivalent sdlc_task
/// writer) by hand: this module cannot import either directly, since both
/// are private to their own module. The two engines write distinct state
/// filenames so a spec driven by one engine never collides with a run of
/// the other against the same `planning/<slug>/` directory — keep this
/// mirror in lockstep with both if either path shape ever changes.
fn state_path_for(repo_path: &Path, block_id: &str, engine: EngineKind) -> PathBuf {
    let filename = match engine {
        EngineKind::Flow => crate::workflows::sdlc_flow::DEFAULT_STATE_FILENAME,
        EngineKind::Task => crate::workflows::sdlc_task::DEFAULT_STATE_FILENAME,
    };
    repo_path
        .join("planning")
        .join(block_id)
        .join("sdlc")
        .join(filename)
}

/// Read the state file `outcome`'s block should have written and confirm
/// `"status"` reads `"done"`. Fails loudly — never a warning — on a
/// missing file, unparsable JSON, missing `"status"` key, or any value
/// other than `"done"`; a mismatch on any of those is exactly the class of
/// silent unreliability this verification step exists to close out.
///
/// Belt to `wrap_up.rs`'s brace: also rejects a state file whose
/// `final_validation.all_passed` reads `false`, even though `"status"`
/// itself reads `"done"`. `wrap_up.rs`'s `derive_terminal_signal` now
/// consults the same gate and stops an in-engine run from ever writing
/// `"done"` on a failed gate (`EN.ticket.final-validation-failure-must-block`
/// Task 2) — but that guard lives inside one engine build. This assertion
/// is the only one that also catches a state file written by an older
/// engine build, or by the JS `/sdlc-flow`, which shares this exact path
/// and schema but has no equivalent guard. `final_validation` being
/// absent or `null` (a bailed run, a pre-`EN.3.E` file, or a JS-written
/// file) is not itself a failure — only an explicit `false` is.
///
/// Also cross-checks identity: if the file carries a non-null
/// `"block_id"`, it must agree with `outcome.block_id`
/// ([`IntegrateError::BlockIdMismatch`]) — this closes the gap where a
/// stale state file left behind by an earlier, different run at the same
/// path would otherwise be admitted as this block's result purely because
/// `"status"` happened to read `"done"`. A state file with no `"block_id"`
/// at all (an older run, or one written by the JS `/sdlc-flow`) still
/// passes; only an actual disagreement fails.
/// Read the `branch_name` `SetupWorktreeNode` stamped onto a completed
/// step's `ctx`, if the run went through it — `EN.11.H` task 2, mirrors
/// `sdlc_flow::wrap_up::branch_name` / `sdlc_flow::task_loop::branch_name`.
/// `None` for a plain-branch (no-worktree) run, or a step whose engine
/// isn't [`EngineKind::Flow`] — either way, "no known branch" rather than
/// an error; [`CheckpointStep::branch`] already treats `None` this way.
fn step_branch_name(ctx: &engine_contract::TaskContext) -> Option<String> {
    get_result(ctx, "SetupWorktreeNode")
        .and_then(|value| value.get("branch_name"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub fn verify_state_write(outcome: &ExecutionOutcome) -> Result<(), IntegrateError> {
    let path = state_path_for(&outcome.repo_path, &outcome.block_id, outcome.engine);
    let raw =
        std::fs::read_to_string(&path).map_err(|source| IntegrateError::StateWriteUnreadable {
            repo: outcome.repo.clone(),
            block_id: outcome.block_id.clone(),
            path: path.clone(),
            source,
        })?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|source| IntegrateError::StateWriteMalformed {
            repo: outcome.repo.clone(),
            block_id: outcome.block_id.clone(),
            path: path.clone(),
            source,
        })?;
    let status = value.get("status").and_then(Value::as_str);
    if status == Some(RECONCILE_FAILED_STATE_STATUS) {
        let summary = value
            .get("bail_reason")
            .and_then(Value::as_str)
            .unwrap_or("(no bail_reason in state file)")
            .to_string();
        return Err(IntegrateError::ReconcileFailed {
            repo: outcome.repo.clone(),
            block_id: outcome.block_id.clone(),
            path,
            summary,
        });
    }
    if status != Some(EXPECTED_STATE_STATUS) {
        return Err(IntegrateError::StateWriteMismatch {
            repo: outcome.repo.clone(),
            block_id: outcome.block_id.clone(),
            path,
            found: status.map(str::to_string),
        });
    }
    if let Some(found_block_id) = value.get("block_id").and_then(Value::as_str) {
        if found_block_id != outcome.block_id {
            return Err(IntegrateError::BlockIdMismatch {
                repo: outcome.repo.clone(),
                block_id: outcome.block_id.clone(),
                path,
                found: found_block_id.to_string(),
            });
        }
    }
    if let Some(final_validation) = value.get("final_validation") {
        if !final_validation.is_null() {
            let all_passed = final_validation.get("all_passed").and_then(Value::as_bool);
            if all_passed == Some(false) {
                let failure_summary = final_validation
                    .get("failure_summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(no failure_summary in state file)")
                    .to_string();
                return Err(IntegrateError::FinalValidationGateFailed {
                    repo: outcome.repo.clone(),
                    block_id: outcome.block_id.clone(),
                    path,
                    failure_summary,
                });
            }
        }
    }
    Ok(())
}

// ── Roadmap directory resolution (begin-orchestration Step 1C) ─────────

/// Resolve a roadmap's directory under `planning_root` by
/// `/begin-orchestration`'s Step 1C rule: `planning/roadmaps/<slug>/`
/// first; otherwise legacy `planning/<slug>/`; a slug present in **both**
/// locations is an error, never a silent preference for one over the
/// other — "an ambiguous roadmap is how a lane appends to the wrong lane
/// log."
pub fn resolve_roadmap_dir(planning_root: &Path, slug: &str) -> Result<PathBuf, IntegrateError> {
    let new_location = planning_root.join("roadmaps").join(slug);
    let legacy_location = planning_root.join(slug);
    let new_exists = new_location.is_dir();
    let legacy_exists = legacy_location.is_dir();
    match (new_exists, legacy_exists) {
        (true, true) => Err(IntegrateError::AmbiguousRoadmapDir {
            slug: slug.to_string(),
            new_location,
            legacy_location,
        }),
        (true, false) => Ok(new_location),
        (false, true) => Ok(legacy_location),
        (false, false) => Err(IntegrateError::RoadmapDirNotFound {
            slug: slug.to_string(),
            new_location,
            legacy_location,
        }),
    }
}

// ── Lane-log append ─────────────────────────────────────────────────────

/// One `lane-log.jsonl` line — the cross-lane channel a sibling lane reads
/// to learn what this lane has already integrated. Field names AND field
/// order are the durable on-disk contract other repos' agents parse:
/// `{ts, lane, repo, block, status, note}`, exactly and in that order,
/// since the file is read by humans as often as by machines. See
/// `scripts/roadmap_status_discovery.py`'s `read_lane_log`.
///
/// `ts` is `DateTime<FixedOffset>` rather than `DateTime<Utc>` so that a
/// fixture line written with a non-UTC offset (e.g. `-03:00`, as most of
/// the fleet's hand-written lines are) round-trips byte-for-byte through
/// serde instead of being silently normalized to `Z` — the offset is part
/// of what is on disk, not lost information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneLogEntry {
    pub ts: DateTime<FixedOffset>,
    pub lane: String,
    pub repo: String,
    pub block: String,
    pub status: LaneLogStatus,
    pub note: String,
}

/// The closed vocabulary of outcomes a lane-log line can record. No
/// string-typed escape variant: this type is only ever constructed by
/// this crate, so an unknown status is a bug here, not data to carry
/// through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneLogStatus {
    Closed,
    Bailed,
    Held,
    /// `EN.11.F` task 4: the chain stopped at a block boundary because a
    /// cancellation was observed — distinct from [`LaneLogStatus::Bailed`]
    /// (a failure) precisely because nothing failed: the in-flight block
    /// (if any) already finished and committed, and only the NEXT block —
    /// the one this line names — never started. Fork 1's decided
    /// semantics (halt at the next block boundary, never mid-block).
    Cancelled,
    /// `EN.11.F` task 4: the chain stopped at a block boundary because the
    /// campaign-scoped [`CampaignLedger`] ceiling tripped (`budget.rs`
    /// task 1) — distinct from both [`LaneLogStatus::Bailed`] (a failure)
    /// and [`LaneLogStatus::Cancelled`] (an explicit human request): a
    /// budget halt is neither, it is the ceiling doing its job. The block
    /// this line names is the one that never started.
    BudgetHalted,
}

impl LaneLogEntry {
    /// A block that finished successfully.
    #[must_use]
    pub fn closed(outcome: &ExecutionOutcome, lane: &str, note: impl Into<String>) -> Self {
        Self {
            ts: Utc::now().into(),
            lane: lane.to_string(),
            repo: outcome.repo.clone(),
            block: outcome.block_id.clone(),
            status: LaneLogStatus::Closed,
            note: note.into(),
        }
    }

    /// A block whose step failed before it could complete — recorded so a
    /// sibling lane sees the attempt rather than silence.
    #[must_use]
    pub fn bailed(step: &ChainStep, lane: &str, note: impl Into<String>) -> Self {
        Self {
            ts: Utc::now().into(),
            lane: lane.to_string(),
            repo: step.repo.clone(),
            block: step.block_id.clone(),
            status: LaneLogStatus::Bailed,
            note: note.into(),
        }
    }

    /// The chain stopped at a block boundary on an observed cancellation.
    /// `step` is the block that never started — every block before it in
    /// the chain already has its own `closed` line on disk, untouched by
    /// this entry (`EN.11.F` task 4: an abort never discards or rewrites
    /// in-flight/completed work).
    #[must_use]
    pub fn cancelled(step: &ChainStep, lane: &str, note: impl Into<String>) -> Self {
        Self {
            ts: Utc::now().into(),
            lane: lane.to_string(),
            repo: step.repo.clone(),
            block: step.block_id.clone(),
            status: LaneLogStatus::Cancelled,
            note: note.into(),
        }
    }

    /// The chain stopped at a block boundary because the campaign budget
    /// ceiling tripped. `step` is the block that never started; `reason`
    /// names the cap, its limit, and the spend that tripped it
    /// ([`BudgetHaltReason::cap_name`]) so the recorded note is
    /// self-explanatory without re-deriving the ledger state.
    #[must_use]
    pub fn budget_halted(step: &ChainStep, lane: &str, reason: BudgetHaltReason) -> Self {
        Self {
            ts: Utc::now().into(),
            lane: lane.to_string(),
            repo: step.repo.clone(),
            block: step.block_id.clone(),
            status: LaneLogStatus::BudgetHalted,
            note: format!(
                "campaign budget ceiling tripped before block {} started: {}",
                step.block_id,
                reason.to_json()
            ),
        }
    }
}

/// Append exactly one JSON line for `entry` to `roadmap_dir/lane-log.jsonl`,
/// creating the file if it does not yet exist. Opens in append mode only —
/// this function never reads, rewrites, or truncates any existing line, so
/// concurrent lanes appending to the same file never clobber each other's
/// entries.
pub fn append_lane_log_line(
    roadmap_dir: &Path,
    entry: &LaneLogEntry,
) -> Result<(), IntegrateError> {
    let path = roadmap_dir.join("lane-log.jsonl");
    let line = serde_json::to_string(entry).map_err(|source| IntegrateError::LaneLogSerialize {
        block_id: entry.block.clone(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| IntegrateError::LaneLogWriteFailed {
            path: path.clone(),
            source,
        })?;
    writeln!(file, "{line}").map_err(|source| IntegrateError::LaneLogWriteFailed { path, source })
}

// ── Errors ───────────────────────────────────────────────────────────────

/// Everything that can go wrong integrating one block or resolving where
/// its lane-log line belongs. Every variant names the block/repo/path
/// involved — this module never fails silently, matching its
/// `chain`/`gates`/`execute` sibling modules.
#[derive(Debug)]
pub enum IntegrateError {
    /// A dependency edge was unmet (from [`check_dependencies`]).
    Gate(GateError),
    /// The block's `SDLC_FLOW` run itself failed (from [`execute_step`]).
    Execute(ExecuteError),
    /// The block's `sdlc-flow-state.json` could not be read at all.
    StateWriteUnreadable {
        repo: String,
        block_id: String,
        path: PathBuf,
        source: std::io::Error,
    },
    /// The block's `sdlc-flow-state.json` was read but did not parse as
    /// JSON.
    StateWriteMalformed {
        repo: String,
        block_id: String,
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The block's `sdlc-flow-state.json` parsed but `"status"` was not
    /// `"done"` — the state write does not match the run that supposedly
    /// completed it.
    StateWriteMismatch {
        repo: String,
        block_id: String,
        path: PathBuf,
        found: Option<String>,
    },
    /// The block's `sdlc-flow-state.json` parsed and `"status"` read
    /// `"done"`, but `final_validation.all_passed` read `false` — the run
    /// finished, but on a red build. Deliberately distinct from
    /// [`IntegrateError::StateWriteMismatch`]: that variant says the run
    /// did not finish; this one says it finished with a failing full-suite
    /// gate, and an operator reading a stopped chain needs to know which.
    FinalValidationGateFailed {
        repo: String,
        block_id: String,
        path: PathBuf,
        failure_summary: String,
    },
    /// The block's `sdlc-flow-state.json` parsed, `"status"` read `"done"`,
    /// and the file carried a non-null `"block_id"` — but that `block_id`
    /// disagreed with `outcome.block_id`, the block this integration run
    /// actually executed. Closes a real gap: without this check, a stale
    /// state file left behind in `planning/{block_id}/sdlc/` by an
    /// earlier, *different* run (e.g. a spec directory reused across
    /// blocks, or a leftover file from a prior chain attempt) would be
    /// silently admitted as this block's result merely because `"status"`
    /// read `"done"` at the expected path. A state file with no
    /// `"block_id"` at all (an older run, or one written by the JS
    /// `/sdlc-flow`, which does not carry this field) still passes —
    /// this check only fires on an actual disagreement, never on absence.
    BlockIdMismatch {
        repo: String,
        block_id: String,
        path: PathBuf,
        found: String,
    },
    /// The block's state file parsed with `"status"` reading
    /// `"reconcile_failed"` — the run's `TerminalSignal::ReconcileFailed`
    /// path (`sdlc_flow::close_block`'s / `sdlc_task::lean_bookkeep`'s
    /// shared skip check), which deliberately SKIPS the block-status flip
    /// because the block is genuinely not closed. TERMINAL: this STOPS
    /// the chain (Fork 5) rather than being treated as a soft warning or
    /// folded into [`IntegrateError::StateWriteMismatch`] — a specific
    /// diagnostic naming the reconcile failure is the whole point.
    ReconcileFailed {
        repo: String,
        block_id: String,
        path: PathBuf,
        summary: String,
    },
    /// A `lane-log.jsonl` entry could not be serialized.
    LaneLogSerialize {
        block_id: String,
        source: serde_json::Error,
    },
    /// `lane-log.jsonl` could not be opened or written.
    LaneLogWriteFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Neither `planning/roadmaps/<slug>/` nor legacy `planning/<slug>/`
    /// exists.
    RoadmapDirNotFound {
        slug: String,
        new_location: PathBuf,
        legacy_location: PathBuf,
    },
    /// `slug` exists in **both** `planning/roadmaps/<slug>/` and legacy
    /// `planning/<slug>/` — resolved as an error, never a silent
    /// preference.
    AmbiguousRoadmapDir {
        slug: String,
        new_location: PathBuf,
        legacy_location: PathBuf,
    },
    /// EN.11.L Task 2: [`wait_for_clearance`] exceeded its `deadline`
    /// while `(repo, block_id)` was still under an operator hold nobody
    /// has answered. Reported loudly — never a silent forever-wait — and
    /// names both the held block and its repo so whoever reads this stop
    /// knows exactly where to look in the operator queue.
    HoldDeadlineExceeded {
        repo: String,
        block_id: String,
        deadline: Duration,
    },
    /// `EN.11.H` task 2: the checkpoint could not be written on the
    /// success path — a real, actionable disk problem (see
    /// [`step_branch_name`]'s call site) rather than a chain already
    /// unwinding, so this one propagates instead of being swallowed like
    /// the bail/cancel/budget paths' best-effort writes.
    CheckpointWriteFailed(super::checkpoint::CheckpointError),
}

impl fmt::Display for IntegrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntegrateError::Gate(err) => write!(f, "{err}"),
            IntegrateError::Execute(err) => write!(f, "{err}"),
            IntegrateError::StateWriteUnreadable {
                repo,
                block_id,
                path,
                source,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') state write verification failed: \
                 could not read {}: {source}",
                path.display()
            ),
            IntegrateError::StateWriteMalformed {
                repo,
                block_id,
                path,
                source,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') state write verification failed: \
                 malformed JSON at {}: {source}",
                path.display()
            ),
            IntegrateError::StateWriteMismatch {
                repo,
                block_id,
                path,
                found,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') state write verification failed: \
                 expected \"status\": \"done\" at {}, found {:?}",
                path.display(),
                found
            ),
            IntegrateError::FinalValidationGateFailed {
                repo,
                block_id,
                path,
                failure_summary,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') state write verification failed: \
                 \"status\": \"done\" at {} but final_validation.all_passed was false: {failure_summary}",
                path.display()
            ),
            IntegrateError::BlockIdMismatch {
                repo,
                block_id,
                path,
                found,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') state write verification failed: \
                 state file at {} carries block_id '{found}', which disagrees with the \
                 executed block '{block_id}'",
                path.display()
            ),
            IntegrateError::ReconcileFailed {
                repo,
                block_id,
                path,
                summary,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') reconcile failed and is NOT closed \
                 (state file at {} reads \"status\": \"reconcile_failed\"): {summary}",
                path.display()
            ),
            IntegrateError::LaneLogSerialize { block_id, source } => write!(
                f,
                "lane-log entry for block '{block_id}' failed to serialize: {source}"
            ),
            IntegrateError::LaneLogWriteFailed { path, source } => {
                write!(f, "failed to append to {}: {source}", path.display())
            }
            IntegrateError::RoadmapDirNotFound {
                slug,
                new_location,
                legacy_location,
            } => write!(
                f,
                "roadmap '{slug}' not found: neither {} nor {} exists",
                new_location.display(),
                legacy_location.display()
            ),
            IntegrateError::AmbiguousRoadmapDir {
                slug,
                new_location,
                legacy_location,
            } => write!(
                f,
                "roadmap '{slug}' is ambiguous: it exists at both {} and {}",
                new_location.display(),
                legacy_location.display()
            ),
            IntegrateError::HoldDeadlineExceeded {
                repo,
                block_id,
                deadline,
            } => write!(
                f,
                "block '{block_id}' (repo '{repo}') operator hold deadline exceeded: \
                 still held after {deadline:?} with no clearance — check the operator \
                 queue for this block"
            ),
            IntegrateError::CheckpointWriteFailed(source) => {
                write!(f, "failed to write checkpoint: {source}")
            }
        }
    }
}

impl std::error::Error for IntegrateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IntegrateError::Gate(err) => Some(err),
            IntegrateError::Execute(err) => Some(err),
            IntegrateError::StateWriteUnreadable { source, .. } => Some(source),
            IntegrateError::StateWriteMalformed { source, .. } => Some(source),
            IntegrateError::LaneLogSerialize { source, .. } => Some(source),
            IntegrateError::LaneLogWriteFailed { source, .. } => Some(source),
            IntegrateError::StateWriteMismatch { .. }
            | IntegrateError::FinalValidationGateFailed { .. }
            | IntegrateError::BlockIdMismatch { .. }
            | IntegrateError::ReconcileFailed { .. }
            | IntegrateError::RoadmapDirNotFound { .. }
            | IntegrateError::AmbiguousRoadmapDir { .. }
            | IntegrateError::HoldDeadlineExceeded { .. } => None,
            IntegrateError::CheckpointWriteFailed(source) => Some(source),
        }
    }
}

impl From<GateError> for IntegrateError {
    fn from(err: GateError) -> Self {
        IntegrateError::Gate(err)
    }
}

impl From<ExecuteError> for IntegrateError {
    fn from(err: ExecuteError) -> Self {
        IntegrateError::Execute(err)
    }
}

impl From<super::checkpoint::CheckpointError> for IntegrateError {
    fn from(err: super::checkpoint::CheckpointError) -> Self {
        IntegrateError::CheckpointWriteFailed(err)
    }
}

// ── Per-step progress ────────────────────────────────────────────────────

/// One completed step's progress, handed to the injected step observer
/// (`Task 3`) exactly once per integrated step — after that step's
/// `lane-log.jsonl` line is appended, so an observed step is always a
/// recorded step.
///
/// `index` is **1-based**: the first completed step reports `index: 1`,
/// matching the human "4 of 30" phrasing a watcher reads it as, rather
/// than the 0-based offset into `chain`. `total` is the chain's full
/// length, resolved once before the loop starts (not the number of steps
/// integrated so far), so a watcher can always tell `index` of `total`
/// (e.g. 4-of-30 from 29-of-30) regardless of whether the chain later
/// stops early.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepProgress {
    pub repo: String,
    pub block_id: String,
    /// 1-based position of this completed step in the chain.
    pub index: usize,
    /// The chain's total step count.
    pub total: usize,
    /// The step's outcome status. Only ever `"completed"` today — the
    /// observer is called exactly once per *completed* step (see this
    /// struct's doc); a step that bails returns an `Err` from
    /// [`integrate_chain`] before reaching the observer call, and a
    /// cancelled chain simply stops calling it, so there is no "failed" or
    /// "cancelled" status to carry here as of Task 3. Kept as an owned
    /// `String` rather than a fixed enum so a later status does not need a
    /// struct-shape change (CLAUDE.md standing rule 6: keep the shape
    /// invariant).
    pub status: String,
}

/// An injected observer called once per completed step, wired by
/// `OrchestrationRunNode::with_step_observer`. `Node::process` has no
/// access to `on_progress` — it is framework-owned and only fires at node
/// boundaries — so per-step progress for a single-node chain like this one
/// can only ever be an explicitly injected seam, never a call into the
/// framework. `Send + Sync` because it crosses the `spawn_blocking`
/// boundary alongside every other closure this node owns.
pub type StepObserverFn = dyn Fn(&StepProgress) + Send + Sync;

// ── The integrated loop ─────────────────────────────────────────────────

/// Drive `chain` to completion, in order: for every step, gate on
/// dependencies, wait out any operator hold, gate on admission (EN.11.L —
/// the permit is scoped to the EXECUTION window only, acquired AFTER the
/// hold clears so a parked step never consumes a concurrency slot),
/// execute via `SDLC_FLOW`, verify the state write, and append exactly one
/// `lane-log.jsonl` line — then move on. Returns every step's
/// [`ExecutionOutcome`] in order, or the first [`IntegrateError`]
/// encountered (a chain stops at the first failing block; nothing after
/// it runs).
///
/// Resumability is structural rather than a separate code path: because
/// this is one continuous sequential loop, a hold's `.await` inside
/// [`wait_for_clearance`] simply suspends the loop at its current
/// position — every step already integrated stays integrated (its
/// lane-log line is already on disk), and the loop resumes at exactly the
/// held step the moment [`HoldSource::is_held`] reports clear, never
/// re-visiting an earlier step.
///
/// `lane` is the ORCHESTRATION event's real lane name for a roadmap+lane
/// chain. An explicit `blocks` chain has no lane by construction (there is
/// no lane file to have parsed one from) — pass `None` and each step's
/// lane-log line falls back to that step's own repo slug, matching how the
/// fleet's hand-written lines already read `lane == repo` for a
/// single-repo lane. The fallback is resolved per step (not once for the
/// whole chain) so a mixed-repo explicit chain still gets a truthful lane
/// per line rather than one step's repo borrowed for another's.
///
/// A step that fails — either [`execute_step`] itself or
/// [`verify_state_write`] afterwards — still gets exactly one `bailed`
/// line recorded before the error propagates, so a sibling lane sees the
/// attempt rather than silence. If the bailed append itself fails, that
/// append failure is swallowed and the *original* step error is what
/// returns — a lane-log write hiccup must never replace the real reason
/// the chain stopped.
///
/// # Cancellation stops the chain BETWEEN steps only
///
/// `cancellation_token`, when given, is checked at the top of every loop
/// iteration (before [`check_dependencies`]) and raced against
/// [`wait_for_clearance`]'s sleep, so a chain parked on an operator hold
/// aborts within one poll tick rather than only at the next iteration's
/// top. A win is **not** an error: the loop simply stops and this function
/// returns `Ok` with whatever outcomes were already integrated — nothing
/// is rolled back, and every step that already ran has its `lane-log.jsonl`
/// line on disk exactly as if the chain had finished normally. The caller
/// (`OrchestrationRunNode::process`) is what stamps the fact that a
/// cancellation happened.
///
/// This does **not** reach inside a step already in flight: a step whose
/// [`execute_step`] call (the child `SDLC_FLOW` run) has already started
/// runs to completion regardless of the token — there is no run id to
/// thread a cancellation into yet. Cancellation here only ever prevents
/// the *next* step from starting.
///
/// A cancellation win is also recorded as one [`LaneLogEntry::cancelled`]
/// line, naming the block that never started — the chain's own record
/// (`lane-log.jsonl`) then carries three mutually exclusive terminal
/// vocabularies for how it can end: `closed`/`bailed` lines for every step
/// that ran, one `cancelled` line if the chain stopped on cancellation, or
/// one `budget_halted` line if the campaign ceiling tripped first (see
/// below) — never more than one of the latter two, since both `break` the
/// loop immediately.
///
/// # The campaign budget ceiling (`EN.11.F` task 1 + task 4)
///
/// `campaign_budget`, when given, is checked at the SAME block boundary as
/// `cancellation_token` — the top of every loop iteration, before that
/// iteration's step is dispatched — via a [`CampaignLedger`] that
/// accumulates each completed step's `cost_usd`/`total_tokens`
/// ([`ExecutionOutcome::cost_usd`]/[`ExecutionOutcome::total_tokens`]).
/// This is checked EVERY boundary, not once per campaign: a two-step chain
/// checks it twice (once trivially, before step 1, with zero accumulated
/// spend; once before step 2, with step 1's spend folded in), which is
/// exactly what lets a cap set below one block's cost halt at the FIRST
/// boundary rather than only after the whole chain. A trip halts the chain
/// exactly like a cancellation win — `Ok` with whatever already
/// integrated, nothing rolled back — but is recorded as a distinct
/// [`LaneLogEntry::budget_halted`] line naming the tripped cap
/// ([`BudgetHaltReason::cap_name`]), so a budget halt is never
/// indistinguishable from an operator's own abort request in the chain's
/// record. `None` (the built-in default) means no ceiling at all —
/// behavior-identical to before this parameter existed.
///
/// # Per-step progress
///
/// `step_observer` is called **exactly once per completed step**,
/// immediately after that step's `lane-log.jsonl` line is appended (so an
/// observed step is always a recorded step) — never before, and never for
/// a step that bails. See [`StepProgress`] for the payload and its 1-based
/// index convention. A no-op observer (e.g. `&|_| {}`) makes this
/// parameter behavior-stable: nothing about the loop's control flow or
/// return value depends on it.
#[allow(clippy::too_many_arguments)]
pub async fn integrate_chain(
    chain: &[ChainStep],
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
    admission: &AdmissionGate,
    hold_source: &dyn HoldSource,
    poll_interval: Duration,
    hold_deadline: Option<Duration>,
    cancellation_token: Option<&crate::cancellation::CancellationToken>,
    campaign_budget: Option<&Budget>,
    resolve_engine: &dyn Fn(&str, &str) -> EngineKind,
    registry: &RepoRegistry,
    run_flow: &FlowRunner,
    roadmap_dir: &Path,
    lane: Option<&str>,
    step_observer: &StepObserverFn,
    default_use_worktree: bool,
    campaign_id: uuid::Uuid,
) -> Result<Vec<ExecutionOutcome>, IntegrateError> {
    let total_steps = chain.len();
    let mut outcomes = Vec::with_capacity(chain.len());
    // `EN.11.F` task 1/4: accumulates each completed step's spend across
    // the whole campaign, checked at every block boundary below —
    // distinct from `execute_step`'s own per-NODE `BudgetLedger` inside a
    // single child run.
    let mut campaign_ledger = CampaignLedger::new();
    // `EN.11.H` task 2: the per-chain checkpoint this run is extending —
    // resumed from whatever's already on disk for this `campaign_id`
    // (e.g. a resumed campaign re-entering `integrate_chain` for its
    // remaining steps), or a fresh, empty one for a first run. A read
    // failure (a corrupt file, not a missing one — see
    // `checkpoint::read_checkpoint`) is treated the same as "start fresh"
    // here: this loop's job is to keep integrating, not to diagnose a
    // torn checkpoint, and every write below re-derives the full state
    // from `outcomes` as it goes.
    let mut checkpoint = read_checkpoint(roadmap_dir, campaign_id)
        .ok()
        .and_then(ReadCheckpoint::into_option)
        .unwrap_or_else(|| Checkpoint::new(campaign_id));
    for step in chain {
        // Checked at the top of every iteration, before anything for this
        // step starts — see the "Cancellation stops the chain BETWEEN
        // steps only" section above.
        if cancellation_token.is_some_and(|t| t.is_cancelled()) {
            let entry = LaneLogEntry::cancelled(
                step,
                lane.unwrap_or(step.repo.as_str()),
                format!(
                    "campaign cancelled at the block boundary before {} started; \
                     every earlier block in this chain already finished and committed",
                    step.block_id
                ),
            );
            let _ = append_lane_log_line(roadmap_dir, &entry);
            let _ = write_checkpoint(roadmap_dir, &checkpoint);
            break;
        }

        // Checked at the SAME boundary as cancellation, every iteration —
        // see the "campaign budget ceiling" doc section above. A trip
        // halts the chain exactly like a cancellation win, but is recorded
        // under its own `budget_halted` status so the two stay
        // distinguishable in the chain's record.
        if let BudgetDecision::Halt(reason) = campaign_ledger.check(campaign_budget) {
            let entry =
                LaneLogEntry::budget_halted(step, lane.unwrap_or(step.repo.as_str()), reason);
            let _ = append_lane_log_line(roadmap_dir, &entry);
            let _ = write_checkpoint(roadmap_dir, &checkpoint);
            break;
        }

        check_dependencies(step, resolve_depends_on, is_edge_met)?;

        // Wait out any operator hold BEFORE touching admission at all —
        // EN.11.L Task 1. The admission permit must be scoped to the
        // EXECUTION window only, never held across an unbounded hold: a
        // step parked here holds no semaphore slot, so every other lane
        // at the ceiling can still start while this one waits on a human.
        //
        // EN.11.L Task 2: `hold_deadline` bounds the wait — a hold nobody
        // is answering now fails the chain loudly instead of parking it
        // forever. A deadline exceeded is reported exactly like any other
        // step failure: one `bailed` lane-log line, then propagate.
        if let Err(err) = wait_for_clearance(
            hold_source,
            &step.repo,
            &step.block_id,
            poll_interval,
            hold_deadline,
            cancellation_token,
        )
        .await
        {
            let entry =
                LaneLogEntry::bailed(step, lane.unwrap_or(step.repo.as_str()), err.to_string());
            let _ = append_lane_log_line(roadmap_dir, &entry);
            let _ = write_checkpoint(roadmap_dir, &checkpoint);
            return Err(err);
        }

        // `wait_for_clearance` can return early on a cancel win (rather
        // than because the hold actually cleared) — re-check here so a
        // cancelled, held chain does not go on to acquire a permit or
        // execute the step it was waiting on.
        if cancellation_token.is_some_and(|t| t.is_cancelled()) {
            let entry = LaneLogEntry::cancelled(
                step,
                lane.unwrap_or(step.repo.as_str()),
                format!(
                    "campaign cancelled while {} was parked on an operator hold; \
                     every earlier block in this chain already finished and committed",
                    step.block_id
                ),
            );
            let _ = append_lane_log_line(roadmap_dir, &entry);
            let _ = write_checkpoint(roadmap_dir, &checkpoint);
            break;
        }

        // Ordering guarantee: `acquire_for` is called AFTER clearance, so
        // a step that just cleared its hold joins the admission
        // semaphore's own FIFO wait queue exactly like any other
        // newly-ready step — it never jumps ahead of a lane that was
        // already queued for a permit before this hold cleared. Nothing
        // here re-orders relative to other lanes; each lane still only
        // ever acquires one permit, once, for its own step.
        let _permit = admission.acquire_for(step).await;

        let step_lane = lane.unwrap_or(step.repo.as_str());

        // `default_use_worktree` is the resolved `OrchestrationPolicy
        // ::default_use_worktree` fallback, threaded in from
        // `OrchestrationRunNode::process` — the row-3 case of
        // `execute::resolve_isolation`'s table. Rows 1/2 (base-template
        // always worktree, the brain root never) are resolved inside
        // `execute_step` itself and are unreachable from this value.
        // Cancellation/budget wiring at this call site is `EN.11.F` task
        // 4's job (the block-boundary check that decides whether to keep
        // dispatching steps at all); this task (task 3) only makes
        // `execute_step`'s signature able to carry them. `None, None` here
        // is mechanical — behavior-identical to before this task, since
        // `RunOptions::default()` was always the child's config.
        let outcome = match execute_step(
            step,
            resolve_engine,
            registry,
            run_flow,
            default_use_worktree,
            campaign_id,
            None,
            None,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                let integrate_err = IntegrateError::from(err);
                let entry = LaneLogEntry::bailed(step, step_lane, integrate_err.to_string());
                let _ = append_lane_log_line(roadmap_dir, &entry);
                let _ = write_checkpoint(roadmap_dir, &checkpoint);
                return Err(integrate_err);
            }
        };

        if let Err(err) = verify_state_write(&outcome) {
            let entry = LaneLogEntry::bailed(step, step_lane, err.to_string());
            let _ = append_lane_log_line(roadmap_dir, &entry);
            let _ = write_checkpoint(roadmap_dir, &checkpoint);
            return Err(err);
        }

        let entry = LaneLogEntry::closed(
            &outcome,
            step_lane,
            format!("block {} closed via SDLC_FLOW", step.block_id),
        );
        append_lane_log_line(roadmap_dir, &entry)?;

        // `EN.11.H` task 2: checkpoint this step as integrated, AFTER the
        // lane-log line — a crash between the two leaves the lane log
        // ahead of the checkpoint, never the other way round. Resume
        // (`EN.11.H` task 4) tolerates a lane-log line for a block its
        // checkpoint doesn't yet know about (it de-duplicates from the
        // checkpoint, so a "duplicate-looking" line it never re-appends),
        // but must never SKIP a block whose checkpoint says "integrated"
        // when the lane log actually never recorded it. Propagated with
        // `?` (unlike the ignored writes on the bail/cancel/budget paths
        // above) because a checkpoint write failure here — on an
        // otherwise-successful step, against a `roadmap_dir` that just
        // accepted a lane-log append — signals a real, actionable disk
        // problem rather than the "chain is already unwinding" case those
        // paths guard.
        checkpoint.steps.push(CheckpointStep {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            index: outcomes.len() as u32 + 1,
            integrated: true,
            branch: step_branch_name(&outcome.ctx),
        });
        write_checkpoint(roadmap_dir, &checkpoint)?;

        // Fold this step's spend into the campaign ledger BEFORE the next
        // iteration's boundary check — `EN.11.F` task 1's documented
        // `None`-cost rule applies unchanged here (a step with no cost
        // figure contributes tokens but never a silent `$0`).
        campaign_ledger.record_step(outcome.cost_usd, outcome.total_tokens);

        outcomes.push(outcome);

        // Called exactly once per completed step, after this step's
        // lane-log line is on disk — see `integrate_chain`'s "Per-step
        // progress" doc. `index` is 1-based (`outcomes.len()` is already
        // the count including the step just pushed).
        step_observer(&StepProgress {
            repo: step.repo.clone(),
            block_id: step.block_id.clone(),
            index: outcomes.len(),
            total: total_steps,
            status: "completed".to_string(),
        });
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
            ..Default::default()
        }
    }

    fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
        )
        .unwrap();
        let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation| {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            Box::pin(async {
                Ok(engine_contract::TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        (runner, calls)
    }

    fn write_done_state(repo_path: &Path, block_id: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({"status": "done"}).to_string(),
        )
        .unwrap();
    }

    fn write_corrupted_state(repo_path: &Path, block_id: &str, status: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({"status": status}).to_string(),
        )
        .unwrap();
    }

    /// Writes a `sdlc-task-state.json` (the task engine's own filename,
    /// never `sdlc-flow-state.json`) with the given `status`.
    fn write_task_state(repo_path: &Path, block_id: &str, status: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-task-state.json"),
            json!({"status": status}).to_string(),
        )
        .unwrap();
    }

    fn outcome_with_engine(
        repo_path: &Path,
        block_id: &str,
        engine: EngineKind,
    ) -> ExecutionOutcome {
        ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: repo_path.to_path_buf(),
            block_id: block_id.into(),
            engine,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        }
    }

    // ── State-write verification ────────────────────────────────────────

    #[test]
    fn state_path_for_selects_the_flow_filename_for_the_flow_engine() {
        let path = state_path_for(Path::new("/repo"), "A.1", EngineKind::Flow);
        assert_eq!(
            path,
            Path::new("/repo/planning/A.1/sdlc/sdlc-flow-state.json")
        );
    }

    #[test]
    fn state_path_for_selects_the_task_filename_for_the_task_engine() {
        let path = state_path_for(Path::new("/repo"), "A.1", EngineKind::Task);
        assert_eq!(
            path,
            Path::new("/repo/planning/A.1/sdlc/sdlc-task-state.json")
        );
    }

    #[test]
    fn a_task_state_file_reading_done_integrates_exactly_like_flow() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state(dir.path(), "A.1", "done");
        let outcome = outcome_with_engine(dir.path(), "A.1", EngineKind::Task);
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn a_task_state_file_reading_reconcile_failed_surfaces_reconcile_failed_not_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state(dir.path(), "A.1", "reconcile_failed");
        let outcome = outcome_with_engine(dir.path(), "A.1", EngineKind::Task);
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(
            matches!(err, IntegrateError::ReconcileFailed { .. }),
            "expected ReconcileFailed, got {err:?}"
        );
        // Never the generic mismatch — a specific diagnostic is the point.
        assert!(!matches!(err, IntegrateError::StateWriteMismatch { .. }));
    }

    #[test]
    fn a_flow_state_file_reading_reconcile_failed_also_surfaces_reconcile_failed() {
        let dir = tempfile::tempdir().unwrap();
        write_corrupted_state(dir.path(), "A.1", "reconcile_failed");
        let outcome = outcome_with_engine(dir.path(), "A.1", EngineKind::Flow);
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(matches!(err, IntegrateError::ReconcileFailed { .. }));
    }

    #[test]
    fn state_write_verification_passes_when_status_is_done() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state(dir.path(), "A.1");
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn a_deliberately_corrupted_state_write_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        write_corrupted_state(dir.path(), "A.1", "in_progress");
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(matches!(err, IntegrateError::StateWriteMismatch { .. }));
        let msg = err.to_string();
        assert!(msg.contains("A.1"));
        assert!(msg.contains("repo-a"));
    }

    #[test]
    fn a_missing_state_file_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(matches!(err, IntegrateError::StateWriteUnreadable { .. }));
    }

    fn write_done_state_with_block_id(repo_path: &Path, block_id: &str, found_block_id: Value) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({"status": "done", "block_id": found_block_id}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn state_write_verification_rejects_a_disagreeing_block_id() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_block_id(dir.path(), "A.1", json!("B.9"));
        let outcome = outcome_for(dir.path(), "A.1");
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(matches!(err, IntegrateError::BlockIdMismatch { .. }));
        let msg = err.to_string();
        assert!(msg.contains("A.1"));
        assert!(msg.contains("B.9"));
    }

    #[test]
    fn state_write_verification_accepts_an_agreeing_block_id() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_block_id(dir.path(), "A.1", json!("A.1"));
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn state_write_verification_accepts_a_null_block_id() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_block_id(dir.path(), "A.1", Value::Null);
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn state_write_verification_accepts_a_state_file_with_no_block_id_key() {
        // A pre-task-1 state file, or one written by the JS `/sdlc-flow` —
        // the "block_id" key is absent entirely, not merely null.
        // `write_done_state` produces exactly this shape.
        let dir = tempfile::tempdir().unwrap();
        write_done_state(dir.path(), "A.1");
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    fn write_done_state_with_final_validation(
        repo_path: &Path,
        block_id: &str,
        final_validation: Value,
    ) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({"status": "done", "final_validation": final_validation}).to_string(),
        )
        .unwrap();
    }

    fn outcome_for(repo_path: &Path, block_id: &str) -> ExecutionOutcome {
        ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: repo_path.to_path_buf(),
            block_id: block_id.into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        }
    }

    #[test]
    fn state_write_verification_rejects_done_status_with_failed_final_validation_gate() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_final_validation(
            dir.path(),
            "A.1",
            json!({
                "all_passed": false,
                "check_results": [],
                "failure_summary": "Failed checks: build, clippy",
            }),
        );
        let outcome = outcome_for(dir.path(), "A.1");
        let err = verify_state_write(&outcome).unwrap_err();
        assert!(matches!(
            err,
            IntegrateError::FinalValidationGateFailed { .. }
        ));
        let msg = err.to_string();
        assert!(msg.contains("Failed checks: build, clippy"));
        assert!(msg.contains("A.1"));
    }

    #[test]
    fn state_write_verification_accepts_done_status_with_passing_final_validation_gate() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_final_validation(
            dir.path(),
            "A.1",
            json!({
                "all_passed": true,
                "check_results": [],
                "failure_summary": "",
            }),
        );
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn state_write_verification_accepts_null_final_validation() {
        let dir = tempfile::tempdir().unwrap();
        write_done_state_with_final_validation(dir.path(), "A.1", Value::Null);
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    #[test]
    fn state_write_verification_accepts_state_file_with_no_final_validation_key() {
        // A pre-EN.3.E state file — the "final_validation" key is absent
        // entirely, not merely null. `write_done_state` produces exactly
        // this shape.
        let dir = tempfile::tempdir().unwrap();
        write_done_state(dir.path(), "A.1");
        let outcome = outcome_for(dir.path(), "A.1");
        assert!(verify_state_write(&outcome).is_ok());
    }

    // ── Roadmap directory resolution ────────────────────────────────────

    #[test]
    fn resolves_new_location_when_only_new_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("roadmaps").join("close-the-loop")).unwrap();
        let resolved = resolve_roadmap_dir(dir.path(), "close-the-loop").unwrap();
        assert_eq!(resolved, dir.path().join("roadmaps").join("close-the-loop"));
    }

    #[test]
    fn resolves_legacy_location_when_only_legacy_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("demand-ready")).unwrap();
        let resolved = resolve_roadmap_dir(dir.path(), "demand-ready").unwrap();
        assert_eq!(resolved, dir.path().join("demand-ready"));
    }

    #[test]
    fn both_locations_existing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("roadmaps").join("dup")).unwrap();
        std::fs::create_dir_all(dir.path().join("dup")).unwrap();
        let err = resolve_roadmap_dir(dir.path(), "dup").unwrap_err();
        assert!(matches!(err, IntegrateError::AmbiguousRoadmapDir { .. }));
    }

    #[test]
    fn neither_location_existing_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_roadmap_dir(dir.path(), "nope").unwrap_err();
        assert!(matches!(err, IntegrateError::RoadmapDirNotFound { .. }));
    }

    // ── Lane-log append ──────────────────────────────────────────────────

    #[test]
    fn exactly_one_log_line_lands_per_block() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let entry = LaneLogEntry::closed(&outcome, "repo-a", "closed");
        append_lane_log_line(dir.path(), &entry).unwrap();

        let contents = std::fs::read_to_string(dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["repo"], json!("repo-a"));
        assert_eq!(parsed["block"], json!("A.1"));
    }

    /// Guard against a future field being added and silently breaking
    /// every reader again: the serialized key set must be exactly
    /// `{ts, lane, repo, block, status, note}`, no more, no fewer.
    #[test]
    fn serialized_key_set_is_exactly_the_six_contract_fields() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let entry = LaneLogEntry::closed(&outcome, "repo-a", "closed");
        let value = serde_json::to_value(&entry).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("entry must serialize to a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["block", "lane", "note", "repo", "status", "ts"]);
    }

    /// No constructor can produce an entry without an explicit status —
    /// `closed` and `bailed` are the only ways to build one, and each
    /// bakes in its own [`LaneLogStatus`] variant.
    #[test]
    fn constructors_bake_in_their_status() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let closed_entry = LaneLogEntry::closed(&outcome, "repo-a", "closed");
        assert_eq!(closed_entry.status, LaneLogStatus::Closed);

        let failing_step = step("repo-a", "A.1");
        let bailed_entry = LaneLogEntry::bailed(&failing_step, "repo-a", "boom");
        assert_eq!(bailed_entry.status, LaneLogStatus::Bailed);
    }

    #[test]
    fn a_second_append_adds_a_second_line_without_touching_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let outcome_a = ExecutionOutcome {
            repo: "repo-a".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "A.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        let outcome_b = ExecutionOutcome {
            repo: "repo-b".into(),
            repo_path: dir.path().to_path_buf(),
            block_id: "B.1".into(),
            engine: EngineKind::Flow,
            ctx: engine_contract::TaskContext {
                event: json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: json!({}),
                node_runs: std::collections::HashMap::new(),
            },
            use_worktree: false,
            campaign_id: uuid::Uuid::new_v4(),
            cost_usd: None,
            total_tokens: 0,
        };
        append_lane_log_line(
            dir.path(),
            &LaneLogEntry::closed(&outcome_a, "repo-a", "closed"),
        )
        .unwrap();
        append_lane_log_line(
            dir.path(),
            &LaneLogEntry::closed(&outcome_b, "repo-b", "closed"),
        )
        .unwrap();

        let contents = std::fs::read_to_string(dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(lines[0]).unwrap()["block"],
            json!("A.1")
        );
        assert_eq!(
            serde_json::from_str::<Value>(lines[1]).unwrap()["block"],
            json!("B.1")
        );
    }

    // ── Operator hold: pause-and-resume ─────────────────────────────────

    struct FlagHold {
        held: Arc<AtomicBool>,
    }

    impl HoldSource for FlagHold {
        fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
            self.held.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn a_hold_pauses_and_resumes_without_rerunning_completed_blocks() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");
        let (runner, calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();

        // Held for the SECOND step only — resolved by call count, so the
        // hold is lifted after 5 checks (well after the first step ran).
        let checks = Arc::new(AtomicUsize::new(0));
        struct CountingHold {
            checks: Arc<AtomicUsize>,
        }
        impl HoldSource for CountingHold {
            fn is_held(&self, _repo: &str, block_id: &str) -> bool {
                if block_id != "B.1" {
                    return false;
                }
                let n = self.checks.fetch_add(1, Ordering::SeqCst);
                n < 3
            }
        }
        let hold = CountingHold {
            checks: checks.clone(),
        };

        let chain = vec![step("repo-a", "A.1"), step("repo-b", "B.1")];

        let outcomes = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &hold,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("chain should complete once the hold clears");

        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].block_id, "A.1");
        assert_eq!(outcomes[1].block_id, "B.1");

        // Step A ran exactly once — the hold on B never caused A to be
        // re-executed.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.iter().filter(|b| *b == "A.1").count(), 1);
        assert_eq!(recorded.iter().filter(|b| *b == "B.1").count(), 1);

        // Exactly one lane-log line per block, in order.
        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    /// EN.11.L Task 1: a step parked on an operator hold must not hold
    /// an admission permit — a second lane at the ceiling proceeds while
    /// the first is still parked. Uses a capacity-1 admission gate shared
    /// across two concurrent chains: chain A is held forever, chain B is
    /// never held. Before the reorder in this commit, chain A's `let
    /// _permit = admission.acquire_for(step).await` ran BEFORE
    /// `wait_for_clearance`, so it would have consumed the single permit
    /// and chain B would have hung; this test fails against that ordering
    /// (observed timeout on `chain_b_done.recv()`).
    #[tokio::test]
    async fn a_held_step_does_not_starve_a_second_lane_of_its_admission_permit() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");

        let admission = AdmissionGate::new(crate::nodes::terminal::AdmissionControl::new(
            crate::nodes::terminal::admission::AdmissionPolicy {
                max_concurrent_terminal_runs: 1,
            },
        ));

        let held = Arc::new(AtomicBool::new(true));
        let hold = FlagHold { held: held.clone() };

        let (runner_a, _calls_a) = recording_runner();
        let (runner_b, _calls_b) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let roadmap_dir_a = tempfile::tempdir().unwrap();
        let roadmap_dir_b = tempfile::tempdir().unwrap();

        let chain_a = vec![step("repo-a", "A.1")];
        let chain_b = vec![step("repo-b", "B.1")];

        // Both chains run concurrently on ONE task via `tokio::join!` —
        // no `tokio::spawn` (which would require `Send` futures the test
        // closures do not provide). Cooperative polling within a single
        // task is enough to prove the ordering: chain A parks on its
        // hold, the checker future observes the permit is still free and
        // drives chain B to completion, then releases the hold so chain A
        // can finish too.
        let chain_a_fut = integrate_chain(
            &chain_a,
            &resolve_deps,
            &is_met,
            &admission,
            &hold,
            Duration::from_millis(5),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner_a,
            roadmap_dir_a.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        );

        let checker_fut = async {
            // Give chain A every chance to (wrongly) grab the only permit
            // before it reaches its held step.
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                admission.available_permits(),
                1,
                "a step parked on a hold must not consume an admission permit"
            );

            // Chain B, at the same capacity-1 ceiling, must be able to
            // proceed and finish while chain A is still parked.
            let outcomes_b = tokio::time::timeout(
                Duration::from_millis(500),
                integrate_chain(
                    &chain_b,
                    &resolve_deps,
                    &is_met,
                    &admission,
                    &NeverHeld,
                    Duration::from_millis(5),
                    None,
                    None,
                    None,
                    &resolve_engine,
                    &registry,
                    &runner_b,
                    roadmap_dir_b.path(),
                    None,
                    &|_: &StepProgress| {},
                    false,
                    uuid::Uuid::new_v4(),
                ),
            )
            .await
            .expect("a second lane must proceed while the first is parked on a hold, not hang")
            .expect("chain B should complete");
            assert_eq!(outcomes_b.len(), 1);
            assert_eq!(outcomes_b[0].block_id, "B.1");

            // Now clear the hold so chain A can finish too.
            held.store(false, Ordering::SeqCst);
        };

        let (outcomes_a, ()) = tokio::join!(chain_a_fut, checker_fut);
        let outcomes_a = outcomes_a.expect("chain A should complete once the hold clears");
        assert_eq!(outcomes_a.len(), 1);
        assert_eq!(outcomes_a[0].block_id, "A.1");
    }

    #[tokio::test]
    async fn while_held_the_run_does_not_proceed() {
        let held = Arc::new(AtomicBool::new(true));
        let hold = FlagHold { held: held.clone() };

        let waited = tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_clearance(&hold, "repo-a", "A.1", Duration::from_millis(5), None, None),
        )
        .await;
        assert!(waited.is_err(), "must not clear while still held");

        held.store(false, Ordering::SeqCst);
        let cleared = tokio::time::timeout(
            Duration::from_millis(200),
            wait_for_clearance(&hold, "repo-a", "A.1", Duration::from_millis(5), None, None),
        )
        .await;
        assert!(cleared.is_ok(), "must clear promptly once unheld");
    }

    #[tokio::test]
    async fn never_held_clears_immediately() {
        let cleared = tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_clearance(
                &NeverHeld,
                "repo-a",
                "A.1",
                Duration::from_millis(5),
                None,
                None,
            ),
        )
        .await;
        assert!(cleared.is_ok());
    }

    // ── Lane threading (EN.ticket.lane-log-entry-schema Task 3) ─────────

    fn failing_runner(fail_block: &'static str) -> FlowRunner {
        Arc::new(move |invocation| {
            Box::pin(async move {
                if invocation.block_id == fail_block {
                    Err(crate::WorkflowError::new(format!(
                        "simulated failure for {}",
                        invocation.block_id
                    )))
                } else {
                    Ok(engine_contract::TaskContext {
                        event: json!({}),
                        nodes: std::collections::HashMap::new(),
                        metadata: json!({}),
                        node_runs: std::collections::HashMap::new(),
                    })
                }
            })
        })
    }

    /// An explicit `blocks` chain has no lane by construction: passing
    /// `lane: None` must make every appended line's `lane` fall back to
    /// that step's own repo slug — matching how the fleet's hand-written
    /// single-repo lines already read `lane == repo`.
    #[tokio::test]
    async fn no_lane_falls_back_to_the_step_repo_slug() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        let (runner, _calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();

        let chain = vec![step("repo-a", "A.1")];

        integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("chain should complete");

        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let parsed: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["lane"], json!("repo-a"));
        assert_eq!(parsed["repo"], json!("repo-a"));
        assert_eq!(parsed["status"], json!("closed"));
    }

    /// A real lane, when given, is used as-is rather than falling back to
    /// the repo slug.
    #[tokio::test]
    async fn a_real_lane_is_threaded_into_every_line() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-b"), "B.1");
        let (runner, _calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();

        let chain = vec![step("repo-a", "A.1"), step("repo-b", "B.1")];

        integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            Some("backend"),
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("chain should complete");

        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        for line in contents.lines() {
            let parsed: Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["lane"], json!("backend"));
        }
    }

    /// A failing step appends exactly one `bailed` line carrying the
    /// error's text as its `note`, and the chain still returns that
    /// error.
    #[tokio::test]
    async fn a_failing_step_appends_a_bailed_line_and_still_returns_the_error() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        let runner = failing_runner("A.1");
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();

        let chain = vec![step("repo-a", "A.1")];

        let err = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect_err("a failing step must propagate its error");

        assert!(matches!(err, IntegrateError::Execute(_)));
        let msg = err.to_string();
        assert!(msg.contains("simulated failure"), "message was: {msg}");

        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one bailed line for the attempt");
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["status"], json!("bailed"));
        assert_eq!(parsed["block"], json!("A.1"));
        assert!(
            parsed["note"]
                .as_str()
                .unwrap()
                .contains("simulated failure"),
            "note should carry the error text: {parsed}"
        );
    }

    /// If the bailed append itself fails (here: the roadmap directory does
    /// not exist, so the lane-log append cannot open the file), the
    /// *original* step error is still what returns — a lane-log write
    /// hiccup must never replace the real reason the chain stopped.
    #[tokio::test]
    async fn a_lane_log_append_failure_during_the_bail_path_does_not_mask_the_original_error() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        let runner = failing_runner("A.1");
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        // A roadmap dir that does not exist: `append_lane_log_line` will
        // fail to open the file, since there is no parent directory.
        let missing_roadmap_dir = dir.path().join("no-such-roadmap-dir");

        let chain = vec![step("repo-a", "A.1")];

        let err = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            &missing_roadmap_dir,
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect_err("the original step failure must still surface");

        assert!(
            matches!(err, IntegrateError::Execute(_)),
            "a masked lane-log write failure would surface as LaneLogWriteFailed instead: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("simulated failure"), "message was: {msg}");
        // `EN.11.H` task 2: `missing_roadmap_dir` also makes the
        // best-effort `write_checkpoint` call on this same bail path
        // fail (no parent directory to rename the temp file into) — that
        // failure must be swallowed exactly like the lane-log append
        // failure above, never surfacing as `CheckpointWriteFailed`
        // instead of the original `Execute` error.
        assert!(
            !matches!(err, IntegrateError::CheckpointWriteFailed(_)),
            "a masked checkpoint write failure would surface as CheckpointWriteFailed \
             instead of the original step failure: {err:?}"
        );
    }

    /// A two-step chain whose first step fails must not advance to the
    /// second: the second block's `run_flow` is never invoked, no `closed`
    /// line is ever written, and the only lane-log line on disk is the
    /// first step's `bailed` entry (EN.11.D Task 3).
    #[tokio::test]
    async fn a_failing_step_does_not_advance_to_the_next_step() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-a"), "A.2");

        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation| {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            Box::pin(async move {
                if invocation.block_id == "A.1" {
                    Err(crate::WorkflowError::new("simulated failure for A.1"))
                } else {
                    Ok(engine_contract::TaskContext {
                        event: json!({}),
                        nodes: std::collections::HashMap::new(),
                        metadata: json!({}),
                        node_runs: std::collections::HashMap::new(),
                    })
                }
            })
        });

        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();

        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        let err = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect_err("the chain must stop on the first failing step");

        assert!(matches!(err, IntegrateError::Execute(_)));

        // The second step's `run_flow` must never have been invoked — the
        // chain stops, it does not skip ahead or continue past a failure.
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["A.1".to_string()],
            "the second step must never run once the first has failed"
        );

        // Exactly one line on disk, and it is `bailed` for A.1 — never a
        // `closed` line for either step.
        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "no line for the never-run second step");
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["status"], json!("bailed"));
        assert_eq!(parsed["block"], json!("A.1"));
    }

    // ── EN.11.F task 4: the campaign budget ceiling at block boundaries ──

    /// A runner whose child `ctx` reports `tokens_per_call` tokens of
    /// usage on every invocation, via one `node_runs` entry — enough for
    /// `step_spend`/`CampaignLedger::record_step` to fold a real,
    /// non-zero, unambiguous token count into the campaign ledger per
    /// step (unlike [`recording_runner`], whose bare `TaskContext`
    /// reports none at all).
    fn token_reporting_runner(tokens_per_call: u64) -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation| {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            Box::pin(async move {
                let mut node_runs = std::collections::HashMap::new();
                node_runs.insert(
                    "SomeNode".to_string(),
                    engine_contract::NodeRun {
                        status: engine_contract::NodeRunStatus::Success,
                        started_at: None,
                        completed_at: None,
                        error: None,
                        input: None,
                        usage: Some(engine_contract::Usage {
                            input_tokens: Some(tokens_per_call),
                            output_tokens: Some(0),
                            model: "claude-sonnet-4-5".to_string(),
                        }),
                    },
                );
                Ok(engine_contract::TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs,
                })
            })
        });
        (runner, calls)
    }

    /// A campaign ceiling set to exactly one block's measured token cost
    /// halts the chain at the FIRST block boundary after that block, not
    /// after the whole chain — the second block never starts, the halt
    /// names the tripped cap, and the first block's `closed` line stays on
    /// disk untouched (Fork 1's decided semantics: the in-flight/completed
    /// block is never rolled back).
    #[tokio::test]
    async fn a_ceiling_at_one_blocks_cost_halts_the_chain_at_the_next_boundary() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        let (runner, calls) = token_reporting_runner(100);
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();
        let budget = Budget {
            max_total_tokens: Some(100),
            max_cost_usd: None,
        };

        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        let outcomes = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            Some(&budget),
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("a budget halt is not an error — it returns Ok with what already integrated");

        assert_eq!(
            outcomes.len(),
            1,
            "only A.1 should have integrated before the ceiling tripped"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["A.1".to_string()],
            "A.2's runner must never be invoked once the ceiling has tripped"
        );

        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "A.1's closed line stays, plus one budget_halted line naming A.2"
        );
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["status"], json!("closed"));
        assert_eq!(first["block"], json!("A.1"));
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["status"], json!("budget_halted"));
        assert_eq!(second["block"], json!("A.2"));
        assert!(
            second["note"]
                .as_str()
                .unwrap()
                .contains("max_total_tokens"),
            "the halt reason must name the tripped cap: {second}"
        );
    }

    /// The campaign ledger's boundary check runs at EVERY boundary, not
    /// once per campaign: a ceiling set ABOVE the combined cost of a
    /// three-block chain never trips, and every block still integrates —
    /// proving the check ran (and allowed) at all three boundaries rather
    /// than being skipped after the first.
    #[tokio::test]
    async fn a_ceiling_never_reached_lets_every_boundary_check_allow_and_the_whole_chain_run() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-a"), "A.2");
        write_done_state(&dir.path().join("repo-a"), "A.3");
        let (runner, calls) = token_reporting_runner(10);
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();
        // Three steps x 10 tokens = 30 accumulated at most; a cap of 1000
        // never trips at any of the three boundaries checked.
        let budget = Budget {
            max_total_tokens: Some(1000),
            max_cost_usd: None,
        };

        let chain = vec![
            step("repo-a", "A.1"),
            step("repo-a", "A.2"),
            step("repo-a", "A.3"),
        ];

        let outcomes = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            Some(&budget),
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("an unreached ceiling must never halt the chain");

        assert_eq!(outcomes.len(), 3, "every block must have integrated");
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["A.1".to_string(), "A.2".to_string(), "A.3".to_string()]
        );
    }

    // ── EN.11.H task 2: checkpoint written as each step integrates ──────

    /// A runner that behaves like [`recording_runner`] but also stamps a
    /// `SetupWorktreeNode` result carrying a deterministic branch name for
    /// each invoked block — mirrors what a real worktree-isolated
    /// `SDLC_FLOW` run leaves in `ctx.nodes` for
    /// [`step_branch_name`]/`wrap_up::branch_name` to read.
    fn branch_stamping_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation| {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            let branch_name = format!("{}-flow", invocation.block_id);
            Box::pin(async move {
                let mut nodes = std::collections::HashMap::new();
                nodes.insert(
                    "SetupWorktreeNode".to_string(),
                    json!({ "branch_name": branch_name }),
                );
                Ok(engine_contract::TaskContext {
                    event: json!({}),
                    nodes,
                    metadata: json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        (runner, calls)
    }

    #[tokio::test]
    async fn checkpoint_records_every_integrated_step_with_branch_in_order() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        write_done_state(&dir.path().join("repo-a"), "A.2");
        write_done_state(&dir.path().join("repo-a"), "A.3");
        let (runner, _calls) = branch_stamping_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = uuid::Uuid::new_v4();

        let chain = vec![
            step("repo-a", "A.1"),
            step("repo-a", "A.2"),
            step("repo-a", "A.3"),
        ];

        let outcomes = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            campaign_id,
        )
        .await
        .expect("all three steps should integrate");
        assert_eq!(outcomes.len(), 3);

        let checkpoint = read_checkpoint(roadmap_dir.path(), campaign_id)
            .expect("checkpoint read must not error")
            .into_option()
            .expect("a checkpoint must exist after integrating steps");

        assert_eq!(checkpoint.campaign_id, campaign_id);
        assert_eq!(checkpoint.steps.len(), 3, "exactly N integrated steps");
        let expected = [
            ("A.1", 1u32, "A.1-flow"),
            ("A.2", 2u32, "A.2-flow"),
            ("A.3", 3u32, "A.3-flow"),
        ];
        for (recorded, (block_id, index, branch)) in checkpoint.steps.iter().zip(expected) {
            assert_eq!(recorded.repo, "repo-a");
            assert_eq!(recorded.block_id, block_id);
            assert_eq!(recorded.index, index);
            assert!(recorded.integrated);
            assert_eq!(recorded.branch.as_deref(), Some(branch));
        }
    }

    /// The checkpoint write on the success path happens AFTER the
    /// lane-log line, per `integrate_chain`'s "Write the checkpoint on
    /// that same path, AFTER the lane-log line" contract. Pinned by
    /// making the checkpoint write fail (a directory sitting at the exact
    /// path `write_checkpoint`'s temp file would occupy) while the
    /// lane-log append succeeds against the same `roadmap_dir`: if the
    /// checkpoint were written FIRST, a crash-equivalent failure there
    /// would leave the lane log without its `closed` line; instead the
    /// lane-log line is on disk even though the overall call errors on
    /// the checkpoint write that came after it.
    #[tokio::test]
    async fn checkpoint_is_written_after_the_lane_log_line_on_the_success_path() {
        let (dir, registry) = two_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");
        let (runner, _calls) = recording_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = uuid::Uuid::new_v4();

        // Pre-create a DIRECTORY at the exact path `write_checkpoint`
        // would write its temp file to, so `fs::write` there fails —
        // simulating a checkpoint write failure without touching
        // `lane-log.jsonl` at all.
        let checkpoint_final = roadmap_dir
            .path()
            .join(format!("checkpoint-{campaign_id}.json"));
        let checkpoint_tmp = checkpoint_final.with_extension("json.tmp");
        std::fs::create_dir_all(&checkpoint_tmp).unwrap();

        let chain = vec![step("repo-a", "A.1")];

        let err = integrate_chain(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_: &StepProgress| {},
            false,
            campaign_id,
        )
        .await
        .expect_err("the checkpoint write failure must surface");

        assert!(
            matches!(err, IntegrateError::CheckpointWriteFailed(_)),
            "expected the checkpoint write's own failure to surface: {err:?}"
        );

        // The lane-log line was already appended BEFORE the (failing)
        // checkpoint write was even attempted.
        let contents = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "the closed lane-log line must be on disk");
        assert_eq!(
            serde_json::from_str::<Value>(lines[0]).unwrap()["block"],
            json!("A.1")
        );
    }
}
