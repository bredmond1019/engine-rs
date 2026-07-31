//! Process-global suspend/resume registries (task 8): the pause-signal map an
//! operator's `POST /events/{run_id}/pause` and a running walk rendezvous on,
//! plus the suspended-run index a later `POST /events/{run_id}/resume` reads
//! back from.
//!
//! **`AppState` gains no field.** `bastion` builds [`crate::http::AppState`]
//! as a struct literal over an unpinned path dependency (see the module docs
//! at `http.rs:170-176`), so a new public field there is an immediate
//! cross-repo compile break for no gain. This module follows the established
//! process-global `OnceLock` pattern from `http.rs`'s `live_run_metadata()`
//! and `stream.rs`'s `registry()` instead.
//!
//! **Why in-memory, not just Postgres.** Holding `data` (the original
//! trigger payload) and `snapshot` (the last `TaskContext`) in
//! [`SuspendedEntry`] is what makes resume work with **no `DATABASE_URL`** —
//! CI has none, and the readback path is deliberately DB-free
//! (`http.rs:456-458`). Postgres is the restart-survival fallback only.
//!
//! **Eviction backstop.** [`insert_suspended`] hands back the entry a
//! bounded FIFO ring pushed out so the caller can stamp cancellation into
//! its snapshot and `mark_terminal` it. Without that, a suspended run
//! evicted from this index would leak in the live map forever and vanish
//! from readback — and auto-expiry is explicitly out of scope, so this is
//! the only backstop.

use std::collections::{HashMap as StdHashMap, VecDeque};
use std::sync::{OnceLock, RwLock};

use chrono::{DateTime, Utc};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::workflow::ResumeState;
use engine_core::{Budget, CancellationToken, PauseSignal, Workflow};
use futures::FutureExt;
use uuid::Uuid;

use crate::abort::RunRegistry;
use crate::durable::{durable_on_progress, DurableHandle};
use crate::live_state::LiveStateStore;

/// A suspended run's rehydration payload — everything a resume needs to
/// rebuild the `Workflow` and continue the walk from `snapshot`'s recorded
/// pointer, without touching Postgres.
#[derive(Clone)]
pub struct SuspendedEntry {
    pub workflow_type: String,
    /// The ORIGINAL trigger payload -- needed to rebuild the `Workflow`.
    pub data: serde_json::Value,
    pub snapshot: TaskContext,
    pub created_at: DateTime<Utc>,
    pub suspended_at: DateTime<Utc>,
    pub resume_at: String,
    pub reason: String,
    /// In-flight-resume guard: set by [`take_for_resume`], cleared back to
    /// `false` (i.e. `Ready`) by [`clear_resuming`] on a failed resume.
    pub resuming: bool,
}

/// The result of [`take_for_resume`]'s atomic read-and-set.
pub enum TakeForResume {
    /// The entry was `Ready`; it is now marked `resuming` in place (still
    /// present in the index) pending the resume outcome. Boxed: at 296+
    /// bytes `SuspendedEntry` would otherwise make every `TakeForResume`
    /// (including the far smaller `AlreadyResuming`/`NotFound` variants) pay
    /// its size.
    Ready(Box<SuspendedEntry>),
    /// A concurrent caller already took this run for resume.
    AlreadyResuming,
    /// No suspended entry exists for this `run_id`.
    NotFound,
}

/// Process-global map of live pause signals, keyed by `run_id`. Populated
/// when a run starts (or is resumed) and consulted by `Workflow::walk`'s
/// operator-pause check at every node boundary.
fn pause_signals() -> &'static RwLock<StdHashMap<Uuid, PauseSignal>> {
    static PAUSE_SIGNALS: OnceLock<RwLock<StdHashMap<Uuid, PauseSignal>>> = OnceLock::new();
    PAUSE_SIGNALS.get_or_init(|| RwLock::new(StdHashMap::new()))
}

/// Register a run's pause signal so `POST /events/{run_id}/pause` can find
/// it later. Overwrites any existing signal for the same `run_id` (the
/// resume path registers a fresh one).
pub fn register_pause_signal(run_id: Uuid, sig: PauseSignal) {
    pause_signals()
        .write()
        .expect("pause-signal registry lock poisoned on write")
        .insert(run_id, sig);
}

/// Look up a run's pause signal, if it is currently registered.
pub fn get_pause_signal(run_id: Uuid) -> Option<PauseSignal> {
    pause_signals()
        .read()
        .expect("pause-signal registry lock poisoned on read")
        .get(&run_id)
        .cloned()
}

/// Deregister a run's pause signal — called once the run goes terminal (or
/// suspended) and the signal is no longer meaningful.
pub fn remove_pause_signal(run_id: Uuid) {
    pause_signals()
        .write()
        .expect("pause-signal registry lock poisoned on write")
        .remove(&run_id);
}

/// Bounded FIFO index of suspended runs, mirroring
/// `live_state::LiveStateStore`'s completed-run ring
/// (`live_state::COMPLETED_RUN_RETENTION`) so a long-lived server process
/// doesn't accumulate one held `TaskContext` + trigger payload per suspended
/// run forever.
#[derive(Default)]
struct SuspendedIndex {
    entries: StdHashMap<Uuid, SuspendedEntry>,
    order: VecDeque<Uuid>,
}

impl SuspendedIndex {
    /// Inserts `entry`, evicting the oldest entry if the cap is exceeded.
    /// Returns the evicted `(run_id, entry)`, if any.
    fn insert(&mut self, run_id: Uuid, entry: SuspendedEntry) -> Option<(Uuid, SuspendedEntry)> {
        if self.entries.insert(run_id, entry).is_none() {
            self.order.push_back(run_id);
        }
        if self.order.len() > crate::live_state::COMPLETED_RUN_RETENTION {
            if let Some(oldest) = self.order.pop_front() {
                if oldest == run_id {
                    // The just-inserted entry is itself the eviction victim
                    // (retention cap of 0, or a re-insert of the same id
                    // that immediately overflowed) -- nothing else to do.
                    return None;
                }
                if let Some(evicted) = self.entries.remove(&oldest) {
                    return Some((oldest, evicted));
                }
            }
        }
        None
    }
}

fn suspended_runs() -> &'static RwLock<SuspendedIndex> {
    static SUSPENDED_RUNS: OnceLock<RwLock<SuspendedIndex>> = OnceLock::new();
    SUSPENDED_RUNS.get_or_init(|| RwLock::new(SuspendedIndex::default()))
}

/// Insert (or overwrite) a suspended run's entry. Returns the entry the
/// bounded ring pushed out, if the cap was exceeded — the caller must stamp
/// cancellation into its snapshot and `mark_terminal` it, or it leaks.
pub fn insert_suspended(run_id: Uuid, entry: SuspendedEntry) -> Option<(Uuid, SuspendedEntry)> {
    suspended_runs()
        .write()
        .expect("suspended-run registry lock poisoned on write")
        .insert(run_id, entry)
}

/// All currently suspended runs, newest first.
pub fn list_suspended() -> Vec<(Uuid, SuspendedEntry)> {
    let guard = suspended_runs()
        .read()
        .expect("suspended-run registry lock poisoned on read");
    guard
        .order
        .iter()
        .rev()
        .filter_map(|run_id| {
            guard
                .entries
                .get(run_id)
                .map(|entry| (*run_id, entry.clone()))
        })
        .collect()
}

/// Atomically read-and-set a suspended run's `resuming` flag under one write
/// lock — a check-then-act split is exactly the double-resume the resume
/// endpoint's acceptance criteria forbid.
///
/// On `Ready`, the entry's `resuming` flag is flipped to `true` **in
/// place** (the entry stays in the index) and a clone is handed back so the
/// caller can rebuild the `Workflow`. Leaving it in the index — rather than
/// removing it — is what makes a second, genuinely concurrent caller land
/// on `AlreadyResuming` instead of `NotFound`: removal would make the
/// second caller's `entries.get` see nothing at all and misreport the run
/// as never-suspended. The entry only leaves the index via
/// [`remove_suspended`] (resume succeeded) or stays until [`clear_resuming`]
/// flips the flag back off (resume failed, retryable).
pub fn take_for_resume(run_id: Uuid) -> TakeForResume {
    let mut guard = suspended_runs()
        .write()
        .expect("suspended-run registry lock poisoned on write");
    match guard.entries.get_mut(&run_id) {
        None => TakeForResume::NotFound,
        Some(entry) if entry.resuming => TakeForResume::AlreadyResuming,
        Some(entry) => {
            entry.resuming = true;
            TakeForResume::Ready(Box::new(entry.clone()))
        }
    }
}

/// Rollback for a failed resume attempt: flips `resuming` back to `false`
/// for `run_id`'s entry (still present in the index from
/// [`take_for_resume`]) so the run is `Ready` again. A no-op if the entry
/// is no longer present (e.g. it was removed / evicted in the meantime).
pub fn clear_resuming(run_id: Uuid) {
    let mut guard = suspended_runs()
        .write()
        .expect("suspended-run registry lock poisoned on write");
    if let Some(entry) = guard.entries.get_mut(&run_id) {
        entry.resuming = false;
    }
}

/// Remove a suspended run's entry outright (e.g. it was cancelled while
/// suspended, without going through resume).
pub fn remove_suspended(run_id: Uuid) -> Option<SuspendedEntry> {
    let mut guard = suspended_runs()
        .write()
        .expect("suspended-run registry lock poisoned on write");
    if let Some(entry) = guard.entries.remove(&run_id) {
        guard.order.retain(|id| *id != run_id);
        Some(entry)
    } else {
        None
    }
}

/// Which of the two starting points a spawned run began from — a fresh
/// trigger (`POST /events/`) or a rehydrated resume (`POST
/// /events/{run_id}/resume`, task 11). Kept as a two-variant enum rather
/// than an `Option` so the dispatch in [`spawn_run`] reads as the two-way
/// fork it is, not an optional extra.
pub(crate) enum RunStart {
    /// A brand-new run: the trigger payload to seed `Workflow::run_with`.
    Fresh(serde_json::Value),
    /// A resume: the rehydrated pointer/context/ledger to seed
    /// `Workflow::run_from`. Not yet constructed anywhere -- the resume
    /// endpoint (EN.6.F task 11) is this variant's first caller.
    #[allow(dead_code)]
    Resume(ResumeState),
}

/// Everything [`spawn_run`] needs to run a workflow to completion (or
/// suspension) and fork its exit path accordingly — the trigger and resume
/// HTTP handlers both build one of these so the two paths can never drift
/// (EN.6.F task 10).
pub(crate) struct SpawnedRun {
    pub run_id: Uuid,
    pub workflow: Workflow,
    pub workflow_type: String,
    /// The ORIGINAL trigger payload, in both the fresh and the resume case
    /// — needed for the durable writer's `events.data` column and, on a
    /// suspended exit, for [`SuspendedEntry::data`] (a later resume rebuilds
    /// the `Workflow` from this, not from `state.ctx`).
    pub data: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub start: RunStart,
    pub live: LiveStateStore,
    pub durable: DurableHandle,
    pub runs: RunRegistry,
    pub token: CancellationToken,
    pub pause: PauseSignal,
    pub budget: Budget,
}

/// Scans `ctx.node_runs` for the first node stamped [`NodeRunStatus::Failed`]
/// and formats a human-readable reason (`"node {name} failed: {error}"`) for
/// the EN.6.J task 5 failure-path terminal writer. Returns `None` for a
/// clean run -- the common case, and the only one that matters for the
/// `Ok(Ok(ctx))` branch of `spawn_run`'s `run_result` match, since a
/// structural `WorkflowError` and a panic are already `Err`/`catch_unwind`
/// branches with their own reason. `HashMap` iteration order is
/// non-deterministic, but that is not a concern here: a node returning
/// `Err` halts the walk (`Workflow::walk`'s dispatch loop stops advancing
/// once a node fails), so a failed walk has at most one `Failed` entry in
/// `node_runs` in practice.
fn failed_node_reason(ctx: &TaskContext) -> Option<String> {
    ctx.node_runs.iter().find_map(|(name, run)| {
        if run.status == NodeRunStatus::Failed {
            let error = run
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            Some(format!("node {name} failed: {error}"))
        } else {
            None
        }
    })
}

/// Runs `spawned.workflow` to completion (or suspension) on
/// `actix_web::rt::spawn`, then forks the exit path on
/// `engine_core::suspend::is_suspended(&final_ctx.metadata)`:
///
/// | | terminal (unchanged) | suspended |
/// |---|---|---|
/// | SSE | `publish_terminal` | `publish_suspended` |
/// | live state | `mark_terminal` (moves to the completed ring) | stays in the live map; `live.record` |
/// | `live_run_metadata()` | removed | kept — preserves the original `created_at` |
/// | suspended index | -- | `insert_suspended`; an eviction is stamped cancelled and `mark_terminal`ed |
/// | `RunRegistry` | `deregister` | `deregister` (nobody is checking the token) |
/// | pause signal | `remove_pause_signal` | `remove_pause_signal` |
///
/// Both `post_events` (fresh trigger) and the resume handler (task 11) call
/// this — the terminal-path logic (the three-way `on_progress` fan-out, the
/// `catch_unwind` guard, and the `Ok(Ok)/Ok(Err)/Err` match) is written
/// exactly once so the two entry points can never drift.
pub(crate) fn spawn_run(spawned: SpawnedRun) {
    let SpawnedRun {
        run_id,
        workflow,
        workflow_type,
        data,
        created_at,
        start,
        live,
        durable,
        runs,
        token,
        pause,
        budget,
    } = spawned;

    actix_web::rt::spawn(async move {
        let mut durable_progress =
            durable_on_progress(durable, run_id, workflow_type.clone(), data.clone());
        let progress_live = live.clone();
        let on_progress: engine_core::OnProgress<'static> = Box::new(move |snapshot| {
            progress_live.record(run_id, snapshot);
            durable_progress(snapshot);
            // Third fan-out alongside live-state and the durable writer —
            // not a second progress mechanism.
            crate::stream::publish(run_id, snapshot);
        });

        let options = engine_core::RunOptions {
            cancellation_token: Some(token),
            budget: Some(budget),
            pause_signal: Some(pause.clone()),
            // EN.6.J task 5: the minted `run_id` now reaches the workflow
            // context via `RunOptions`, stamped into `ctx.metadata` by
            // `Workflow::run_with`/`run_from` (task 2) before the first node
            // dispatches -- this is what makes a flow-state artifact
            // joinable back to the engine run that produced it.
            run_id: Some(run_id),
        };

        // A cancelled or budget-halted run returns `Ok` with the marker
        // already stamped into `ctx.metadata` (see `RunOptions`'s docs); a
        // node's own failure is likewise folded into `Ok(ctx)` (the node
        // run is stamped FAILED, the walk halts, and the accumulated
        // context is still returned). Only a structural `WorkflowError`
        // (e.g. an unresolvable node identity) lands in `Err` here. The
        // response was sent long ago either way, so there is no status code
        // to map failure to — the readback and the terminal SSE frame are
        // how it surfaces.
        //
        // `catch_unwind` guards against a node implementation panicking
        // instead of returning `Err` (an internal `unwrap()`/`expect()`/
        // index-panic): without it, the panic would abort this spawned
        // task before reaching the cleanup below, leaking the run in
        // `live_run_metadata()`/`RunRegistry` forever and leaving any SSE
        // subscriber hanging with no terminal frame.
        let run_result = match start {
            RunStart::Fresh(event) => {
                std::panic::AssertUnwindSafe(workflow.run_with(event, on_progress, options))
                    .catch_unwind()
                    .await
            }
            RunStart::Resume(state) => {
                std::panic::AssertUnwindSafe(workflow.run_from(state, on_progress, options))
                    .catch_unwind()
                    .await
            }
        };

        // `failure_reason` names why the walk did not complete cleanly --
        // `Some` for the `Ok(Err)`/panic branches below and for an
        // `Ok(Ok(ctx))` whose `node_runs` recorded a node stamped `Failed`
        // (a node returning its own `Err` halts the walk but still returns
        // `Ok(ctx)` from `run_with`/`run_from` -- see the comment above this
        // match). `None` for a clean run.
        let (final_ctx, failure_reason) = match run_result {
            Ok(Ok(ctx)) => {
                let reason = failed_node_reason(&ctx);
                (ctx, reason)
            }
            Ok(Err(err)) => {
                eprintln!("run {run_id} failed: {err}");
                let mut ctx = live
                    .get(run_id)
                    .unwrap_or_else(crate::http::empty_task_context);
                let reason = err.to_string();
                crate::http::stamp_failure(&mut ctx, &reason);
                (ctx, Some(reason))
            }
            Err(panic_payload) => {
                let message = crate::http::panic_message(&panic_payload);
                eprintln!("run {run_id} panicked: {message}");
                let mut ctx = live
                    .get(run_id)
                    .unwrap_or_else(crate::http::empty_task_context);
                let reason = format!("node panicked: {message}");
                crate::http::stamp_failure(&mut ctx, &reason);
                (ctx, Some(reason))
            }
        };

        let updated_at = Utc::now();
        let suspended = engine_core::suspend::is_suspended(&final_ctx.metadata);

        // EN.6.J task 5: a failed walk (not a suspended exit -- that run is
        // not over) leaves a terminal `"blocked"` status in the flow's
        // committed `sdlc-flow-state.json` instead of rotting at whatever
        // non-terminal `"running"` the last `SaveStateNode` write left it
        // at. Guarded on `workflow_type` so no other workflow pays for this
        // lookup; `write_terminal_blocked_state` is a safe no-op for a
        // non-SDLC-flow context anyway (see its doc comment), but the guard
        // makes the intent legible at this call site.
        if !suspended && workflow_type == engine_core::workflows::sdlc_flow::graph::WORKFLOW_TYPE {
            if let Some(reason) = &failure_reason {
                let _ = engine_core::workflows::sdlc_flow::wrap_up::write_terminal_blocked_state(
                    &final_ctx, reason,
                );
            }
        }

        if suspended {
            // Suspended exit (EN.6.F): the run is not over, so it stays in
            // the live map and `live_run_metadata()` -- unlike the terminal
            // branch below, neither is cleared here.
            crate::stream::publish_suspended(run_id, &final_ctx);
            live.record(run_id, &final_ctx);

            let suspension = engine_core::suspend::read_suspension(&final_ctx.metadata);
            let resume_at = suspension
                .as_ref()
                .and_then(|s| s.resume_at.clone())
                .unwrap_or_default();
            let reason = suspension
                .as_ref()
                .and_then(|s| s.reason)
                .and_then(|r| serde_json::to_value(r).ok())
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();

            let entry = SuspendedEntry {
                workflow_type: workflow_type.clone(),
                data,
                snapshot: final_ctx.clone(),
                created_at,
                suspended_at: updated_at,
                resume_at,
                reason,
                resuming: false,
            };

            // If the bounded ring evicted an older suspended run to make
            // room, that run has nowhere left to resume from -- stamp
            // cancellation into its retained snapshot and mark it terminal
            // so it stops looking live/suspended forever (the eviction
            // backstop documented at the top of this module).
            if let Some((evicted_id, mut evicted_entry)) = insert_suspended(run_id, entry) {
                engine_core::stamp_cancelled(&mut evicted_entry.snapshot.metadata);
                live.mark_terminal(
                    evicted_id,
                    &evicted_entry.snapshot,
                    evicted_entry.workflow_type,
                    evicted_entry.created_at,
                    Utc::now(),
                );
            }
        } else {
            // Publish the SSE terminal frame before marking the readback
            // terminal, so a client racing the two never sees a terminal
            // readback with no terminal frame having gone out yet.
            crate::stream::publish_terminal(run_id, &final_ctx);
            // Order matters: mark terminal *before* deregistering.
            // Deregistration is the externally-observable "this run is
            // over" edge (an abort against a deregistered run 404s), so
            // anything a client can read after that edge must already be
            // in place.
            live.mark_terminal(run_id, &final_ctx, workflow_type, created_at, updated_at);
            crate::http::live_run_metadata()
                .write()
                .expect("live run metadata lock poisoned on write")
                .remove(&run_id);
        }

        runs.deregister(run_id);
        remove_pause_signal(run_id);
    });
}

/// Serializes tests across this crate that touch the process-global
/// suspend/pause-signal registries (`suspended_runs()`, `pause_signals()`).
///
/// Under `cargo nextest run` each test is its own process, so the
/// `OnceLock`-backed statics start fresh every time and cross-test
/// contamination is structurally impossible (CLAUDE.md standing rule 7).
/// Plain `cargo test` runs every test as a thread in ONE process sharing
/// those statics, so two registry-touching tests running concurrently can
/// observe each other's transient inserts (e.g. one test's momentary
/// suspended entry making another test's `list_suspended().is_empty()`
/// assertion fail) or starve each other's spawned workflow of CPU past a
/// polling deadline. Any test in `suspend.rs`, `resume.rs`, or `http.rs`
/// that inserts into, removes from, or lists the suspended index (or
/// registers/removes a pause signal) must hold this lock for its duration.
#[cfg(test)]
pub(crate) fn registry_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(reason: &str) -> SuspendedEntry {
        let now = Utc::now();
        SuspendedEntry {
            workflow_type: "test-workflow".to_string(),
            data: serde_json::json!({"k": "v"}),
            snapshot: TaskContext {
                event: serde_json::Value::Null,
                nodes: StdHashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: StdHashMap::new(),
            },
            created_at: now,
            suspended_at: now,
            resume_at: "node-b".to_string(),
            reason: reason.to_string(),
            resuming: false,
        }
    }

    // -- pause signals ---------------------------------------------------

    #[test]
    fn register_get_remove_pause_signal_round_trips() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        assert!(get_pause_signal(run_id).is_none());

        let sig = PauseSignal::new();
        register_pause_signal(run_id, sig.clone());

        let fetched = get_pause_signal(run_id).expect("signal should be registered");
        assert!(!fetched.is_paused());
        sig.pause();
        assert!(fetched.is_paused(), "clones observe the same signal");

        remove_pause_signal(run_id);
        assert!(get_pause_signal(run_id).is_none());
    }

    #[test]
    fn get_pause_signal_missing_returns_none() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        assert!(get_pause_signal(run_id).is_none());
    }

    // -- suspended index: FIFO eviction -----------------------------------

    #[test]
    fn insert_suspended_evicts_fifo_at_the_retention_cap() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ids: Vec<Uuid> = (0..(crate::live_state::COMPLETED_RUN_RETENTION + 1))
            .map(|_| Uuid::new_v4())
            .collect();

        let mut evicted = None;
        for id in &ids {
            let result = insert_suspended(*id, sample_entry("fifo-test"));
            if result.is_some() {
                evicted = result;
            }
        }

        let (evicted_id, _) = evicted.expect("cap should have been exceeded exactly once");
        assert_eq!(
            evicted_id, ids[0],
            "the oldest inserted entry must be the one evicted"
        );

        // Clean up so this test doesn't leave the global index bloated for
        // any test run after it in the same process.
        for id in &ids[1..] {
            remove_suspended(*id);
        }
    }

    // -- take_for_resume / clear_resuming ----------------------------------

    #[test]
    fn take_for_resume_is_ready_once_then_already_resuming() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        insert_suspended(run_id, sample_entry("double-resume-test"));

        match take_for_resume(run_id) {
            TakeForResume::Ready(entry) => {
                assert!(entry.resuming);
            }
            _ => panic!("first take_for_resume should be Ready"),
        }

        // The entry stays in the index (still present, `resuming == true`),
        // so a second, genuinely concurrent caller must see
        // `AlreadyResuming` rather than `NotFound`.
        match take_for_resume(run_id) {
            TakeForResume::AlreadyResuming => {}
            _ => panic!("second concurrent take_for_resume should be AlreadyResuming"),
        }

        remove_suspended(run_id);
    }

    #[test]
    fn take_for_resume_from_multiple_threads_grants_ready_exactly_once() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        insert_suspended(run_id, sample_entry("thread-race-test"));

        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    matches!(take_for_resume(run_id), TakeForResume::Ready(_))
                })
            })
            .collect();

        let ready_count = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .filter(|was_ready| *was_ready)
            .count();

        assert_eq!(ready_count, 1, "exactly one caller should get Ready");
        remove_suspended(run_id);
    }

    #[test]
    fn take_for_resume_missing_run_is_not_found() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        match take_for_resume(run_id) {
            TakeForResume::NotFound => {}
            _ => panic!("expected NotFound for an unregistered run id"),
        }
    }

    #[test]
    fn clear_resuming_restores_entry_to_ready() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        insert_suspended(run_id, sample_entry("clear-resuming-test"));

        match take_for_resume(run_id) {
            TakeForResume::Ready(entry) => assert!(entry.resuming),
            _ => panic!("expected Ready"),
        };

        clear_resuming(run_id);

        match take_for_resume(run_id) {
            TakeForResume::Ready(entry) => assert!(entry.resuming),
            _ => panic!("clear_resuming should have made the run Ready again"),
        }

        remove_suspended(run_id);
    }

    // -- list_suspended ordering -------------------------------------------

    #[test]
    fn list_suspended_orders_newest_first() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();

        insert_suspended(id_a, sample_entry("order-a"));
        insert_suspended(id_b, sample_entry("order-b"));
        insert_suspended(id_c, sample_entry("order-c"));

        let listed = list_suspended();
        let positions: StdHashMap<Uuid, usize> = listed
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (*id, i))
            .collect();

        assert!(positions[&id_c] < positions[&id_b]);
        assert!(positions[&id_b] < positions[&id_a]);

        remove_suspended(id_a);
        remove_suspended(id_b);
        remove_suspended(id_c);
    }

    #[test]
    fn remove_suspended_missing_returns_none() {
        let _guard = registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        assert!(remove_suspended(run_id).is_none());
    }
}
