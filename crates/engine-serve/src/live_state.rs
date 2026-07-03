//! In-memory live run-state store (D42): the local Console reads run state
//! directly from this store, with **no Postgres query** on the hot path.
//!
//! `LiveStateStore` keeps the latest `TaskContext` snapshot per run id in a
//! shared, lock-guarded map. `record` is meant to be called from inside an
//! `on_progress` closure at every node boundary (a cheap clone of the
//! snapshot); `get`/`list_active` are the local read API.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use engine_contract::TaskContext;
use uuid::Uuid;

/// Identifies a single workflow run. Matches the `events.id` primary key
/// (contract §4) so the live in-memory snapshot and the durable row share
/// the same identity.
pub type RunId = Uuid;

/// In-memory store of the latest `TaskContext` snapshot per run.
///
/// Cheap to clone (an `Arc` around the guarded map) so it can be shared
/// between the HTTP handlers, the `on_progress` recorder, and any
/// background tasks without extra synchronization.
#[derive(Clone, Default)]
pub struct LiveStateStore {
    inner: Arc<RwLock<HashMap<RunId, TaskContext>>>,
}

impl LiveStateStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the latest snapshot for `run_id`, overwriting whatever was
    /// there before. Intended to be called from within an `on_progress`
    /// closure at every node boundary.
    pub fn record(&self, run_id: RunId, snapshot: &TaskContext) {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.insert(run_id, snapshot.clone());
    }

    /// Read the latest snapshot for `run_id`, if any. Touches only the
    /// in-memory map — no Postgres access.
    pub fn get(&self, run_id: RunId) -> Option<TaskContext> {
        let guard = self
            .inner
            .read()
            .expect("live state store lock poisoned on read");
        guard.get(&run_id).cloned()
    }

    /// The run ids currently tracked by the store.
    pub fn list_active(&self) -> Vec<RunId> {
        let guard = self
            .inner
            .read()
            .expect("live state store lock poisoned on read");
        guard.keys().copied().collect()
    }

    /// Remove a run's snapshot from the store (e.g. once a run is complete
    /// and no longer needs to be served live).
    pub fn remove(&self, run_id: RunId) -> Option<TaskContext> {
        let mut guard = self
            .inner
            .write()
            .expect("live state store lock poisoned on write");
        guard.remove(&run_id)
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
}
