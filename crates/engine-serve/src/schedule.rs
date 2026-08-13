//! `EN.6.G` task 2: the schedule source — a thin adapter over
//! `engine_core::cron`'s durable primitive (`EN.6.M`) that turns a `tick()`
//! fire into a `Schedule`-typed [`engine_contract::envelope::IngressEnvelope`]
//! and dispatches it through the exact same non-blocking-trigger sequence
//! `crate::http::post_events` uses (`dispatch_with_event` -> mint `run_id`
//! -> construct `SpawnedRun` -> `spawn_run`) — no self-directed HTTP call.
//!
//! Mirrors this repo's established injectable-seam convention (see
//! `engine_core::cron::store`'s `CronStore` doc comment, and
//! `crate::email_webhooks`'s `dispatch_and_spawn`): this module owns no
//! cron mechanics of its own (fire log, catch-up correctness, drift-free
//! anchoring all live in `engine_core::cron`) and no HTTP transport of its
//! own (dispatch reuses `crate::suspend::{SpawnedRun, spawn_run}` in
//! process).
//!
//! ## Two-part registration
//!
//! [`CronStore`] (the trait from `engine_core::cron`) has no `insert`/
//! `upsert` method on its trait surface — only the concrete
//! [`engine_core::cron::store::FileCronStore`] exposes `upsert` (by design;
//! see that type's doc comment: "callers that only hold `&dyn CronStore`
//! never need to add records, only tick through the ones already there").
//! So a [`ScheduleRegistry`] is built from a store that has *already* been
//! seeded with one [`CronRecord`] per entry (via `normalize_schedule` +
//! `FileCronStore::upsert`, done by [`build_seeded_registry`]) —
//! [`ScheduleRegistry::register`] only attaches the workflow-facing metadata
//! (`workflow_type`, `profile`, `data`) a `CronRecord` has no field for.
//!
//! ## Out of scope (per the block's Notes)
//!
//! A scheduling UI/CLI, distributed/multi-node scheduling or leader
//! election, backfill/catch-up semantics (owned by `EN.6.M`), and per-entry
//! retry policy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use engine_contract::envelope::{ChannelType, IngressEnvelope, SourcePayload};
use engine_core::cron::record::{CronRecord, FireOutcome};
use engine_core::cron::store::{CronStore, FileCronStore};
use engine_core::cron::CronSchedule;
use engine_core::{CancellationToken, PauseSignal};
use serde::Deserialize;
use uuid::Uuid;

use crate::dispatch::DispatchError;
use crate::http::AppState;

/// One registered scheduled entry: the (already-normalized) [`CronSchedule`]
/// its firing mechanics run through, plus the workflow-facing data a fire
/// dispatches. `data` is the caller-supplied payload merged into the
/// dispatched event alongside the built [`IngressEnvelope`] — mirrors
/// `crate::email_webhooks::dispatch_and_spawn`'s `event` construction.
#[derive(Debug, Clone)]
pub struct ScheduleEntry {
    pub cron_id: String,
    pub schedule: CronSchedule,
    pub workflow_type: String,
    pub profile: Option<String>,
    pub data: serde_json::Value,
    /// The first-fire anchor `engine_core::cron::normalize_schedule` computed
    /// for this entry at load time (against the `now` [`load_schedule_entries`]
    /// / [`load_schedule_entries_from_str`] were called with). Seeding a
    /// [`CronRecord`] from this entry must use this exact value rather than
    /// recomputing `normalize_schedule` against a second, later `now` — doing
    /// so would skew the first fire. `None` only for hand-built entries (e.g.
    /// test fixtures) that never went through the loader.
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// The raw shape a `planning/harness.json` `schedule[]` entry deserializes
/// from — see [`load_schedule_entries_from_str`].
#[derive(Debug, Clone, Deserialize)]
struct RawScheduleEntry {
    cron_id: String,
    /// Mutually exclusive with `every_ms`, normalized the same way
    /// `engine_core::cron::normalize_schedule` validates a `RawSchedule`.
    #[serde(default)]
    cron_expr: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    every_ms: Option<i64>,
    workflow_type: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    data: serde_json::Value,
}

/// A defect found while loading `planning/harness.json`'s `schedule[]`
/// entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadScheduleError {
    /// The file could not be read (missing is NOT an error — see
    /// [`load_schedule_entries`] — this variant is for a real I/O failure).
    Io(String),
    /// The file was not valid JSON, or the `schedule` key was not an array
    /// of the expected shape.
    Parse(String),
    /// One entry's `cron_expr`/`timezone`/`every_ms` fields failed
    /// [`engine_core::cron::normalize_schedule`].
    InvalidSchedule { cron_id: String, message: String },
}

impl std::fmt::Display for LoadScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadScheduleError::Io(message) => write!(f, "failed to read harness.json: {message}"),
            LoadScheduleError::Parse(message) => {
                write!(
                    f,
                    "failed to parse harness.json schedule entries: {message}"
                )
            }
            LoadScheduleError::InvalidSchedule { cron_id, message } => {
                write!(
                    f,
                    "schedule entry {cron_id:?} has an invalid schedule: {message}"
                )
            }
        }
    }
}

impl std::error::Error for LoadScheduleError {}

/// Load `harness.json`'s top-level `schedule` array (documented in
/// `planning/harness.json` alongside the other stack-facing knobs), the
/// same read-the-whole-file-then-index-by-key pattern
/// `engine_core::policy::profiles::read_harness_policy_defaults_from` uses
/// for `<workflow_key>.policy`/`.profiles` — except `schedule` is a
/// top-level key, not workflow-keyed, since a scheduled fire targets
/// whatever `workflow_type` its own entry names. A missing file or a
/// missing/absent `schedule` key both yield an empty `Vec` (no entries
/// configured is not an error); a present-but-malformed `schedule` key is.
pub fn load_schedule_entries(harness_path: &Path) -> Result<Vec<ScheduleEntry>, LoadScheduleError> {
    if !harness_path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(harness_path)
        .map_err(|err| LoadScheduleError::Io(err.to_string()))?;
    load_schedule_entries_from_str(&raw, Utc::now())
}

/// The pure parsing half of [`load_schedule_entries`] — takes the raw JSON
/// text and `now` (for entries whose interval schedule needs a first-fire
/// anchor) directly, so every branch is unit-testable without touching the
/// filesystem.
fn load_schedule_entries_from_str(
    raw: &str,
    now: DateTime<Utc>,
) -> Result<Vec<ScheduleEntry>, LoadScheduleError> {
    let harness: serde_json::Value =
        serde_json::from_str(raw).map_err(|err| LoadScheduleError::Parse(err.to_string()))?;

    // `schedule.entries` — a sibling `_comment` key documents the knob (see
    // `planning/harness.json`), matching the `_comment`-alongside-data
    // convention `engine_core::policy::profiles::read_harness_profiles_from`
    // already uses for `<workflow_key>.profiles`.
    let Some(entries_value) = harness.get("schedule").and_then(|v| v.get("entries")) else {
        return Ok(Vec::new());
    };

    let raw_entries: Vec<RawScheduleEntry> = serde_json::from_value(entries_value.clone())
        .map_err(|err| LoadScheduleError::Parse(err.to_string()))?;

    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw_entry in raw_entries {
        let normalized = engine_core::cron::RawSchedule {
            cron_expr: raw_entry.cron_expr,
            timezone: raw_entry.timezone,
            every_ms: raw_entry.every_ms,
            first_fire_at: None,
        };
        let (schedule, next_fire_at) = engine_core::cron::normalize_schedule(normalized, now)
            .map_err(|err| LoadScheduleError::InvalidSchedule {
                cron_id: raw_entry.cron_id.clone(),
                message: err.to_string(),
            })?;

        entries.push(ScheduleEntry {
            cron_id: raw_entry.cron_id,
            schedule,
            workflow_type: raw_entry.workflow_type,
            profile: raw_entry.profile,
            data: raw_entry.data,
            next_fire_at: Some(next_fire_at),
        });
    }

    Ok(entries)
}

/// The "two-part registration" caller named in this module's own doc: turn
/// (`harness_path`, `store_path`) into a fully-seeded, fully-registered
/// [`ScheduleRegistry`].
///
/// Loads `harness_path`'s `schedule.entries` via [`load_schedule_entries`],
/// opens (or creates) a [`FileCronStore`] at `store_path`, `upsert`s one
/// [`CronRecord`] per entry, then [`ScheduleRegistry::register`]s each
/// entry's workflow-facing metadata on top.
///
/// **Restart safety.** `FileCronStore::upsert` replaces the whole record,
/// so a re-seed (e.g. on process restart, against the same `store_path`)
/// must not clobber a record's firing history: for a `cron_id` the store
/// already has a record for, `created_at`/`last_fired_at`/`next_fire_at`
/// are carried over from the existing record rather than reset to the
/// load-time anchor — otherwise a restart could either immediately
/// re-fire an entry that already fired, or skip ahead past one that
/// hadn't. Only a brand-new `cron_id` gets `load_schedule_entries`'s
/// load-time `next_fire_at` (see [`ScheduleEntry::next_fire_at`]'s doc for
/// why that must be the loader's value, not a value recomputed here
/// against a second, later `now`).
///
/// Zero configured entries yields an empty, freshly-opened store and an
/// empty registry — no error, and [`FileCronStore::open`] is the only
/// filesystem touch (it creates nothing until the first `upsert`).
pub fn build_seeded_registry(
    harness_path: &Path,
    store_path: &Path,
) -> Result<ScheduleRegistry, LoadScheduleError> {
    let entries = load_schedule_entries(harness_path)?;
    let store =
        FileCronStore::open(store_path).map_err(|err| LoadScheduleError::Io(err.to_string()))?;

    let seeded_at = Utc::now();
    for entry in &entries {
        let (created_at, last_fired_at, next_fire_at) = match store.get(&entry.cron_id) {
            Some(existing) => (
                existing.created_at,
                existing.last_fired_at,
                existing.next_fire_at,
            ),
            None => (seeded_at, None, entry.next_fire_at),
        };
        store.upsert(CronRecord {
            id: entry.cron_id.clone(),
            schedule: entry.schedule.clone(),
            created_at,
            enabled: true,
            last_fired_at,
            next_fire_at,
        });
    }

    let mut registry = ScheduleRegistry::new(Arc::new(store));
    for entry in entries {
        registry.register(entry);
    }

    Ok(registry)
}

/// Behavior-stable fallback for `schedule.poll_interval_ms` when
/// `harness.json` doesn't set it (standing rule 6 — a knob's built-in
/// default must not change what an existing run does; since the default
/// `schedule.entries` is `[]`, no interval value changes any behavior until
/// an entry is actually configured). Comfortably under the 60_000ms
/// `every_ms` floor `schedule.entries` already enforces, so the fastest
/// legal interval entry cannot fire and clear before the loop next polls.
const DEFAULT_SCHEDULE_POLL_INTERVAL_MS: u64 = 15_000;

/// Behavior-stable fallback for `schedule.store_path` when `harness.json`
/// doesn't set it: a `cron-store.json` file next to `harness_path` itself,
/// so a fresh checkout with no `schedule.store_path` configured still has a
/// sane, discoverable place for [`FileCronStore`] to persist to.
fn default_schedule_store_path(harness_path: &Path) -> PathBuf {
    harness_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("cron-store.json")
}

/// Read the `schedule.poll_interval_ms` / `schedule.store_path` knobs out of
/// `harness_path` (documented in `planning/harness.json` alongside
/// `schedule.entries`), falling back to
/// [`DEFAULT_SCHEDULE_POLL_INTERVAL_MS`] / [`default_schedule_store_path`]
/// for whichever is absent. A missing `harness_path` or a missing/absent
/// `schedule` key both yield the defaults (mirrors
/// [`load_schedule_entries`]'s own "missing is not an error" rule) — only a
/// present-but-malformed value is an error.
fn read_schedule_loop_config(
    harness_path: &Path,
) -> Result<(Duration, PathBuf), LoadScheduleError> {
    let default_store_path = default_schedule_store_path(harness_path);
    if !harness_path.exists() {
        return Ok((
            Duration::from_millis(DEFAULT_SCHEDULE_POLL_INTERVAL_MS),
            default_store_path,
        ));
    }

    let raw = std::fs::read_to_string(harness_path)
        .map_err(|err| LoadScheduleError::Io(err.to_string()))?;
    let harness: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| LoadScheduleError::Parse(err.to_string()))?;

    let Some(schedule) = harness.get("schedule") else {
        return Ok((
            Duration::from_millis(DEFAULT_SCHEDULE_POLL_INTERVAL_MS),
            default_store_path,
        ));
    };

    let poll_interval_ms = match schedule.get("poll_interval_ms") {
        None | Some(serde_json::Value::Null) => DEFAULT_SCHEDULE_POLL_INTERVAL_MS,
        Some(value) => value.as_u64().ok_or_else(|| {
            LoadScheduleError::Parse("schedule.poll_interval_ms must be a positive integer".into())
        })?,
    };

    let store_path = match schedule.get("store_path") {
        None | Some(serde_json::Value::Null) => default_store_path,
        Some(value) => PathBuf::from(value.as_str().ok_or_else(|| {
            LoadScheduleError::Parse("schedule.store_path must be a string".into())
        })?),
    };

    Ok((Duration::from_millis(poll_interval_ms), store_path))
}

/// A handle to the background schedule tick loop [`spawn_schedule_loop`]
/// spawned. Mirrors `crate::durable::DurableHandle`'s "hold or drop" shape:
/// the caller may hold this to [`abort`](Self::abort) the loop (e.g. on
/// shutdown) or drop it — dropping does **not** stop the loop (a
/// `tokio::task::JoinHandle` detaches on drop), matching how the durable
/// writer's own background task is not torn down by dropping its handle
/// either.
pub struct ScheduleLoopHandle {
    task: tokio::task::JoinHandle<()>,
}

impl ScheduleLoopHandle {
    /// Stop the background tick loop.
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// The spawnable schedule bootstrap: seed a [`ScheduleRegistry`] from
/// `harness_path` (via [`build_seeded_registry`]) and, if any entries were
/// configured, `tokio::spawn` a background loop that polls
/// [`ScheduleRegistry::tick`] on a `tokio::time::interval` and dispatches
/// each fire through [`dispatch_scheduled_entry`].
///
/// **Zero entries is a no-op**: nothing is spawned and nothing is logged —
/// adding this call to a startup path does not change the behavior of any
/// run that has no `schedule.entries` configured (the harness.json default).
///
/// **The tick stays off the async path.** `ScheduleRegistry::tick` calls
/// into `FileCronStore`, which persists to disk synchronously
/// (`engine_core::cron::store`), so each poll runs inside
/// `tokio::task::spawn_blocking` rather than directly in the loop's async
/// block — a slow disk write must not stall every other task on this
/// process's runtime.
///
/// The poll interval and the store path are both knobs (standing rule 6),
/// read from `harness_path`'s `schedule` block via
/// [`read_schedule_loop_config`] — see [`DEFAULT_SCHEDULE_POLL_INTERVAL_MS`]
/// / [`default_schedule_store_path`] for their built-in defaults.
///
/// Returns `Ok(None)` (nothing spawned) for zero configured entries, or
/// `Ok(Some(handle))` once the loop is running. The one remaining wiring
/// step — calling this from a live process's startup path — is a different
/// repo's change (`bastion`'s `serve/mod.rs`, alongside its existing
/// `spawn_durable_writer` call) and is out of scope here; see this spec's
/// Notes.
pub fn spawn_schedule_loop(
    harness_path: &Path,
    state: Arc<AppState>,
) -> Result<Option<ScheduleLoopHandle>, LoadScheduleError> {
    let (poll_interval, store_path) = read_schedule_loop_config(harness_path)?;
    let registry = build_seeded_registry(harness_path, &store_path)?;

    if registry.is_empty() {
        return Ok(None);
    }

    let entry_count = registry.len();
    let registry = Arc::new(registry);

    let task = tokio::spawn(async move {
        println!("schedule loop: starting with {entry_count} entries");

        let mut interval = tokio::time::interval(poll_interval);
        loop {
            interval.tick().await;

            let registry = registry.clone();
            let state = state.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let now = Utc::now();
                registry.tick(now, |entry| dispatch_scheduled_entry(&state, entry, now))
            })
            .await;
        }
    });

    Ok(Some(ScheduleLoopHandle { task }))
}

/// The seam a [`ScheduleRegistry::tick`] fire dispatches through. `entries`
/// is the workflow-facing metadata attached via [`ScheduleRegistry::register`];
/// `store` holds the pure cron mechanics (`CronRecord`, fire log,
/// drift-free `next_fire_at` advancement) that `engine_core::cron::tick`
/// already implements — this struct adds nothing to that beyond the lookup.
pub struct ScheduleRegistry {
    store: Arc<dyn CronStore>,
    entries: HashMap<String, ScheduleEntry>,
}

impl ScheduleRegistry {
    /// Build an (initially empty) registry over `store` — the store is
    /// expected to already hold one [`CronRecord`] per entry that will be
    /// [`register`](Self::register)ed (see the module doc's "Two-part
    /// registration" section).
    pub fn new(store: Arc<dyn CronStore>) -> Self {
        ScheduleRegistry {
            store,
            entries: HashMap::new(),
        }
    }

    /// Attach `entry`'s workflow-facing metadata under its `cron_id`,
    /// overwriting any prior registration for the same id.
    pub fn register(&mut self, entry: ScheduleEntry) {
        self.entries.insert(entry.cron_id.clone(), entry);
    }

    /// A registered entry by its `cron_id`, if any.
    pub fn get(&self, cron_id: &str) -> Option<&ScheduleEntry> {
        self.entries.get(cron_id)
    }

    /// The durable store this registry ticks through.
    pub fn store(&self) -> &Arc<dyn CronStore> {
        &self.store
    }

    /// The number of registered entries — used by [`spawn_schedule_loop`] to
    /// decide whether spawning a tick loop is worthwhile at all (zero
    /// entries must spawn nothing, per this module's Acceptance Criteria).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff no entries are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Poll the store for every due, enabled [`CronRecord`] as of `now`,
    /// calling `dispatch` with the matching registered [`ScheduleEntry`] for
    /// each — delegating every firing/catch-up mechanic (drift-free
    /// advancement, fire-log durability) to
    /// [`engine_core::cron::tick`]. A due record with no matching
    /// registration (should not happen when the store and `entries` are
    /// seeded together, but defensive against drift between the two) fires
    /// [`FireOutcome::Silent`] rather than panicking or fabricating a
    /// dispatch. Returns the count of records fired (matching
    /// `engine_core::cron::tick`'s own return).
    pub fn tick(
        &self,
        now: DateTime<Utc>,
        dispatch: impl Fn(&ScheduleEntry) -> FireOutcome,
    ) -> usize {
        engine_core::cron::store::tick(self.store.as_ref(), now, |record: &CronRecord| {
            match self.entries.get(&record.id) {
                Some(entry) => dispatch(entry),
                None => FireOutcome::Silent,
            }
        })
    }
}

/// Build the `Schedule`-typed [`IngressEnvelope`] a scheduled fire carries
/// — `source` is [`SourcePayload::WorkflowTrigger`] naming the entry's
/// target `workflow_type` and its configured `data`, matching how
/// `crate::email_webhooks`'s inbound-email adapter shapes its own envelope
/// before dispatch.
fn build_schedule_envelope(entry: &ScheduleEntry, fired_at: DateTime<Utc>) -> IngressEnvelope {
    IngressEnvelope {
        envelope_id: format!("schedule-{}-{}", entry.cron_id, fired_at.timestamp_millis()),
        channel_type: ChannelType::Schedule,
        sender_id: None,
        reply_context: None,
        timestamp: fired_at.to_rfc3339(),
        source: SourcePayload::WorkflowTrigger {
            workflow_type: entry.workflow_type.clone(),
            event: entry.data.clone(),
        },
        raw_payload: serde_json::json!({ "cron_id": entry.cron_id }),
    }
}

/// Dispatch one scheduled fire: build its `Schedule` envelope, dispatch
/// `entry.workflow_type` through `state.dispatcher.dispatch_with_event`
/// (exactly `crate::http::post_events`'s dispatch call), then follow that
/// same non-blocking-trigger sequence — mint a `run_id`, register a
/// cancellation token + pause signal, seed the default HTTP budget, record
/// `live_run_metadata`, hand off to `crate::suspend::spawn_run` — instead of
/// making an HTTP call against this same server.
///
/// Returns [`FireOutcome::Reported`] naming the minted `run_id` on a
/// successful dispatch, or naming the dispatch failure otherwise (an
/// unregistered `workflow_type` or a policy-resolution failure) — either
/// way something happened worth a fire-log note, so this never returns
/// [`FireOutcome::Silent`] (the silence protocol is for a fire that
/// legitimately has nothing to report, which a dispatch attempt always
/// does).
pub fn dispatch_scheduled_entry(
    state: &AppState,
    entry: &ScheduleEntry,
    fired_at: DateTime<Utc>,
) -> FireOutcome {
    let envelope = build_schedule_envelope(entry, fired_at);
    let event = serde_json::json!({
        "envelope": envelope,
        "profile": entry.profile,
    });

    let workflow = match state
        .dispatcher
        .dispatch_with_event(&entry.workflow_type, &event)
    {
        Ok(workflow) => workflow,
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            return FireOutcome::Reported(format!(
                "dispatch failed: unknown workflow_type {workflow_type:?}"
            ));
        }
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            return FireOutcome::Reported(format!("dispatch failed: policy resolution: {message}"));
        }
    };

    let run_id = Uuid::new_v4();
    let workflow_type = entry.workflow_type.clone();
    let data = event.clone();
    let live = state.live.clone();
    let runs = state.runs.clone();
    let durable_handle = state.durable.clone();

    let token = CancellationToken::new();
    runs.register(run_id, token.clone());

    let pause = PauseSignal::new();
    crate::suspend::register_pause_signal(run_id, pause.clone());

    let budget = crate::http::default_budget_from_env();

    let created_at = Utc::now();
    crate::http::live_run_metadata()
        .write()
        .expect("live run metadata lock poisoned on write")
        .insert(run_id, (workflow_type.clone(), created_at));

    crate::suspend::spawn_run(crate::suspend::SpawnedRun {
        run_id,
        workflow,
        workflow_type,
        data: data.clone(),
        created_at,
        start: crate::suspend::RunStart::Fresh(data),
        live,
        durable: durable_handle,
        runs,
        token,
        pause,
        budget,
    });

    FireOutcome::Reported(format!(
        "dispatched run {run_id} for cron {}",
        entry.cron_id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use engine_core::cron::record::CronFireLogEntry;
    use std::sync::Mutex as StdMutex;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// An in-memory, test-only `CronStore` — no real file I/O. Mirrors
    /// `engine_core::cron::store::tests::InMemoryCronStore` (that one is
    /// private to its own module, so this crate keeps its own copy rather
    /// than reaching into another crate's `#[cfg(test)]` internals).
    struct InMemoryCronStore {
        records: StdMutex<HashMap<String, CronRecord>>,
    }

    impl InMemoryCronStore {
        fn new() -> Self {
            InMemoryCronStore {
                records: StdMutex::new(HashMap::new()),
            }
        }

        fn upsert(&self, record: CronRecord) {
            self.records
                .lock()
                .unwrap()
                .insert(record.id.clone(), record);
        }
    }

    impl CronStore for InMemoryCronStore {
        fn list(&self) -> Vec<CronRecord> {
            self.records.lock().unwrap().values().cloned().collect()
        }

        fn get(&self, id: &str) -> Option<CronRecord> {
            self.records.lock().unwrap().get(id).cloned()
        }

        fn due(&self, now: DateTime<Utc>) -> Vec<CronRecord> {
            self.records
                .lock()
                .unwrap()
                .values()
                .filter(|record| record.enabled)
                .filter(|record| matches!(record.next_fire_at, Some(t) if t <= now))
                .cloned()
                .collect()
        }

        fn record_fire(
            &self,
            id: &str,
            entry: CronFireLogEntry,
            next_fire_at: Option<DateTime<Utc>>,
        ) {
            let mut records = self.records.lock().unwrap();
            if let Some(record) = records.get_mut(id) {
                record.last_fired_at = Some(entry.fired_at);
                record.next_fire_at = next_fire_at;
            }
        }

        fn fire_log(&self, _id: &str) -> Vec<CronFireLogEntry> {
            Vec::new()
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

    fn schedule_entry(cron_id: &str) -> ScheduleEntry {
        ScheduleEntry {
            cron_id: cron_id.to_string(),
            schedule: CronSchedule::Interval {
                every_ms: 60_000,
                first_fire_at: utc(2026, 1, 1, 0, 0, 0),
            },
            workflow_type: "CONTENT_PIPELINE".to_string(),
            profile: Some("cheap-fast".to_string()),
            data: serde_json::json!({ "source": "daily-digest" }),
            next_fire_at: Some(utc(2026, 1, 1, 0, 0, 0)),
        }
    }

    #[test]
    fn a_registered_entry_fires_on_a_stubbed_clock_and_carries_its_profile() {
        let store = Arc::new(InMemoryCronStore::new());
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record("due-entry", Some(now), true));

        let mut registry = ScheduleRegistry::new(store);
        registry.register(schedule_entry("due-entry"));

        let fired_profiles: Arc<StdMutex<Vec<Option<String>>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let fired_profiles_for_closure = fired_profiles.clone();

        let fired = registry.tick(now, move |entry| {
            fired_profiles_for_closure
                .lock()
                .unwrap()
                .push(entry.profile.clone());
            FireOutcome::Reported(format!("fired {}", entry.cron_id))
        });

        assert_eq!(fired, 1);
        assert_eq!(
            fired_profiles.lock().unwrap().as_slice(),
            &[Some("cheap-fast".to_string())]
        );
    }

    #[test]
    fn a_disabled_entry_never_fires() {
        let store = Arc::new(InMemoryCronStore::new());
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record("disabled-entry", Some(now), false));

        let mut registry = ScheduleRegistry::new(store);
        registry.register(schedule_entry("disabled-entry"));

        let fire_calls = std::sync::atomic::AtomicUsize::new(0);
        let fired = registry.tick(now, |_entry| {
            fire_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FireOutcome::Silent
        });

        assert_eq!(fired, 0);
        assert_eq!(fire_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn a_not_yet_due_entry_never_fires() {
        let store = Arc::new(InMemoryCronStore::new());
        let now = utc(2026, 1, 1, 0, 0, 0);
        store.upsert(interval_record(
            "future-entry",
            Some(now + Duration::minutes(10)),
            true,
        ));

        let mut registry = ScheduleRegistry::new(store);
        registry.register(schedule_entry("future-entry"));

        let fired = registry.tick(now, |_entry| FireOutcome::Silent);
        assert_eq!(fired, 0);
    }

    #[test]
    fn load_schedule_entries_from_str_returns_empty_vec_when_schedule_key_absent() {
        let entries = load_schedule_entries_from_str("{}", Utc::now()).expect("should parse");
        assert!(entries.is_empty());
    }

    #[test]
    fn load_schedule_entries_from_str_parses_calendar_and_interval_entries() {
        let raw = serde_json::json!({
            "schedule": {
                "entries": [
                    {
                        "cron_id": "daily-digest",
                        "cron_expr": "0 0 12 * * *",
                        "timezone": "UTC",
                        "workflow_type": "CONTENT_PIPELINE",
                        "profile": "cheap-fast",
                        "data": { "source": "daily-digest" }
                    },
                    {
                        "cron_id": "hourly-check",
                        "every_ms": 3_600_000,
                        "workflow_type": "CONTENT_PIPELINE"
                    }
                ]
            }
        })
        .to_string();

        let entries =
            load_schedule_entries_from_str(&raw, utc(2026, 1, 1, 0, 0, 0)).expect("should parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].cron_id, "daily-digest");
        assert_eq!(entries[0].profile, Some("cheap-fast".to_string()));
        assert!(matches!(entries[0].schedule, CronSchedule::Calendar { .. }));
        assert!(entries[0].next_fire_at.is_some());
        assert_eq!(entries[1].cron_id, "hourly-check");
        assert_eq!(entries[1].profile, None);
        assert!(matches!(entries[1].schedule, CronSchedule::Interval { .. }));
        assert!(entries[1].next_fire_at.is_some());
    }

    #[test]
    fn load_schedule_entries_from_str_carries_normalize_schedules_anchor_for_the_same_now() {
        let now = utc(2026, 1, 1, 0, 0, 0);
        let raw = serde_json::json!({
            "schedule": {
                "entries": [
                    {
                        "cron_id": "hourly-check",
                        "every_ms": 3_600_000,
                        "workflow_type": "CONTENT_PIPELINE"
                    }
                ]
            }
        })
        .to_string();

        let entries = load_schedule_entries_from_str(&raw, now).expect("should parse");
        assert_eq!(entries.len(), 1);

        // Independently recompute what normalize_schedule returns for the
        // same raw shape and the same `now` — the loaded entry's
        // `next_fire_at` must match this exactly, not a value recomputed
        // against some later `now`.
        let expected = engine_core::cron::normalize_schedule(
            engine_core::cron::RawSchedule {
                cron_expr: None,
                timezone: None,
                every_ms: Some(3_600_000),
                first_fire_at: None,
            },
            now,
        )
        .expect("valid schedule")
        .1;

        assert_eq!(entries[0].next_fire_at, Some(expected));
    }

    #[test]
    fn load_schedule_entries_from_str_rejects_an_invalid_schedule_naming_the_cron_id() {
        let raw = serde_json::json!({
            "schedule": {
                "entries": [
                    {
                        "cron_id": "too-fast",
                        "every_ms": 1_000,
                        "workflow_type": "CONTENT_PIPELINE"
                    }
                ]
            }
        })
        .to_string();

        let err = load_schedule_entries_from_str(&raw, Utc::now()).unwrap_err();
        match err {
            LoadScheduleError::InvalidSchedule { cron_id, .. } => {
                assert_eq!(cron_id, "too-fast");
            }
            other => panic!("expected InvalidSchedule, got {other:?}"),
        }
    }

    #[test]
    fn load_schedule_entries_returns_empty_vec_for_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.json");
        let entries = load_schedule_entries(&path).expect("missing file is not an error");
        assert!(entries.is_empty());
    }

    fn write_harness_with_one_entry(dir: &Path, cron_id: &str) -> std::path::PathBuf {
        let harness_path = dir.join("harness.json");
        let raw = serde_json::json!({
            "schedule": {
                "entries": [
                    {
                        "cron_id": cron_id,
                        "every_ms": 3_600_000,
                        "workflow_type": "CONTENT_PIPELINE"
                    }
                ]
            }
        });
        std::fs::write(&harness_path, raw.to_string()).expect("write harness.json");
        harness_path
    }

    #[test]
    fn build_seeded_registry_seeds_a_store_record_and_registers_metadata_for_each_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let harness_path = write_harness_with_one_entry(dir.path(), "hourly-check");
        let store_path = dir.path().join("cron-store.json");

        let registry =
            build_seeded_registry(&harness_path, &store_path).expect("should build registry");

        let record = registry
            .store()
            .get("hourly-check")
            .expect("record should be seeded in the store");
        assert!(record.enabled);
        assert!(record.next_fire_at.is_some());

        let entry = registry
            .get("hourly-check")
            .expect("entry should be registered");
        assert_eq!(entry.next_fire_at, record.next_fire_at);
        assert_eq!(entry.workflow_type, "CONTENT_PIPELINE");
    }

    #[test]
    fn build_seeded_registry_with_zero_entries_yields_an_empty_registry_and_no_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let harness_path = dir.path().join("harness.json");
        std::fs::write(&harness_path, "{}").expect("write harness.json");
        let store_path = dir.path().join("cron-store.json");

        let registry =
            build_seeded_registry(&harness_path, &store_path).expect("should build registry");

        assert!(registry.get("anything").is_none());
        assert!(registry.store().list().is_empty());
    }

    #[test]
    fn build_seeded_registry_reseeding_preserves_last_fired_at_and_next_fire_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let harness_path = write_harness_with_one_entry(dir.path(), "hourly-check");
        let store_path = dir.path().join("cron-store.json");

        let first = build_seeded_registry(&harness_path, &store_path).expect("first seed");
        let fired_at = utc(2026, 1, 1, 0, 0, 0);
        let post_fire_next = utc(2026, 1, 1, 1, 0, 0);
        first.store().record_fire(
            "hourly-check",
            CronFireLogEntry {
                fired_at,
                scheduled_at: Some(fired_at),
                outcome_kind: "reported",
                note: Some("fired".to_string()),
            },
            Some(post_fire_next),
        );

        let second =
            build_seeded_registry(&harness_path, &store_path).expect("re-seed should succeed");
        let record = second
            .store()
            .get("hourly-check")
            .expect("record should still exist");

        assert_eq!(record.last_fired_at, Some(fired_at));
        assert_eq!(record.next_fire_at, Some(post_fire_next));
    }

    #[test]
    fn read_schedule_loop_config_falls_back_to_both_defaults_when_unconfigured() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Three ways to be "unconfigured", all of which must yield the
        // behavior-stable defaults rather than an error: no file at all, a
        // file with no `schedule` key, and explicit nulls.
        let missing = dir.path().join("absent-harness.json");

        let no_key = dir.path().join("no-key.json");
        std::fs::write(&no_key, "{}").expect("write no-key harness");

        let explicit_nulls = dir.path().join("nulls.json");
        std::fs::write(
            &explicit_nulls,
            r#"{ "schedule": { "poll_interval_ms": null, "store_path": null } }"#,
        )
        .expect("write nulls harness");

        for harness_path in [&missing, &no_key, &explicit_nulls] {
            let (interval, store_path) =
                read_schedule_loop_config(harness_path).expect("defaults are not an error");
            assert_eq!(
                interval,
                std::time::Duration::from_millis(DEFAULT_SCHEDULE_POLL_INTERVAL_MS),
                "unconfigured poll interval should fall back to the built-in default"
            );
            assert_eq!(
                store_path,
                default_schedule_store_path(harness_path),
                "unconfigured store path should fall back beside harness.json"
            );
        }

        // The default must stay under the 60_000ms `every_ms` floor that
        // `schedule.entries` enforces, or the fastest legal interval entry
        // could come due and be missed between polls.
        const {
            assert!(
                DEFAULT_SCHEDULE_POLL_INTERVAL_MS < 60_000,
                "default poll interval must not exceed the every_ms floor"
            );
        }
    }

    #[test]
    fn read_schedule_loop_config_honours_configured_values_and_rejects_malformed_ones() {
        let dir = tempfile::tempdir().expect("tempdir");

        let configured = dir.path().join("configured.json");
        std::fs::write(
            &configured,
            r#"{ "schedule": { "poll_interval_ms": 5000, "store_path": "/tmp/somewhere/cron.json" } }"#,
        )
        .expect("write configured harness");

        let (interval, store_path) =
            read_schedule_loop_config(&configured).expect("configured values should parse");
        assert_eq!(interval, std::time::Duration::from_millis(5000));
        assert_eq!(
            store_path,
            std::path::PathBuf::from("/tmp/somewhere/cron.json")
        );

        // Present-but-malformed is an error, unlike absent (the distinction
        // `load_schedule_entries` already draws).
        let bad_interval = dir.path().join("bad-interval.json");
        std::fs::write(
            &bad_interval,
            r#"{ "schedule": { "poll_interval_ms": "soon" } }"#,
        )
        .expect("write bad interval");
        assert!(read_schedule_loop_config(&bad_interval).is_err());

        let bad_path = dir.path().join("bad-path.json");
        std::fs::write(&bad_path, r#"{ "schedule": { "store_path": 42 } }"#)
            .expect("write bad path");
        assert!(read_schedule_loop_config(&bad_path).is_err());
    }

    #[test]
    fn build_schedule_envelope_carries_schedule_channel_type_and_workflow_trigger_source() {
        let entry = schedule_entry("daily-digest");
        let fired_at = utc(2026, 1, 1, 12, 0, 0);
        let envelope = build_schedule_envelope(&entry, fired_at);

        assert_eq!(envelope.channel_type, ChannelType::Schedule);
        match envelope.source {
            SourcePayload::WorkflowTrigger {
                workflow_type,
                event,
            } => {
                assert_eq!(workflow_type, "CONTENT_PIPELINE");
                assert_eq!(event, entry.data);
            }
            other => panic!("expected WorkflowTrigger source, got {other:?}"),
        }
    }

    /// A node that records the event it was constructed against into a
    /// shared sink and does nothing else — the observation point for
    /// "exactly one run was dispatched carrying this event", mirroring
    /// `crate::email_webhooks`'s own `MarkerNode` test fixture.
    struct MarkerNode;

    #[async_trait::async_trait]
    impl engine_core::Node for MarkerNode {
        async fn process(
            &self,
            mut ctx: engine_contract::TaskContext,
        ) -> Result<engine_contract::TaskContext, engine_core::NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    fn fixture_schema(workflow_type: &str) -> engine_core::WorkflowSchema {
        let mut nodes = HashMap::new();
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        engine_core::WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
    }

    /// An `AppState` whose sole registration is `CONTENT_PIPELINE` against a
    /// fixture single-node graph, recording every dispatched event into
    /// `recorded` for assertion.
    fn test_app_state() -> (AppState, Arc<StdMutex<Vec<serde_json::Value>>>) {
        let recorded: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let recorded_for_factory = recorded.clone();

        let mut dispatcher = crate::dispatch::Dispatcher::new();
        dispatcher.register(
            fixture_schema("CONTENT_PIPELINE"),
            Box::new(move |event: &serde_json::Value| {
                recorded_for_factory
                    .lock()
                    .expect("recorded lock poisoned")
                    .push(event.clone());
                let mut registry = engine_core::NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(engine_core::Workflow::new(
                    registry,
                    fixture_schema("CONTENT_PIPELINE"),
                ))
            }),
        );

        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: crate::live_state::LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: crate::abort::RunRegistry::new(),
            api_key: "test-key".to_string(),
        };
        (state, recorded)
    }

    #[actix_web::test]
    async fn dispatch_scheduled_entry_dispatches_exactly_one_run_reusing_dispatch_with_event_and_spawn_run(
    ) {
        let (state, recorded) = test_app_state();
        let entry = schedule_entry("daily-digest");
        let fired_at = utc(2026, 1, 1, 12, 0, 0);

        let outcome = dispatch_scheduled_entry(&state, &entry, fired_at);
        assert!(matches!(outcome, FireOutcome::Reported(_)));

        let events = recorded.lock().expect("recorded lock poisoned").clone();
        assert_eq!(events.len(), 1, "exactly one run should be dispatched");
        assert_eq!(events[0]["profile"], serde_json::json!("cheap-fast"));
        assert_eq!(
            events[0]["envelope"]["channel_type"],
            serde_json::json!("schedule")
        );
    }

    #[actix_web::test]
    async fn dispatch_scheduled_entry_reports_unknown_workflow_type_without_panicking() {
        let (state, recorded) = test_app_state();
        let mut entry = schedule_entry("unregistered-entry");
        entry.workflow_type = "NOT_REGISTERED".to_string();
        let fired_at = utc(2026, 1, 1, 12, 0, 0);

        let outcome = dispatch_scheduled_entry(&state, &entry, fired_at);
        match outcome {
            FireOutcome::Reported(message) => {
                assert!(message.contains("unknown workflow_type"));
            }
            FireOutcome::Silent => panic!("expected a Reported outcome naming the failure"),
        }
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }
}
