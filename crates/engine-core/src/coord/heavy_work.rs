//! `coord::heavy_work` — a generic heavy-work queue for Rust SDLC check runs.
//!
//! `EN.17.I` task 1. Concurrent Rust lanes compiling at once is this fleet's measured
//! contention failure (see the block record's `why`), and the only gate today
//! (`scripts/fleet_build.py`) covers one repo and leaks: its `_sweep_stale` deletes any permit
//! whose `started_at` is older than `FLEET_BUILD_TTL_SECONDS` (300) even when the holder pid is
//! still alive, so a live build over five minutes loses its slot to the next acquirer.
//!
//! This module fixes that by reclaiming on LIVENESS (holder pid + heartbeat), never on start
//! age. Task 1 lands the job record shape, its on-disk store (one JSON file per job under
//! `<lock_dir>/heavy-work/jobs/`, written temp-file-plus-rename), the `[heavy_work]` `brain.toml`
//! config parse, and the injectable `FreeMemoryProbe` seam with its `vm_stat`-based default. The
//! admission/reclaim algorithm and the per-lock-dir worker (`HeavyWorkQueue`) land in task 2.
//!
//! ## An absent `[heavy_work]` table means the queue is DISABLED
//!
//! Per standing rule 6's behaviour-stable default: no table at all means every check run inline
//! exactly as today. [`HeavyWorkConfig::disabled`] is that state; [`HeavyWorkConfig::load`]
//! returns it whenever `brain.toml` carries no `[heavy_work]` table. A PRESENT table with a class
//! whose `limit` is `0` fails loudly instead, naming the offending key
//! (`heavy_work.classes.<name>.limit`) — see [`HeavyWorkConfigError::InvalidClassLimit`].
//!
//! ## Why this crate parses `brain.toml` itself rather than reusing `mev::brain::config`
//!
//! `mev::brain::config::BrainConfig` (the reader `policy::permission` already uses for
//! `[permission_profiles]`) carries no `heavy_work` field, and its `Deserialize` derive has no
//! `deny_unknown_fields`, so handing it this file would silently ignore `[heavy_work]` rather
//! than surface it. This module defines its own minimal wrapper struct naming only the
//! `heavy_work` key it cares about; every other top-level table in `brain.toml` (`[vocab]`,
//! `[[repos]]`, `[permission_profiles]`, …) is simply absent from that struct and therefore
//! ignored by `toml::from_str`, exactly the parse-clean behaviour the block record's own
//! `interfaces` section notes as verified 2026-09-10.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `<lock_dir>/heavy-work` — the root of this module's on-disk layout.
const HEAVY_WORK_SUBDIR: &str = "heavy-work";

/// `<lock_dir>/heavy-work/jobs` — one JSON file per [`HeavyWorkJob`], named `<job_id>.json`.
const JOBS_SUBDIR: &str = "jobs";

/// Default heartbeat cadence (seconds) when `[heavy_work]` is present but does not set
/// `heartbeat_interval_secs`.
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Default staleness threshold (seconds) when `[heavy_work]` is present but does not set
/// `stale_after_secs`. Mirrors `scripts/fleet_build.py`'s own `FLEET_BUILD_TTL_SECONDS` — the
/// number this queue's liveness-based reclaim replaces as the sole staleness signal (a dead pid
/// or a stale heartbeat, never elapsed time since admission; see task 2).
const DEFAULT_STALE_AFTER_SECS: u64 = 300;

/// Default dequeue poll cadence (milliseconds) when `[heavy_work]` is present but does not set
/// `poll_interval_ms`. Mirrors `scripts/fleet_build.py`'s `POLL_INTERVAL_SECONDS` (0.05s).
const DEFAULT_POLL_INTERVAL_MS: u64 = 50;

// -------------------------------------------------------------------------------------------
// The job record and its on-disk store.
// -------------------------------------------------------------------------------------------

/// The lifecycle of one [`HeavyWorkJob`].
///
/// Exhaustive: a job is always in exactly one of these five states, and reclaim (task 2) is the
/// only path that moves a `Running` record to `Abandoned` — never `Cancelled`, which is reserved
/// for an explicit cancellation this task does not add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Persisted and waiting for admission; not yet running.
    Queued,
    /// Admitted: a holder pid is executing `commands`, restamping `heartbeat_at` periodically.
    Running,
    /// Finished normally — `passed` and `finished_at` are set.
    Done,
    /// Explicitly cancelled before or during execution. Not written by task 1 or task 2; reserved
    /// for a future caller.
    Cancelled,
    /// Reclaimed because its holder pid was no longer running, or its heartbeat had gone stale —
    /// never because of elapsed time since admission or enqueue (see the module doc comment).
    Abandoned,
}

/// One heavy-work job: a queued/admitted/finished unit of check-running work, gated by class
/// (`test` / `build`), persisted as one JSON file under `<lock_dir>/heavy-work/jobs/<job_id>.json`.
///
/// `#[serde(rename_all = "snake_case")]` keys mirror every other coordination record in this
/// crate (`okf_core::coord::*`, `coord::write`'s registry/lease/message records).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HeavyWorkJob {
    pub job_id: Uuid,
    /// The admission class this job competes for a slot in (`"test"` / `"build"`, or any other
    /// key a caller names — an unconfigured class runs unqueued and `degraded`, see task 2).
    pub class: String,
    pub state: JobState,
    pub repo: String,
    pub cwd: PathBuf,
    pub commands: Vec<String>,
    /// The SDLC run this job's check stage belongs to, when the caller has one — `None` for a
    /// job submitted outside a run context (e.g. a standalone test fixture).
    pub run_id: Option<Uuid>,
    /// The OS pid of the process holding this job while it is `Running`. `None` before admission
    /// and after the job finishes/is abandoned.
    pub holder_pid: Option<u32>,
    pub enqueued_at: DateTime<Utc>,
    /// Set once, on admission — `None` while `Queued`.
    pub admitted_at: Option<DateTime<Utc>>,
    /// Restamped every `heartbeat_interval_secs` while `Running` (task 2's heartbeat task).
    /// `None` before admission and once the job reaches a terminal state.
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Set once the job reaches `Done`, `Cancelled`, or `Abandoned`.
    pub finished_at: Option<DateTime<Utc>>,
    /// The check run's pass/fail outcome, set only when `state == Done`.
    pub passed: Option<bool>,
}

/// Everything that can go wrong reading or writing a [`HeavyWorkJob`] through the store.
#[derive(Debug, thiserror::Error)]
pub enum HeavyWorkStoreError {
    /// An I/O error occurred creating the jobs directory, writing the temp file, or renaming it
    /// into place (write path) — or reading the file at all (read path).
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The job failed to serialize to JSON — never expected in practice (every field here is
    /// `Serialize`-derivable), but surfaced rather than panicking.
    #[error("heavy-work job record at {path} failed to serialize: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The file at `path` did not deserialize into a [`HeavyWorkJob`].
    #[error("heavy-work job record at {path} failed to deserialize: {source}")]
    Deserialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// `<lock_dir>/heavy-work/jobs` — the directory every job record lives directly inside.
pub fn jobs_dir(lock_dir: &Path) -> PathBuf {
    lock_dir.join(HEAVY_WORK_SUBDIR).join(JOBS_SUBDIR)
}

/// `<lock_dir>/heavy-work/jobs/<job_id>.json` — the path one job's record is stored at.
pub fn job_path(lock_dir: &Path, job_id: Uuid) -> PathBuf {
    jobs_dir(lock_dir).join(format!("{job_id}.json"))
}

/// Write `job` to its path under `lock_dir`, creating parent directories as needed, via
/// temp-file-plus-rename: the record is written to a sibling `<job_id>.json.tmp-<pid>-<uuid>`
/// file first, then atomically renamed into place, so a reader can never observe a
/// partially-written job record.
pub fn write_job(lock_dir: &Path, job: &HeavyWorkJob) -> Result<(), HeavyWorkStoreError> {
    let path = job_path(lock_dir, job.job_id);
    let text = serde_json::to_string_pretty(job).map_err(|e| HeavyWorkStoreError::Serialize {
        path: path.clone(),
        source: e,
    })?;
    write_atomic(&path, &text).map_err(|e| HeavyWorkStoreError::Io { path, source: e })
}

/// Read the job record at `path` (as returned by [`job_path`], or discovered by listing
/// [`jobs_dir`]).
pub fn read_job(path: &Path) -> Result<HeavyWorkJob, HeavyWorkStoreError> {
    let text = fs::read_to_string(path).map_err(|e| HeavyWorkStoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&text).map_err(|e| HeavyWorkStoreError::Deserialize {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Write `text` to `path` atomically: create `path`'s parent directory if needed, write to a
/// uniquely-named sibling temp file, then `rename` it over `path`. The rename is atomic on every
/// filesystem this fleet runs on (APFS/HFS+ locally, the same POSIX guarantee in CI), so a reader
/// racing this write either sees the old content or the new content in full — never a partial
/// write.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "job".to_string());
    let tmp_name = format!("{file_name}.tmp-{}-{}", std::process::id(), Uuid::new_v4());
    let tmp_path = path.with_file_name(tmp_name);
    fs::write(&tmp_path, text)?;
    fs::rename(&tmp_path, path)
}

// -------------------------------------------------------------------------------------------
// `brain.toml` `[heavy_work]` configuration.
// -------------------------------------------------------------------------------------------

/// Per-class admission bound: at most `limit` concurrently `Running` (live) jobs of this class,
/// and a job never starts while the free-memory probe reads below `min_free_mb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassLimit {
    pub limit: usize,
    pub min_free_mb: u64,
}

/// The resolved `[heavy_work]` configuration. `enabled == false` (produced only by
/// [`HeavyWorkConfig::disabled`]) means "no `[heavy_work]` table at all" — every consumer runs
/// its checks inline, unqueued, exactly as before this block landed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeavyWorkConfig {
    pub enabled: bool,
    pub heartbeat_interval_secs: u64,
    pub stale_after_secs: u64,
    pub poll_interval_ms: u64,
    pub classes: HashMap<String, ClassLimit>,
}

/// Everything that can go wrong loading `[heavy_work]` from a `brain.toml`.
#[derive(Debug, thiserror::Error)]
pub enum HeavyWorkConfigError {
    #[error("could not read brain.toml at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse brain.toml at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    /// A present `[heavy_work.classes.<name>]` table set `limit` to `0` (or, structurally,
    /// anything below `1`). `key` names the exact dotted path (e.g.
    /// `heavy_work.classes.test.limit`) so the failure can be fixed without re-deriving it.
    #[error("{key} must be at least 1, got {value}")]
    InvalidClassLimit { key: String, value: usize },
}

impl HeavyWorkConfig {
    /// The disabled state: no `[heavy_work]` table present. Every field is a zero value and
    /// `classes` is empty, but the only field a caller should actually branch on is `enabled`.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            heartbeat_interval_secs: 0,
            stale_after_secs: 0,
            poll_interval_ms: 0,
            classes: HashMap::new(),
        }
    }

    /// This class's configured [`ClassLimit`], or `None` when `class` is absent from `classes`
    /// (including when the whole config is `disabled()`) — the "unconfigured class" case that
    /// task 2's queue runs unqueued and `degraded`, never refused.
    pub fn class(&self, class: &str) -> Option<&ClassLimit> {
        self.classes.get(class)
    }

    /// Load `[heavy_work]` from the `brain.toml` at `path`. An absent table parses to
    /// [`HeavyWorkConfig::disabled`] — never an error. A present table with any class's `limit`
    /// below `1` is rejected, naming the offending key.
    pub fn load(path: &Path) -> Result<Self, HeavyWorkConfigError> {
        let text = fs::read_to_string(path).map_err(|e| HeavyWorkConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&text, path)
    }

    /// Parse `[heavy_work]` out of already-read `brain.toml` text. Split from [`Self::load`] so
    /// tests can exercise the parse/validate logic against an in-memory fixture string without
    /// touching the filesystem; `path` is carried through only to name the file in an error.
    fn parse(text: &str, path: &Path) -> Result<Self, HeavyWorkConfigError> {
        let wrapper: RawBrainToml =
            toml::from_str(text).map_err(|e| HeavyWorkConfigError::Parse {
                path: path.to_path_buf(),
                source: e,
            })?;

        let Some(raw) = wrapper.heavy_work else {
            return Ok(Self::disabled());
        };

        let mut classes = HashMap::with_capacity(raw.classes.len());
        for (name, raw_class) in raw.classes {
            if raw_class.limit < 1 {
                return Err(HeavyWorkConfigError::InvalidClassLimit {
                    key: format!("heavy_work.classes.{name}.limit"),
                    value: raw_class.limit,
                });
            }
            classes.insert(
                name,
                ClassLimit {
                    limit: raw_class.limit,
                    min_free_mb: raw_class.min_free_mb,
                },
            );
        }

        Ok(Self {
            enabled: true,
            heartbeat_interval_secs: raw
                .heartbeat_interval_secs
                .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_SECS),
            stale_after_secs: raw.stale_after_secs.unwrap_or(DEFAULT_STALE_AFTER_SECS),
            poll_interval_ms: raw.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS),
            classes,
        })
    }
}

/// The whole `brain.toml` file, as far as this module cares: every other top-level table
/// (`[vocab]`, `[[repos]]`, `[permission_profiles]`, …) is simply absent from this struct, and
/// `toml::from_str` silently ignores unrecognized keys when the target has no
/// `#[serde(deny_unknown_fields)]` — which this struct deliberately omits, matching
/// `mev::brain::config::BrainConfig`'s own posture (see the module doc comment).
#[derive(Debug, Deserialize)]
struct RawBrainToml {
    heavy_work: Option<RawHeavyWorkTable>,
}

#[derive(Debug, Deserialize)]
struct RawHeavyWorkTable {
    heartbeat_interval_secs: Option<u64>,
    stale_after_secs: Option<u64>,
    poll_interval_ms: Option<u64>,
    #[serde(default)]
    classes: HashMap<String, RawClassLimit>,
}

#[derive(Debug, Deserialize)]
struct RawClassLimit {
    limit: usize,
    min_free_mb: u64,
}

// -------------------------------------------------------------------------------------------
// `FreeMemoryProbe` — the injectable free-memory reading seam.
// -------------------------------------------------------------------------------------------

/// A source of "how much free memory does this host have right now, in MB". Injectable so
/// task 2's admission loop (and this task's own unit tests) can supply a scripted sequence of
/// readings instead of shelling out to `vm_stat` on every dequeue attempt.
///
/// The real implementation ([`VmStatFreeMemoryProbe`]) never fails outright: an unreadable or
/// unparseable `vm_stat` reports `u64::MAX` (i.e. "assume plenty of memory"), matching
/// `scripts/fleet_build.py`'s own `_vm_stat_free_mb` fail-open posture — an unreadable metric
/// must never itself block a build.
pub trait FreeMemoryProbe: Send + Sync {
    fn free_mb(&self) -> u64;
}

/// The default [`FreeMemoryProbe`]: shells out to `vm_stat` and sums "Pages free" + "Pages
/// inactive" (both reclaimable without swap activity), mirroring
/// `scripts/fleet_build.py`'s `_vm_stat_free_mb` exactly, including its rationale for summing
/// both fields rather than reading "Pages free" alone (see that function's own doc comment).
#[derive(Debug, Default, Clone, Copy)]
pub struct VmStatFreeMemoryProbe;

impl FreeMemoryProbe for VmStatFreeMemoryProbe {
    fn free_mb(&self) -> u64 {
        run_vm_stat()
            .and_then(|output| parse_vm_stat_free_mb(&output))
            .unwrap_or(u64::MAX)
    }
}

/// Shell out to `vm_stat` and return its stdout, or `None` on any failure to launch it or a
/// non-success exit — mirroring the Python's own `try/except (OSError,
/// subprocess.TimeoutExpired)` fail-open handling (this call has no explicit timeout since
/// `std::process::Command` has none to set; `vm_stat` is a fast, local, non-blocking syscall
/// wrapper in practice).
fn run_vm_stat() -> Option<String> {
    let output = Command::new("vm_stat").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `vm_stat`'s text output into a free-MB estimate: `(Pages free + Pages inactive) *
/// page_size / (1024 * 1024)`. Mirrors `scripts/fleet_build.py`'s `_vm_stat_free_mb` regex
/// pattern-for-pattern: a page size line (`"page size of (\d+) bytes"`, defaulting to 4096 when
/// absent) and per-field lines (`"Pages <word...>: <count>."`). Returns `None` when the fields it
/// needs cannot be found or parsed — the caller ([`VmStatFreeMemoryProbe::free_mb`]) treats that
/// as "assume plenty of memory" (`u64::MAX`), never a hard failure.
fn parse_vm_stat_free_mb(output: &str) -> Option<u64> {
    let page_size_re = Regex::new(r"page size of (\d+) bytes").expect("valid regex literal");
    let field_re =
        Regex::new(r"(?m)^(Pages [a-z ]+):\s+(\d+)\.?\s*$").expect("valid regex literal");

    let page_size: u64 = page_size_re
        .captures(output)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(4096);

    let mut free_pages: Option<u64> = None;
    let mut inactive_pages: Option<u64> = None;
    for caps in field_re.captures_iter(output) {
        let field = caps.get(1)?.as_str();
        let value: u64 = caps.get(2)?.as_str().parse().ok()?;
        match field {
            "Pages free" => free_pages = Some(value),
            "Pages inactive" => inactive_pages = Some(value),
            _ => {}
        }
    }

    let free_pages = free_pages?;
    let inactive_pages = inactive_pages?;
    Some((free_pages + inactive_pages) * page_size / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_job(job_id: Uuid) -> HeavyWorkJob {
        HeavyWorkJob {
            job_id,
            class: "test".to_string(),
            state: JobState::Queued,
            repo: "engine-rs".to_string(),
            cwd: PathBuf::from("/Users/brandon/Dev/agentic-portfolio/core/engine-rs"),
            commands: vec!["cargo nextest run --workspace".to_string()],
            run_id: Some(Uuid::new_v4()),
            holder_pid: None,
            enqueued_at: "2026-09-10T09:00:00Z".parse().expect("valid timestamp"),
            admitted_at: None,
            heartbeat_at: None,
            finished_at: None,
            passed: None,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Serde round-trip / shape.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn heavy_work_job_round_trips_through_serde_with_snake_case_keys() {
        let job_id = Uuid::new_v4();
        let job = sample_job(job_id);

        let value = serde_json::to_value(&job).expect("serialize");
        let obj = value.as_object().expect("job serializes to an object");
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "admitted_at",
                "class",
                "commands",
                "cwd",
                "enqueued_at",
                "finished_at",
                "heartbeat_at",
                "holder_pid",
                "job_id",
                "passed",
                "repo",
                "run_id",
                "state",
            ]
        );
        assert_eq!(
            obj.get("state").and_then(|v| v.as_str()),
            Some("queued"),
            "JobState must serialize snake_case"
        );

        let round_tripped: HeavyWorkJob = serde_json::from_value(value).expect("deserialize");
        assert_eq!(round_tripped, job);
    }

    #[test]
    fn class_limit_and_config_compile_and_round_trip_through_serde() {
        let limit = ClassLimit {
            limit: 2,
            min_free_mb: 2048,
        };
        let value = serde_json::to_value(limit).expect("serialize ClassLimit");
        let round_tripped: ClassLimit = serde_json::from_value(value).expect("deserialize");
        assert_eq!(round_tripped, limit);

        let mut classes = HashMap::new();
        classes.insert("test".to_string(), limit);
        let config = HeavyWorkConfig {
            enabled: true,
            heartbeat_interval_secs: 30,
            stale_after_secs: 300,
            poll_interval_ms: 50,
            classes,
        };
        let value = serde_json::to_value(&config).expect("serialize HeavyWorkConfig");
        let round_tripped: HeavyWorkConfig = serde_json::from_value(value).expect("deserialize");
        assert_eq!(round_tripped, config);
    }

    // ---------------------------------------------------------------------------------------
    // The file store — write via temp-file-plus-rename, read back.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_job_written_to_disk_and_re_read_is_byte_identical_modulo_whitespace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let job = sample_job(Uuid::new_v4());

        write_job(lock_dir, &job).expect("write should succeed");

        let path = job_path(lock_dir, job.job_id);
        assert!(path.exists());
        assert_eq!(
            path,
            lock_dir
                .join("heavy-work")
                .join("jobs")
                .join(format!("{}.json", job.job_id))
        );

        let read_back = read_job(&path).expect("read should succeed");
        assert_eq!(read_back, job, "round-tripped job must be identical");

        // No leftover temp file after a successful write.
        let entries: Vec<_> = fs::read_dir(jobs_dir(lock_dir))
            .expect("read jobs dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec![format!("{}.json", job.job_id)],
            "no stray .tmp- file should remain: {entries:?}"
        );
    }

    #[test]
    fn overwriting_a_job_record_replaces_it_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let job_id = Uuid::new_v4();
        let mut job = sample_job(job_id);

        write_job(lock_dir, &job).expect("first write");
        job.state = JobState::Running;
        job.holder_pid = Some(4242);
        write_job(lock_dir, &job).expect("second write");

        let path = job_path(lock_dir, job_id);
        let read_back = read_job(&path).expect("read back");
        assert_eq!(read_back.state, JobState::Running);
        assert_eq!(read_back.holder_pid, Some(4242));
    }

    #[test]
    fn reading_a_nonexistent_job_reports_an_io_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("heavy-work")
            .join("jobs")
            .join("missing.json");
        let err = read_job(&path).expect_err("must fail");
        assert!(matches!(err, HeavyWorkStoreError::Io { .. }));
    }

    // ---------------------------------------------------------------------------------------
    // `HeavyWorkConfig::load` / `parse`.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn config_absent_is_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.toml");
        fs::write(
            &path,
            "[vocab]\nlayer = [\"brain\", \"engine\"]\n\n[[repos]]\nslug = \"engine-rs\"\n",
        )
        .expect("write fixture brain.toml");

        let config = HeavyWorkConfig::load(&path).expect("load should succeed");
        assert_eq!(config, HeavyWorkConfig::disabled());
        assert!(!config.enabled);
        assert!(config.classes.is_empty());
    }

    #[test]
    fn config_present_with_valid_classes_parses_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.toml");
        fs::write(
            &path,
            r#"
[vocab]
layer = ["brain"]

[heavy_work]
heartbeat_interval_secs = 15
stale_after_secs = 120
poll_interval_ms = 25

[heavy_work.classes.test]
limit = 2
min_free_mb = 2048

[heavy_work.classes.build]
limit = 2
min_free_mb = 2048
"#,
        )
        .expect("write fixture brain.toml");

        let config = HeavyWorkConfig::load(&path).expect("load should succeed");
        assert!(config.enabled);
        assert_eq!(config.heartbeat_interval_secs, 15);
        assert_eq!(config.stale_after_secs, 120);
        assert_eq!(config.poll_interval_ms, 25);
        assert_eq!(
            config.class("test"),
            Some(&ClassLimit {
                limit: 2,
                min_free_mb: 2048
            })
        );
        assert_eq!(
            config.class("build"),
            Some(&ClassLimit {
                limit: 2,
                min_free_mb: 2048
            })
        );
        assert_eq!(config.class("browser"), None);
    }

    #[test]
    fn config_present_with_no_explicit_intervals_uses_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.toml");
        fs::write(
            &path,
            "[heavy_work]\n\n[heavy_work.classes.test]\nlimit = 1\nmin_free_mb = 1024\n",
        )
        .expect("write fixture brain.toml");

        let config = HeavyWorkConfig::load(&path).expect("load should succeed");
        assert!(config.enabled);
        assert_eq!(
            config.heartbeat_interval_secs,
            DEFAULT_HEARTBEAT_INTERVAL_SECS
        );
        assert_eq!(config.stale_after_secs, DEFAULT_STALE_AFTER_SECS);
        assert_eq!(config.poll_interval_ms, DEFAULT_POLL_INTERVAL_MS);
    }

    #[test]
    fn config_limit_zero_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.toml");
        fs::write(
            &path,
            "[heavy_work]\n\n[heavy_work.classes.test]\nlimit = 0\nmin_free_mb = 2048\n",
        )
        .expect("write fixture brain.toml");

        let err = HeavyWorkConfig::load(&path).expect_err("must fail");
        match err {
            HeavyWorkConfigError::InvalidClassLimit { key, value } => {
                assert_eq!(key, "heavy_work.classes.test.limit");
                assert_eq!(value, 0);
            }
            other => panic!("expected InvalidClassLimit, got {other:?}"),
        }
    }

    #[test]
    fn config_missing_brain_toml_fails_with_a_read_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist").join("brain.toml");
        let err = HeavyWorkConfig::load(&path).expect_err("must fail");
        assert!(matches!(err, HeavyWorkConfigError::Read { .. }));
    }

    #[test]
    fn config_malformed_toml_fails_with_a_parse_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("brain.toml");
        fs::write(&path, "[heavy_work\nclasses = not valid toml").expect("write bad toml");
        let err = HeavyWorkConfig::load(&path).expect_err("must fail");
        assert!(matches!(err, HeavyWorkConfigError::Parse { .. }));
    }

    // ---------------------------------------------------------------------------------------
    // `FreeMemoryProbe` — `vm_stat` output parsing against a captured fixture.
    // ---------------------------------------------------------------------------------------

    /// A `vm_stat` output string captured from a real macOS host (page size 16384 bytes, as on
    /// Apple Silicon), used to pin the parser against real-world formatting: trailing periods on
    /// some fields, right-aligned counts, and the leading "Mach Virtual Memory Statistics" banner
    /// line the parser must skip over.
    const VM_STAT_FIXTURE: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
Pages free:                               58921.\n\
Pages active:                            412903.\n\
Pages inactive:                          103482.\n\
Pages speculative:                          1042.\n\
Pages throttled:                               0.\n\
Pages wired down:                         198765.\n\
Pages purgeable:                            8821.\n\
\"Translation faults\":                 88213445.\n\
Pages copy-on-write:                     1233456.\n\
Pages zero filled:                      33221190.\n\
Pages reactivated:                         44012.\n\
Pages purged:                              91233.\n\
File-backed pages:                       210983.\n\
Anonymous pages:                         305402.\n\
Pages stored in compressor:               512093.\n\
Pages occupied by compressor:             128456.\n\
Decompressions:                          1029384.\n\
Compressions:                            2093841.\n\
Pageins:                                 5566778.\n\
Pageouts:                                  22011.\n\
Swapins:                                       0.\n\
Swapouts:                                      0.\n";

    #[test]
    fn vm_stat_parse_matches_fixture() {
        // (58921 + 103482) pages * 16384 bytes / (1024 * 1024) = 2537 MB (integer division).
        let expected = (58_921_u64 + 103_482) * 16384 / (1024 * 1024);
        let parsed = parse_vm_stat_free_mb(VM_STAT_FIXTURE).expect("fixture must parse");
        assert_eq!(parsed, expected);
    }

    #[test]
    fn vm_stat_parse_defaults_page_size_when_banner_line_is_absent() {
        let text = "Pages free:                               1000.\nPages inactive:                           1000.\n";
        // Default page size is 4096 when no "page size of N bytes" line is found.
        let expected = (1000_u64 + 1000) * 4096 / (1024 * 1024);
        let parsed = parse_vm_stat_free_mb(text).expect("must parse with default page size");
        assert_eq!(parsed, expected);
    }

    #[test]
    fn vm_stat_parse_returns_none_when_fields_are_missing() {
        let text =
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages wired down: 100.\n";
        assert_eq!(parse_vm_stat_free_mb(text), None);
    }

    #[test]
    fn vm_stat_free_memory_probe_default_never_panics() {
        // Exercises the real `vm_stat`-shelling path (whatever this CI/dev host reports); the
        // only invariant under test is that it returns *something* without panicking, since the
        // actual value is host-dependent and not fixture-controlled.
        let probe = VmStatFreeMemoryProbe;
        let _ = probe.free_mb();
    }
}
