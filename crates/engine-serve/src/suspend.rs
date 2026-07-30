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
use engine_contract::TaskContext;
use engine_core::PauseSignal;
use uuid::Uuid;

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
        let run_id = Uuid::new_v4();
        assert!(get_pause_signal(run_id).is_none());
    }

    // -- suspended index: FIFO eviction -----------------------------------

    #[test]
    fn insert_suspended_evicts_fifo_at_the_retention_cap() {
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
        let run_id = Uuid::new_v4();
        match take_for_resume(run_id) {
            TakeForResume::NotFound => {}
            _ => panic!("expected NotFound for an unregistered run id"),
        }
    }

    #[test]
    fn clear_resuming_restores_entry_to_ready() {
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
        let run_id = Uuid::new_v4();
        assert!(remove_suspended(run_id).is_none());
    }
}
