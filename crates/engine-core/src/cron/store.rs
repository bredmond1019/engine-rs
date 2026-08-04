//! Injectable `CronStore` seam + file-backed default impl + `tick()` driver
//! — Phase 6 Block M (`EN.6.M`) task 3.
//!
//! Mirrors this repo's established injectable-seam convention (see
//! `crates/engine-core/src/workflows/sdlc_flow/mod.rs`'s `CommandRunner`): a
//! trait here (`CronStore`), a real impl backed by I/O (`FileCronStore`),
//! and an in-memory `#[cfg(test)]` stub for tests. `tick()` is the driver a
//! caller (e.g. `EN.6.G`'s `schedule.rs`) polls on an interval: it fires
//! every due, enabled record, advances its `next_fire_at` via
//! [`super::advance_next_fire_at`], and records the fire in the durable log.
//!
//! `FileCronStore` persists the whole store as a single JSON file, written
//! whole-object on every mutation (matching this repo's existing
//! whole-object JSON persistence convention — see `repo_registry.rs` —
//! rather than inventing an append-only log file format). This is
//! deliberately *not* the Postgres pattern in `engine-serve/src/durable.rs`:
//! the `events` table that code writes to is the orchestrator's
//! pre-existing externally-provisioned schema (contract §4) with no
//! migration tooling in this repo to add a table alongside it, and a single
//! Mac Mini operator needs nothing more than "survives a process restart".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::record::{CronFireLogEntry, CronRecord, FireOutcome};
use super::{advance_next_fire_at, CronSchedule};

/// The injectable seam for cron durable state — mirrors `CommandRunner`'s
/// trait/closure convention. A default impl ([`FileCronStore`]) persists to
/// a JSON file; tests substitute an in-memory stub (no real filesystem
/// writes in the gated suite).
pub trait CronStore: Send + Sync {
    /// All records, in no particular order.
    fn list(&self) -> Vec<CronRecord>;
    /// A single record by id, if it exists.
    fn get(&self, id: &str) -> Option<CronRecord>;
    /// All *enabled* records whose `next_fire_at` is at or before `now`.
    fn due(&self, now: DateTime<Utc>) -> Vec<CronRecord>;
    /// Append a fire-log entry for `id` and update its `next_fire_at`.
    fn record_fire(&self, id: &str, entry: CronFireLogEntry, next_fire_at: Option<DateTime<Utc>>);
    /// The full fire log for `id`, in fire order (oldest first).
    fn fire_log(&self, id: &str) -> Vec<CronFireLogEntry>;
}

/// The JSON shape persisted by [`FileCronStore`] — a whole-object dump of
/// every record plus its fire log, matching this repo's existing
/// whole-object JSON persistence convention.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreFile {
    entries: HashMap<String, StoreEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreEntry {
    record: CronRecord,
    fire_log: Vec<CronFireLogEntry>,
}

/// A [`CronStore`] backed by a single JSON file, written whole-object on
/// every mutation. Read on construction if the file already exists —
/// this is what makes the fire log survive a process restart.
pub struct FileCronStore {
    path: PathBuf,
    entries: Mutex<HashMap<String, StoreEntry>>,
}

impl FileCronStore {
    /// Open (or create) a `FileCronStore` at `path`. If the file exists, it
    /// is read and deserialized immediately; if it does not exist yet, the
    /// store starts empty and is created on the first mutation.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let entries = Self::load(&path)?;
        Ok(FileCronStore {
            path,
            entries: Mutex::new(entries),
        })
    }

    fn load(path: &Path) -> std::io::Result<HashMap<String, StoreEntry>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let raw = std::fs::read_to_string(path)?;
        let file: StoreFile = serde_json::from_str(&raw).map_err(std::io::Error::other)?;
        Ok(file.entries)
    }

    /// Register a new record (or overwrite an existing one by id) without
    /// counting as a fire. Used to seed the store; not part of the
    /// `CronStore` trait since only a `FileCronStore` (or an equivalent
    /// concrete store) needs a construction-time seeding API — callers that
    /// only hold `&dyn CronStore` never need to add records, only tick
    /// through the ones already there.
    pub fn upsert(&self, record: CronRecord) {
        let mut entries = self.entries.lock().expect("CronStore mutex poisoned");
        entries
            .entry(record.id.clone())
            .and_modify(|e| e.record = record.clone())
            .or_insert_with(|| StoreEntry {
                record,
                fire_log: Vec::new(),
            });
        drop(entries);
        self.persist();
    }

    fn persist(&self) {
        let entries = self.entries.lock().expect("CronStore mutex poisoned");
        let file = StoreFile {
            entries: entries.clone(),
        };
        drop(entries);
        // Best-effort: a failed write here means the in-memory state (still
        // correct) simply doesn't survive the *next* restart; it must not
        // panic a live tick() loop over a transient disk issue.
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

impl CronStore for FileCronStore {
    fn list(&self) -> Vec<CronRecord> {
        self.entries
            .lock()
            .expect("CronStore mutex poisoned")
            .values()
            .map(|e| e.record.clone())
            .collect()
    }

    fn get(&self, id: &str) -> Option<CronRecord> {
        self.entries
            .lock()
            .expect("CronStore mutex poisoned")
            .get(id)
            .map(|e| e.record.clone())
    }

    fn due(&self, now: DateTime<Utc>) -> Vec<CronRecord> {
        self.entries
            .lock()
            .expect("CronStore mutex poisoned")
            .values()
            .filter(|e| e.record.enabled)
            .filter(|e| matches!(e.record.next_fire_at, Some(t) if t <= now))
            .map(|e| e.record.clone())
            .collect()
    }

    fn record_fire(&self, id: &str, entry: CronFireLogEntry, next_fire_at: Option<DateTime<Utc>>) {
        {
            let mut entries = self.entries.lock().expect("CronStore mutex poisoned");
            if let Some(e) = entries.get_mut(id) {
                e.record.last_fired_at = Some(entry.fired_at);
                e.record.next_fire_at = next_fire_at;
                e.fire_log.push(entry);
            }
        }
        self.persist();
    }

    fn fire_log(&self, id: &str) -> Vec<CronFireLogEntry> {
        self.entries
            .lock()
            .expect("CronStore mutex poisoned")
            .get(id)
            .map(|e| e.fire_log.clone())
            .unwrap_or_default()
    }
}

/// Poll every due, enabled record in `store`, fire it via `fire`, advance
/// its `next_fire_at` via [`super::advance_next_fire_at`], and record the
/// fire in the durable log. Returns the count of records fired.
///
/// **Anchor contract** (see [`super::advance_next_fire_at`]'s doc): the
/// scheduled anchor passed to `advance_next_fire_at` is the record's
/// pre-fire `next_fire_at` for calendar schedules (drift-free) and the
/// actual fire time (`now`) for interval schedules (exactly one catch-up).
/// `advance_next_fire_at` itself already encodes this per-variant, so
/// `tick()` only has to pass `now` — a calendar variant internally uses its
/// own `expr`/`timezone` against `now`... but that would reintroduce drift.
/// To honor the drift-free contract, `tick()` computes the anchor
/// per-variant explicitly rather than always passing `now`.
pub fn tick<S: CronStore + ?Sized>(
    store: &S,
    now: DateTime<Utc>,
    fire: impl Fn(&CronRecord) -> FireOutcome,
) -> usize {
    let due = store.due(now);
    let mut fired = 0usize;
    for record in due {
        let outcome = fire(&record);
        // Anchor: calendar schedules advance from the *scheduled* instant
        // (the record's pre-fire next_fire_at) to stay drift-free; interval
        // schedules advance from the *actual* fire instant (`now`) to
        // guarantee exactly one catch-up fire. See
        // `advance_next_fire_at`'s doc for the full rationale.
        let scheduled_at = record.next_fire_at;
        let advance_anchor = match &record.schedule {
            CronSchedule::Calendar { .. } => scheduled_at.unwrap_or(now),
            CronSchedule::Interval { .. } => now,
        };
        let next_fire_at = advance_next_fire_at(&record.schedule, advance_anchor);
        let entry = CronFireLogEntry::new(&outcome, now, scheduled_at);
        store.record_fire(&record.id, entry, next_fire_at);
        fired += 1;
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use std::sync::Mutex as StdMutex;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// An in-memory, test-only `CronStore` stub — no real file I/O.
    struct InMemoryCronStore {
        entries: StdMutex<HashMap<String, StoreEntry>>,
    }

    impl InMemoryCronStore {
        fn new() -> Self {
            InMemoryCronStore {
                entries: StdMutex::new(HashMap::new()),
            }
        }

        fn upsert(&self, record: CronRecord) {
            let mut entries = self.entries.lock().unwrap();
            entries.insert(
                record.id.clone(),
                StoreEntry {
                    record,
                    fire_log: Vec::new(),
                },
            );
        }
    }

    impl CronStore for InMemoryCronStore {
        fn list(&self) -> Vec<CronRecord> {
            self.entries
                .lock()
                .unwrap()
                .values()
                .map(|e| e.record.clone())
                .collect()
        }

        fn get(&self, id: &str) -> Option<CronRecord> {
            self.entries
                .lock()
                .unwrap()
                .get(id)
                .map(|e| e.record.clone())
        }

        fn due(&self, now: DateTime<Utc>) -> Vec<CronRecord> {
            self.entries
                .lock()
                .unwrap()
                .values()
                .filter(|e| e.record.enabled)
                .filter(|e| matches!(e.record.next_fire_at, Some(t) if t <= now))
                .map(|e| e.record.clone())
                .collect()
        }

        fn record_fire(
            &self,
            id: &str,
            entry: CronFireLogEntry,
            next_fire_at: Option<DateTime<Utc>>,
        ) {
            let mut entries = self.entries.lock().unwrap();
            if let Some(e) = entries.get_mut(id) {
                e.record.last_fired_at = Some(entry.fired_at);
                e.record.next_fire_at = next_fire_at;
                e.fire_log.push(entry);
            }
        }

        fn fire_log(&self, id: &str) -> Vec<CronFireLogEntry> {
            self.entries
                .lock()
                .unwrap()
                .get(id)
                .map(|e| e.fire_log.clone())
                .unwrap_or_default()
        }
    }

    fn interval_record(id: &str, next_fire_at: Option<DateTime<Utc>>, enabled: bool) -> CronRecord {
        CronRecord {
            id: id.to_string(),
            schedule: CronSchedule::Interval {
                every_ms: 60_000,
                first_fire_at: utc(2026, 1, 1, 0, 0, 0),
            },
            created_at: utc(2025, 12, 31, 0, 0, 0),
            enabled,
            last_fired_at: None,
            next_fire_at,
        }
    }

    #[test]
    fn tick_fires_exactly_due_records_and_advances_next_fire_at() {
        let store = InMemoryCronStore::new();
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record("due-one", Some(now), true));
        store.upsert(interval_record(
            "not-due",
            Some(now + Duration::minutes(10)),
            true,
        ));

        let fired = tick(&store, now, |_record| {
            FireOutcome::Reported("ok".to_string())
        });

        assert_eq!(fired, 1);
        let due_record = store.get("due-one").unwrap();
        assert_eq!(due_record.last_fired_at, Some(now));
        assert_eq!(due_record.next_fire_at, Some(now + Duration::seconds(60)));
        let log = store.fire_log("due-one");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome_kind, "reported");

        // The not-due record is untouched.
        let not_due = store.get("not-due").unwrap();
        assert_eq!(not_due.last_fired_at, None);
        assert!(store.fire_log("not-due").is_empty());
    }

    #[test]
    fn due_excludes_disabled_records() {
        let store = InMemoryCronStore::new();
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record("disabled", Some(now), false));

        let fired = tick(&store, now, |_record| FireOutcome::Silent);

        assert_eq!(fired, 0);
        let record = store.get("disabled").unwrap();
        assert_eq!(record.last_fired_at, None);
    }

    #[test]
    fn silent_fire_produces_log_entry_with_no_downstream_output() {
        let store = InMemoryCronStore::new();
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record("silent-cron", Some(now), true));

        let fire_calls = std::sync::atomic::AtomicUsize::new(0);
        let fired = tick(&store, now, |_record| {
            fire_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FireOutcome::Silent
        });

        assert_eq!(fired, 1);
        assert_eq!(fire_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let log = store.fire_log("silent-cron");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome_kind, "silent");
        assert_eq!(log[0].note, None, "a silent fire must not carry a note");
    }

    #[test]
    fn file_cron_store_survives_drop_and_reconstruction_from_same_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cron-store.json");

        let now = utc(2026, 1, 1, 0, 0, 0);
        {
            let store = FileCronStore::open(&path).expect("open store");
            store.upsert(interval_record("restart-cron", Some(now), true));
            let fired = tick(&store, now, |_record| {
                FireOutcome::Reported("first fire".to_string())
            });
            assert_eq!(fired, 1);
            // `store` is dropped here — simulating a process restart.
        }

        let reopened = FileCronStore::open(&path).expect("reopen store");
        let record = reopened
            .get("restart-cron")
            .expect("record should survive restart");
        assert_eq!(record.last_fired_at, Some(now));
        assert_eq!(record.next_fire_at, Some(now + Duration::seconds(60)));

        let log = reopened.fire_log("restart-cron");
        assert_eq!(log.len(), 1, "fire log should survive restart");
        assert_eq!(log[0].outcome_kind, "reported");
        assert_eq!(log[0].note, Some("first fire".to_string()));
    }

    #[test]
    fn file_cron_store_open_on_missing_path_starts_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist-yet.json");

        let store = FileCronStore::open(&path).expect("open store on missing file");
        assert!(store.list().is_empty());
    }
}
