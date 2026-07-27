//! In-memory live run-state store (D42): the local Console reads run state
//! directly from this store, with **no Postgres query** on the hot path.
//!
//! `LiveStateStore` keeps the latest `TaskContext` snapshot per run id in a
//! shared, lock-guarded map. `record` is meant to be called from inside an
//! `on_progress` closure at every node boundary (a cheap clone of the
//! snapshot); `get`/`list_active` are the local read API.
//!
//! Once a run finishes, `mark_terminal` moves it out of the live map and
//! into a bounded ring of the most recent [`COMPLETED_RUN_RETENTION`]
//! completed runs, which is what `GET /events/{event_id}` (EN.5.F) serves a
//! terminal readback from. Live runs are never evicted by the completed-run
//! cap — only entries already marked terminal compete for the ring's slots.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use engine_contract::TaskContext;
use uuid::Uuid;

/// Identifies a single workflow run. Matches the `events.id` primary key
/// (contract §4) so the live in-memory snapshot and the durable row share
/// the same identity.
pub type RunId = Uuid;

/// Number of completed runs retained for readback once they go terminal.
/// The oldest completed run is evicted once the ring is full; live
/// (non-terminal) runs are never subject to this cap.
pub const COMPLETED_RUN_RETENTION: usize = 100;

/// A retained record for a completed run — everything the canonical
/// `GET /events/{event_id}` readback needs beyond the bare `TaskContext`
/// snapshot (contract: `{event_id, workflow_type, status, created_at,
/// updated_at, task_context}`, `status` derived from `terminal`).
#[derive(Clone, Debug, PartialEq)]
pub struct RunRecord {
    pub snapshot: TaskContext,
    pub workflow_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal: bool,
}

/// Bounded FIFO ring of completed-run records, keyed by run id.
#[derive(Default)]
struct CompletedRing {
    entries: HashMap<RunId, RunRecord>,
    order: VecDeque<RunId>,
}

impl CompletedRing {
    fn insert(&mut self, run_id: RunId, record: RunRecord) {
        if self.entries.insert(run_id, record).is_none() {
            self.order.push_back(run_id);
        }
        while self.order.len() > COMPLETED_RUN_RETENTION {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

/// In-memory store of the latest `TaskContext` snapshot per run, plus a
/// bounded ring of completed-run records for terminal readback.
///
/// Cheap to clone (an `Arc` around each guarded map) so it can be shared
/// between the HTTP handlers, the `on_progress` recorder, and any
/// background tasks without extra synchronization.
#[derive(Clone, Default)]
pub struct LiveStateStore {
    inner: Arc<RwLock<HashMap<RunId, TaskContext>>>,
    completed: Arc<RwLock<CompletedRing>>,
}

impl LiveStateStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the latest snapshot for `run_id`, overwriting whatever was
    /// there before. Intended to be called from within an `on_progress`
    /// closure at every node boundary.
    ///
    /// Signature and semantics are locked: `bastion`'s `GET /api/runs/{id}`
    /// projection and every existing `on_progress` recorder call this with
    /// exactly these two arguments.
    pub fn record(&self, run_id: RunId, snapshot: &TaskContext) {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.insert(run_id, snapshot.clone());
    }

    /// Read the latest snapshot for `run_id`, if any. Serves from the live
    /// map first, then falls back to the completed ring, so it keeps
    /// returning a snapshot for a run that has since gone terminal. Touches
    /// only in-memory state — no Postgres access.
    ///
    /// Signature and semantics are locked: `bastion`'s `GET /api/runs/{id}`
    /// projection consumes this over an unpinned path dependency.
    pub fn get(&self, run_id: RunId) -> Option<TaskContext> {
        {
            let guard = self
                .inner
                .read()
                .expect("live state store lock poisoned on read");
            if let Some(snapshot) = guard.get(&run_id) {
                return Some(snapshot.clone());
            }
        }
        let guard = self
            .completed
            .read()
            .expect("completed run ring lock poisoned on read");
        guard.entries.get(&run_id).map(|r| r.snapshot.clone())
    }

    /// Read the full retained record for a completed run — the snapshot
    /// plus `workflow_type`, `created_at`, `updated_at`, and the terminal
    /// flag the readback endpoint needs. `None` for a run that is still
    /// live (never marked terminal) or unknown entirely.
    pub fn get_record(&self, run_id: RunId) -> Option<RunRecord> {
        let guard = self
            .completed
            .read()
            .expect("completed run ring lock poisoned on read");
        guard.entries.get(&run_id).cloned()
    }

    /// The run ids currently tracked as **live** by the store. Terminal
    /// runs — retained in the completed ring for readback — are excluded.
    pub fn list_active(&self) -> Vec<RunId> {
        let guard = self
            .inner
            .read()
            .expect("live state store lock poisoned on read");
        guard.keys().copied().collect()
    }

    /// Remove a run's snapshot from the live map (e.g. once a run is
    /// complete and no longer needs to be served live). Does not touch the
    /// completed ring.
    pub fn remove(&self, run_id: RunId) -> Option<TaskContext> {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.remove(&run_id)
    }

    /// Mark a run terminal: move it out of the live map (if present) and
    /// into the bounded completed ring, retaining `snapshot`,
    /// `workflow_type`, `created_at`, and `updated_at` for readback. Meant
    /// to be called by the spawned run on every exit path — success, node
    /// error, cancellation, and budget halt.
    ///
    /// Evicts the oldest completed run once the ring exceeds
    /// [`COMPLETED_RUN_RETENTION`]; live runs are never evicted by this cap.
    pub fn mark_terminal(
        &self,
        run_id: RunId,
        snapshot: &TaskContext,
        workflow_type: impl Into<String>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) {
        let record = RunRecord {
            snapshot: snapshot.clone(),
            workflow_type: workflow_type.into(),
            created_at,
            updated_at,
            terminal: true,
        };
        // Insert into the completed ring *before* removing from the live
        // map: `inner` and `completed` are separate locks, so a `get`/
        // `get_record` landing between the two operations must never find
        // the run in neither — briefly present in both is fine, absent from
        // both is a spurious "unknown run".
        {
            let mut completed = self
                .completed
                .write()
                .expect("completed run ring lock poisoned on write");
            completed.insert(run_id, record);
        }
        let mut live = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        live.remove(&run_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn fixture_context(marker: &str) -> TaskContext {
        TaskContext {
            event: serde_json::json!({ "marker": marker }),
            nodes: StdHashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: StdHashMap::new(),
        }
    }

    #[test]
    fn record_then_get_returns_latest_state() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = fixture_context("first");

        store.record(run_id, &snapshot);

        assert_eq!(store.get(run_id), Some(snapshot));
    }

    #[test]
    fn later_snapshot_overwrites_earlier_one() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();

        store.record(run_id, &fixture_context("first"));
        store.record(run_id, &fixture_context("second"));

        let latest = store.get(run_id).expect("snapshot should be present");
        assert_eq!(latest.event, serde_json::json!({ "marker": "second" }));
    }

    #[test]
    fn get_on_unknown_run_returns_none() {
        let store = LiveStateStore::new();

        assert_eq!(store.get(Uuid::new_v4()), None);
    }

    #[test]
    fn list_active_reflects_recorded_runs_with_no_database() {
        // This test exercises only in-memory reads/writes: no DATABASE_URL,
        // no engine-store, no sqlx pool involved anywhere in this module —
        // the local read path never touches Postgres.
        let store = LiveStateStore::new();
        let run_a = Uuid::new_v4();
        let run_b = Uuid::new_v4();

        store.record(run_a, &fixture_context("a"));
        store.record(run_b, &fixture_context("b"));

        let mut active = store.list_active();
        active.sort();
        let mut expected = vec![run_a, run_b];
        expected.sort();
        assert_eq!(active, expected);
    }

    #[test]
    fn remove_drops_a_run_from_the_store() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        store.record(run_id, &fixture_context("gone-soon"));

        let removed = store.remove(run_id);

        assert_eq!(removed, Some(fixture_context("gone-soon")));
        assert_eq!(store.get(run_id), None);
    }

    #[test]
    fn terminal_run_is_readable_via_both_accessors() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let snapshot = fixture_context("done");
        let created_at = Utc::now();
        let updated_at = created_at + chrono::Duration::seconds(5);

        store.record(run_id, &snapshot);
        store.mark_terminal(
            run_id,
            &snapshot,
            "example_workflow",
            created_at,
            updated_at,
        );

        assert_eq!(store.get(run_id), Some(snapshot.clone()));

        let record = store
            .get_record(run_id)
            .expect("terminal run should have a retained record");
        assert_eq!(record.snapshot, snapshot);
        assert_eq!(record.workflow_type, "example_workflow");
        assert_eq!(record.created_at, created_at);
        assert_eq!(record.updated_at, updated_at);
        assert!(record.terminal);
    }

    #[test]
    fn the_101st_completed_run_evicts_the_oldest() {
        let store = LiveStateStore::new();
        let now = Utc::now();
        let mut run_ids = Vec::new();

        for i in 0..(COMPLETED_RUN_RETENTION + 1) {
            let run_id = Uuid::new_v4();
            let snapshot = fixture_context(&format!("run-{i}"));
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
            run_ids.push(run_id);
        }

        let oldest = run_ids[0];
        let newest = *run_ids.last().unwrap();

        assert_eq!(
            store.get(oldest),
            None,
            "oldest completed run should be evicted"
        );
        assert!(store.get_record(oldest).is_none());
        assert!(
            store.get(newest).is_some(),
            "newest completed run should remain"
        );
    }

    #[test]
    fn a_live_run_is_never_evicted_by_the_completed_cap() {
        let store = LiveStateStore::new();
        let live_run = Uuid::new_v4();
        store.record(live_run, &fixture_context("still-running"));

        let now = Utc::now();
        for i in 0..(COMPLETED_RUN_RETENTION + 10) {
            let run_id = Uuid::new_v4();
            let snapshot = fixture_context(&format!("completed-{i}"));
            store.mark_terminal(run_id, &snapshot, "wf", now, now);
        }

        assert_eq!(
            store.get(live_run),
            Some(fixture_context("still-running")),
            "a live run must never be evicted by the completed-run cap"
        );
        assert!(store.list_active().contains(&live_run));
    }

    #[test]
    fn list_active_excludes_terminal_runs() {
        let store = LiveStateStore::new();
        let live_run = Uuid::new_v4();
        let terminal_run = Uuid::new_v4();

        store.record(live_run, &fixture_context("live"));
        store.record(terminal_run, &fixture_context("about-to-finish"));

        let now = Utc::now();
        store.mark_terminal(
            terminal_run,
            &fixture_context("about-to-finish"),
            "wf",
            now,
            now,
        );

        let active = store.list_active();
        assert!(active.contains(&live_run));
        assert!(!active.contains(&terminal_run));
    }
}
