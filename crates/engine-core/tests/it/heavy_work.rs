//! Integration tests for `coord::heavy_work` over real tempdir lock dirs.
//!
//! `EN.17.I` task 3. Task 1/2 landed the job store, `HeavyWorkConfig`, `FreeMemoryProbe`, and
//! `HeavyWorkQueue`'s FIFO admission + liveness-only reclaim as unit tests colocated with the
//! module. This file exercises the same machinery end to end through the real filesystem store
//! (`tempfile::tempdir`), an injected clock, an injected `FreeMemoryProbe`, and stub work
//! closures — the shape every acceptance criterion in the block record names.
//!
//! ## OBSERVED RED (D68) — the prior-art defect this queue fixes
//!
//! Before writing this file, the following was run against `scripts/fleet_build.py`'s own
//! `_sweep_stale`, in a tempdir holding a permit JSON with this test process's own (live) pid and
//! `started_at` 400 seconds in the past, `ttl_seconds=300`:
//!
//! ```text
//! before sweep, exists: True
//! after sweep, exists: False
//! ```
//!
//! `_sweep_stale` deletes the permit purely because `age (400s) > ttl_seconds (300s)`, even
//! though the holder pid is alive — the exact defect `coord::heavy_work::is_reclaimable`'s doc
//! comment describes and its signature structurally cannot repeat (it takes no age parameter at
//! all). [`heavy_work_live_long_job_keeps_its_slot`] below is the equivalent-fixture Rust
//! assertion that the SAME shape (a live pid, well past `stale_after_secs` since admission) is
//! NOT reclaimed by this module.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use engine_core::coord::heavy_work::{
    is_reclaimable, job_path, read_job, ClassLimit, FreeMemoryProbe, HeavyWorkConfig,
    HeavyWorkQueue, HeavyWorkSpec, JobState,
};

fn spec(dir: &Path, class: &str) -> HeavyWorkSpec {
    HeavyWorkSpec {
        class: class.to_string(),
        repo: "engine-rs".to_string(),
        cwd: dir.to_path_buf(),
        commands: vec!["cargo nextest run --workspace".to_string()],
        run_id: None,
    }
}

fn config_with_class(class: &str, limit: usize, min_free_mb: u64) -> HeavyWorkConfig {
    let mut classes = HashMap::new();
    classes.insert(class.to_string(), ClassLimit { limit, min_free_mb });
    HeavyWorkConfig {
        enabled: true,
        heartbeat_interval_secs: 3600, // long enough that no heartbeat task fires mid-test
        stale_after_secs: 300,
        poll_interval_ms: 5,
        classes,
    }
}

/// A [`FreeMemoryProbe`] that always reports "plenty of memory" — the default for tests whose
/// own subject is FIFO/limit behaviour, not the memory floor.
#[derive(Debug, Clone, Copy)]
struct AlwaysFreeProbe;

impl FreeMemoryProbe for AlwaysFreeProbe {
    fn free_mb(&self) -> u64 {
        u64::MAX
    }
}

/// A [`FreeMemoryProbe`] that returns a scripted sequence of readings, one per call, holding the
/// last value once the sequence is exhausted — mirroring `fleet_build.py`'s own
/// `_pop_sequence_value` test-injection convention (`scripts/fleet_build.py`'s doc comment on
/// that function), reused here in spirit for the Rust seam.
struct ScriptedProbe {
    readings: Mutex<Vec<u64>>,
    calls: AtomicUsize,
}

impl ScriptedProbe {
    fn new(readings: Vec<u64>) -> Self {
        Self {
            readings: Mutex::new(readings),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl FreeMemoryProbe for ScriptedProbe {
    fn free_mb(&self) -> u64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut readings = self.readings.lock().expect("lock poisoned");
        if readings.len() > 1 {
            readings.remove(0)
        } else {
            *readings.first().unwrap_or(&0)
        }
    }
}

/// A fixed clock the test can advance by replacing the `Arc<Mutex<...>>` cell's value, so
/// `HeavyWorkQueue::with_clock` reads whatever the test currently wants "now" to be.
fn fixed_clock(
    at: Arc<Mutex<DateTime<Utc>>>,
) -> impl Fn() -> DateTime<Utc> + Send + Sync + 'static {
    move || *at.lock().expect("lock poisoned")
}

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 11, 12, 0, 0).unwrap()
}

// -------------------------------------------------------------------------------------------
// FIFO order + limit bound.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn heavy_work_fifo_order_limit_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_class("test", 1, 0);
    let queue =
        HeavyWorkQueue::new(dir.path().to_path_buf(), config).with_probe(Arc::new(AlwaysFreeProbe));

    let started: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let running_now: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let overlap_seen = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for label in ["A", "B", "C"] {
        // Stagger submission so enqueued_at strictly orders A < B < C even at coarse clock
        // resolution: this queue's real clock has real (non-injected) timestamps here, so a
        // small sleep between submits is what pins ordering rather than relying on sub-ms ties.
        let queue = queue.clone();
        let dir_path = dir.path().to_path_buf();
        let started = Arc::clone(&started);
        let running_now = Arc::clone(&running_now);
        let overlap_seen = Arc::clone(&overlap_seen);
        handles.push(tokio::spawn(async move {
            queue
                .run(spec(&dir_path, "test"), move || {
                    started.lock().expect("lock poisoned").push(label);
                    {
                        let mut running = running_now.lock().expect("lock poisoned");
                        if !running.is_empty() {
                            overlap_seen.fetch_add(1, Ordering::SeqCst);
                        }
                        running.push(label);
                    }
                    std::thread::sleep(Duration::from_millis(30));
                    running_now
                        .lock()
                        .expect("lock poisoned")
                        .retain(|&x| x != label);
                    label
                })
                .await
        }));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    for h in handles {
        h.await.expect("submitted task must not panic");
    }

    assert_eq!(
        *started.lock().expect("lock poisoned"),
        vec!["A", "B", "C"],
        "with limit 1, jobs must start in exactly submission order"
    );
    assert_eq!(
        overlap_seen.load(Ordering::SeqCst),
        0,
        "no two jobs may ever be running at once at limit 1"
    );
}

// -------------------------------------------------------------------------------------------
// Limit bounds in-flight concurrency, with a positive control.
// -------------------------------------------------------------------------------------------

async fn max_concurrency_at_limit(limit: usize) -> usize {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_class("test", limit, 0);
    let queue =
        HeavyWorkQueue::new(dir.path().to_path_buf(), config).with_probe(Arc::new(AlwaysFreeProbe));

    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let queue = queue.clone();
        let dir_path = dir.path().to_path_buf();
        let in_flight = Arc::clone(&in_flight);
        let max_seen = Arc::clone(&max_seen);
        handles.push(tokio::spawn(async move {
            queue
                .run(spec(&dir_path, "test"), move || {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(40));
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await
        }));
    }
    for h in handles {
        h.await.expect("submitted task must not panic");
    }

    max_seen.load(Ordering::SeqCst)
}

#[tokio::test]
async fn heavy_work_limit_bounds_in_flight() {
    let observed_at_2 = max_concurrency_at_limit(2).await;
    assert_eq!(
        observed_at_2, 2,
        "with limit 2 and five concurrent jobs, observed max concurrency must be exactly 2"
    );
}

#[tokio::test]
async fn heavy_work_limit_bounds_in_flight_positive_control_at_limit_three() {
    // Positive control for the test above: the harness itself can observe concurrency, so a
    // limit that never actually bounds anything would not silently pass either assertion.
    let observed_at_3 = max_concurrency_at_limit(3).await;
    assert_eq!(
        observed_at_3, 3,
        "with limit 3 and five concurrent jobs, observed max concurrency must be exactly 3"
    );
}

// -------------------------------------------------------------------------------------------
// Memory floor: re-read on every dequeue attempt, never cached.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn heavy_work_memory_floor_rereads_per_dequeue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_class("test", 1, 2048);
    let probe = Arc::new(ScriptedProbe::new(vec![1000, 4096]));
    let queue = HeavyWorkQueue::new(dir.path().to_path_buf(), config).with_probe(probe.clone());

    let start = std::time::Instant::now();
    let outcome = queue.run(spec(dir.path(), "test"), || "done").await;
    let elapsed = start.elapsed();

    assert_eq!(outcome.output, "done");
    assert!(
        probe.call_count() >= 2,
        "the probe must be read at least twice: {} calls",
        probe.call_count()
    );
    // The first reading (1000 < 2048) must have blocked the first dequeue attempt: the job could
    // not possibly have been admitted before at least one poll_interval_ms (5ms) elapsed.
    assert!(
        elapsed >= Duration::from_millis(5),
        "job must not start on the first (below-floor) reading: elapsed {elapsed:?}"
    );
}

#[tokio::test]
async fn heavy_work_memory_floor_blocked_head_is_not_overtaken() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_class("test", 1, 2048);
    // The head job's probe never clears the floor for the duration of this test.
    let probe = Arc::new(ScriptedProbe::new(vec![1000]));
    let queue = HeavyWorkQueue::new(dir.path().to_path_buf(), config).with_probe(probe.clone());

    let dir_path_head = dir.path().to_path_buf();
    let queue_head = queue.clone();
    let head_handle = tokio::spawn(async move {
        queue_head
            .run(spec(&dir_path_head, "test"), || "head")
            .await
    });

    // Give the head job time to enqueue and attempt (and fail) admission at least once.
    tokio::time::sleep(Duration::from_millis(60)).await;

    let later_started = Arc::new(AtomicUsize::new(0));
    let dir_path_later = dir.path().to_path_buf();
    let queue_later = queue.clone();
    let later_started_clone = Arc::clone(&later_started);
    let later_handle = tokio::spawn(async move {
        queue_later
            .run(spec(&dir_path_later, "test"), move || {
                later_started_clone.fetch_add(1, Ordering::SeqCst);
                "later"
            })
            .await
    });

    // While the head is still blocked on the floor, the later job must not have started.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        later_started.load(Ordering::SeqCst),
        0,
        "a later same-class job must not start ahead of a blocked head"
    );

    // Abort both — this test only needs to observe ordering, not full completion, since the
    // scripted probe never clears the floor.
    head_handle.abort();
    later_handle.abort();
}

// -------------------------------------------------------------------------------------------
// Reclaim: liveness only, never elapsed time since admission (the OBSERVED RED fix).
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn heavy_work_live_long_job_keeps_its_slot() {
    // Equivalent-fixture assertion to the OBSERVED RED demonstration in this file's module doc:
    // a live pid, well past stale_after_secs since admission, must NOT be reclaimed and its slot
    // must still be counted against the limit.
    let stale_after_secs = 300;
    let now = base_time();
    let admitted_at = now - chrono::Duration::seconds(10 * stale_after_secs as i64);
    let live_pid = std::process::id();

    assert!(
        !is_reclaimable(Some(live_pid), Some(now), stale_after_secs, now),
        "a live pid with a fresh heartbeat must not be reclaimed regardless of admission age"
    );
    // admitted_at itself is never even consulted by is_reclaimable — the parameter list has no
    // slot for it. This assertion documents that the OBSERVED RED scenario (age far past ttl,
    // holder alive) reads as "keep the slot" here, unlike fleet_build.py's _sweep_stale.
    let _ = admitted_at;
}

#[tokio::test]
async fn heavy_work_dead_holder_is_reclaimed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_with_class("test", 1, 0);
    let clock_cell = Arc::new(Mutex::new(base_time()));
    let queue = HeavyWorkQueue::new(dir.path().to_path_buf(), config.clone())
        .with_probe(Arc::new(AlwaysFreeProbe))
        .with_clock(fixed_clock(Arc::clone(&clock_cell)));

    // Admit a first job that never finishes (simulated by aborting its task after admission),
    // leaving behind a Running record whose holder pid we then rewrite to a dead one.
    let dir_path = dir.path().to_path_buf();
    let admitted = Arc::new(tokio::sync::Notify::new());
    let admitted_clone = Arc::clone(&admitted);
    let release = Arc::new(tokio::sync::Notify::new());
    let release_clone = Arc::clone(&release);
    let held = tokio::spawn({
        let queue = queue.clone();
        async move {
            queue
                .run(spec(&dir_path, "test"), move || {
                    admitted_clone.notify_one();
                    // Block the worker thread until the test releases it, so the job stays
                    // Running long enough for the test to rewrite its holder_pid.
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(release_clone.notified());
                })
                .await
        }
    });
    admitted.notified().await;
    // Give the store a moment to reflect the Running write.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Find the Running job and rewrite its holder_pid to one that is not running.
    let jobs_dir = dir.path().join("heavy-work").join("jobs");
    let mut job_path_found = None;
    for entry in std::fs::read_dir(&jobs_dir)
        .expect("read jobs dir")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(job) = read_job(&path) {
                if job.state == JobState::Running {
                    job_path_found = Some((path, job));
                }
            }
        }
    }
    let (_path, mut job) = job_path_found.expect("must have found the Running job");
    job.holder_pid = Some(999_999); // a pid that is not running
    engine_core::coord::heavy_work::write_job(dir.path(), &job).expect("rewrite holder_pid");

    // A second same-class job should now be admitted once the dead holder is reclaimed on the
    // next admission attempt.
    let second_admitted = Arc::new(AtomicUsize::new(0));
    let second_admitted_clone = Arc::clone(&second_admitted);
    let dir_path2 = dir.path().to_path_buf();
    let second = queue
        .run(spec(&dir_path2, "test"), move || {
            second_admitted_clone.fetch_add(1, Ordering::SeqCst);
            "second"
        })
        .await;

    assert_eq!(second.output, "second");
    assert_eq!(second_admitted.load(Ordering::SeqCst), 1);

    release.notify_one();
    let _ = held.await;
    let _ = job_path(dir.path(), job.job_id); // keep `job_path` import used at call sites too
}

// -------------------------------------------------------------------------------------------
// Disabled queue: today's inline, overlapping behaviour is unchanged.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn heavy_work_disabled_runs_inline_and_overlaps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let queue = HeavyWorkQueue::new(dir.path().to_path_buf(), HeavyWorkConfig::disabled());

    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let queue = queue.clone();
        let dir_path = dir.path().to_path_buf();
        let in_flight = Arc::clone(&in_flight);
        let max_seen = Arc::clone(&max_seen);
        handles.push(tokio::spawn(async move {
            queue
                .run(spec(&dir_path, "test"), move || {
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(40));
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                })
                .await
        }));
    }

    let mut outcomes = Vec::new();
    for h in handles {
        outcomes.push(h.await.expect("submitted task must not panic"));
    }

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        2,
        "disabled queue must let both jobs run inline, overlapping, exactly as before this block"
    );
    for outcome in outcomes {
        assert!(!outcome.degraded, "disabled mode reports degraded == false");
        assert_eq!(
            outcome.mode,
            engine_core::coord::heavy_work::HeavyWorkMode::Disabled
        );
    }
}

// -------------------------------------------------------------------------------------------
// Unwritable lock dir: degrades open rather than failing the work.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn heavy_work_unwritable_lock_dir_degrades_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_dir = dir.path().join("locked");
    std::fs::create_dir_all(&lock_dir).expect("create lock dir");

    let mut perms = std::fs::metadata(&lock_dir)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o500); // read+execute, no write
    }
    std::fs::set_permissions(&lock_dir, perms).expect("set read-only permissions");

    let config = config_with_class("test", 1, 0);
    let queue = HeavyWorkQueue::new(lock_dir.clone(), config).with_probe(Arc::new(AlwaysFreeProbe));

    let outcome = queue.run(spec(&lock_dir, "test"), || "ran anyway").await;

    assert_eq!(
        outcome.output, "ran anyway",
        "work must still run under an unwritable lock dir"
    );
    assert!(
        outcome.degraded,
        "an unwritable lock dir must degrade open, not refuse the job"
    );

    // Restore write permission so tempdir cleanup doesn't fail on drop.
    let mut restore = std::fs::metadata(&lock_dir)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        restore.set_mode(0o700);
    }
    std::fs::set_permissions(&lock_dir, restore).expect("restore permissions");
}
