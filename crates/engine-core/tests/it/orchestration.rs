//! `EN.10.B` Task 6 — two-repo end-to-end integration suite for the
//! ORCHESTRATION workflow's chain/gates/execute/integrate modules.
//!
//! Drives a real two-repo chain end to end against tempdir fixture repos and
//! a tempdir lane log — never a real roadmap's `lane-log.jsonl` or a tracked
//! `state.json`, since a sibling lane reads that file and a test writing
//! into it would be indistinguishable from a real run. Covers every
//! acceptance criterion named across `EN.10.B` Tasks 1-4:
//! - per-step cwd
//! - unmet-dependency refusal (names the edge)
//! - admission waiting at capacity (never proceeding or failing)
//! - `HELD-UNTIL` refusal (names the held block and repo)
//! - operator-hold pause/resume without re-running completed blocks
//! - exactly one `lane-log.jsonl` line per integrated block
//! - loud failure on a corrupted state write

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine_core::cancellation::CancellationToken;
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::{
    resolve_explicit_chain, resolve_lane_chain, ChainError,
};
use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, HoldSource, IntegrateError, NeverHeld, StepProgress,
};

use engine_core::nodes::terminal::admission::{AdmissionControl, AdmissionPolicy};

// ── Shared fixtures ──────────────────────────────────────────────────────

/// A tempdir `brain.toml` + repo registry with two real repo directories,
/// `repo-a` and `repo-b` — mirrors the pattern `execute.rs`'s and
/// `integrate.rs`'s own unit tests already use, kept here so the
/// integration suite drives the *same* modules through their public API,
/// not a reimplementation of the fixture.
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

fn write_state(repo_path: &Path, block_id: &str, status: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        serde_json::json!({"status": status}).to_string(),
    )
    .unwrap();
}

/// Build a tempdir roadmap directory under `planning/roadmaps/<slug>/` (the
/// new-location-first rule) so `append_lane_log_line` has somewhere to
/// write that is never a real, tracked roadmap.
fn fixture_roadmap_dir(slug: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let planning_root = tempfile::tempdir().expect("tempdir");
    let roadmap_dir = planning_root.path().join("roadmaps").join(slug);
    std::fs::create_dir_all(&roadmap_dir).unwrap();
    (planning_root, roadmap_dir)
}

/// A [`FlowRunner`] test double: records every invocation's `(repo,
/// block_id, repo_path)` and, unless configured otherwise, writes a
/// `"status": "done"` state file into the invocation's own `repo_path` so
/// [`integrate_chain`]'s state-write verification passes by default. A
/// per-block override lets a test force a specific (possibly corrupted)
/// status, or skip writing the file entirely.
#[derive(Clone)]
struct RecordingRunner {
    calls: Arc<Mutex<Vec<(String, String, std::path::PathBuf)>>>,
    overrides: Arc<Mutex<std::collections::HashMap<String, Option<String>>>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            overrides: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Force `block_id`'s state-file status to `status` (e.g. a corrupted
    /// value) instead of the default `"done"`. `None` skips writing the
    /// state file at all.
    fn set_status_override(&self, block_id: &str, status: Option<&str>) {
        self.overrides
            .lock()
            .unwrap()
            .insert(block_id.to_string(), status.map(str::to_string));
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn calls_for(&self, block_id: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, id, _)| id == block_id)
            .count()
    }

    fn cwd_for(&self, block_id: &str) -> std::path::PathBuf {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|(_, id, _)| id == block_id)
            .expect("block should have been invoked")
            .2
            .clone()
    }

    fn into_runner(self) -> FlowRunner {
        Arc::new(move |invocation| {
            let this = self.clone();
            Box::pin(async move {
                this.calls.lock().unwrap().push((
                    invocation.repo.clone(),
                    invocation.block_id.clone(),
                    invocation.repo_path.clone(),
                ));
                let status = this
                    .overrides
                    .lock()
                    .unwrap()
                    .get(&invocation.block_id)
                    .cloned()
                    .unwrap_or_else(|| Some("done".to_string()));
                if let Some(status) = status {
                    write_state(&invocation.repo_path, &invocation.block_id, &status);
                }
                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        })
    }
}

fn no_deps(_repo: &str, _id: &str) -> Vec<DependencyEdge> {
    Vec::new()
}

fn always_met(_repo: &str, _id: &str) -> bool {
    true
}

fn always_flow(_repo: &str, _id: &str) -> EngineKind {
    EngineKind::Flow
}

fn lane_log_lines(roadmap_dir: &Path) -> Vec<serde_json::Value> {
    let path = roadmap_dir.join("lane-log.jsonl");
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ── Full lifecycle: two-repo chain, per-step cwd, one lane-log line each ──

#[tokio::test]
async fn two_repo_chain_runs_end_to_end_with_per_step_cwd_and_one_lane_log_line_each() {
    let (repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let outcomes = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
    )
    .await
    .expect("two-repo chain should integrate cleanly");

    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].repo_path, repos_dir.path().join("repo-a"));
    assert_eq!(outcomes[1].repo_path, repos_dir.path().join("repo-b"));

    // Cwd actually passed to the runner, not merely the outcome the test
    // computed independently.
    assert_eq!(runner.cwd_for("A.1"), repos_dir.path().join("repo-a"));
    assert_eq!(runner.cwd_for("B.1"), repos_dir.path().join("repo-b"));

    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 2, "exactly one lane-log line per block");
    assert_eq!(lines[0]["block"], "A.1");
    assert_eq!(lines[0]["repo"], "repo-a");
    assert_eq!(lines[1]["block"], "B.1");
    assert_eq!(lines[1]["repo"], "repo-b");
}

// ── Dependency gate ─────────────────────────────────────────────────────

#[tokio::test]
async fn unmet_dependency_stops_the_chain_before_it_starts_and_names_the_edge() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    let resolve_deps = |_repo: &str, _id: &str| {
        vec![DependencyEdge {
            repo: "engine-rs".to_string(),
            block_id: "EN.9.F".to_string(),
        }]
    };
    let never_met = |_repo: &str, _id: &str| false;

    let err = integrate_chain(
        &chain,
        &resolve_deps,
        &never_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
    )
    .await
    .expect_err("an unmet dependency must refuse the block");

    assert!(matches!(err, IntegrateError::Gate(_)));
    let msg = err.to_string();
    assert!(
        msg.contains("EN.9.F"),
        "message should name the edge: {msg}"
    );
    assert!(
        msg.contains("engine-rs"),
        "message should name the repo: {msg}"
    );

    // The block never ran, and nothing was logged.
    assert_eq!(runner.call_count(), 0);
    assert!(lane_log_lines(&roadmap_dir).is_empty());
}

// ── Admission gate ──────────────────────────────────────────────────────

#[tokio::test]
async fn admission_at_capacity_waits_rather_than_proceeding_or_failing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(admission_at_capacity_waits_rather_than_proceeding_or_failing_inner())
        .await;
}

// `integrate_chain`'s `FlowFuture` is deliberately not `Send` (see
// `execute.rs`'s doc comment), so this concurrency test drives it via
// `LocalSet::spawn_local` on a single thread rather than `tokio::spawn` —
// cooperative scheduling across the two chains' `.await` points is all this
// test needs; no OS-thread parallelism is required to prove the ordering.
async fn admission_at_capacity_waits_rather_than_proceeding_or_failing_inner() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();

    // Capacity of exactly one admitted block at a time.
    let admission = AdmissionGate::new(AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: 1,
    }));

    let chain_a = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    let chain_b = resolve_explicit_chain(vec![("repo-b".to_string(), "B.1".to_string())]);

    // Occupy the single admission slot directly (not via integrate_chain)
    // so the test controls exactly when it releases.
    let held_permit = admission.acquire_for(&chain_a[0]).await;
    assert_eq!(admission.available_permits(), 0);

    let admission2 = admission.clone();
    let registry_path = registry.brain_root().to_path_buf();
    let admitted = Arc::new(AtomicBool::new(false));
    let admitted_writer = admitted.clone();
    let flow_runner2 = flow_runner.clone();
    let roadmap_dir2 = roadmap_dir.clone();
    let waiter = tokio::task::spawn_local(async move {
        let registry = RepoRegistry::from_brain_root(&registry_path).unwrap();
        let outcome = integrate_chain(
            &chain_b,
            &no_deps,
            &always_met,
            &admission2,
            &NeverHeld,
            Duration::from_millis(5),
            None,
            &always_flow,
            &registry,
            &flow_runner2,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
        )
        .await;
        admitted_writer.store(true, Ordering::SeqCst);
        outcome
    });

    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        !admitted.load(Ordering::SeqCst),
        "a block at capacity must wait, not proceed"
    );
    assert!(!waiter.is_finished(), "waiter must not have failed either");
    assert_eq!(runner.call_count(), 0, "B.1 must not have run yet");

    drop(held_permit);
    let result = tokio::time::timeout(Duration::from_millis(500), waiter)
        .await
        .expect("waiter should complete promptly once the slot frees")
        .expect("join should succeed");
    result.expect("B.1 should integrate once admitted");
    assert_eq!(runner.call_count(), 1);
}

// ── HELD-UNTIL directive ─────────────────────────────────────────────────

#[tokio::test]
async fn a_lane_whose_held_until_names_an_open_block_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let json = r#"{
        "blocks": [
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "directives": {"held_until": "EN.9.F"}}
        ]
    }"#;
    let path = dir.path().join("lane-segments.json");
    std::fs::write(&path, json).unwrap();

    let err = resolve_lane_chain(&path, "r", "l", &|token| token == "EN.9.F")
        .expect_err("an open HELD-UNTIL target must refuse the whole lane");

    match &err {
        ChainError::Held {
            held_until, repo, ..
        } => {
            assert_eq!(held_until, "EN.9.F");
            assert_eq!(repo, "repo-a");
        }
        other => panic!("expected ChainError::Held, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("EN.9.F"));
    assert!(msg.contains("repo-a"));
}

// ── Operator hold: pause-and-resume without re-running completed blocks ──

/// A [`HoldSource`] that holds `held_block` until a test flips its shared
/// `AtomicBool` clear — re-read on every poll, exactly like the production
/// seam this double stands in for.
struct FlaggedHold {
    held_block: &'static str,
    cleared: Arc<AtomicBool>,
}

impl HoldSource for FlaggedHold {
    fn is_held(&self, _repo: &str, block_id: &str) -> bool {
        block_id == self.held_block && !self.cleared.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn an_operator_hold_pauses_and_resumes_without_rerunning_completed_blocks() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(an_operator_hold_pauses_and_resumes_without_rerunning_completed_blocks_inner())
        .await;
}

// See the admission test's doc comment: `integrate_chain`'s future is not
// `Send`, so this drives it via `LocalSet::spawn_local` rather than
// `tokio::spawn`.
async fn an_operator_hold_pauses_and_resumes_without_rerunning_completed_blocks_inner() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let cleared = Arc::new(AtomicBool::new(false));
    let hold = FlaggedHold {
        held_block: "B.1",
        cleared: cleared.clone(),
    };

    let registry_path = registry.brain_root().to_path_buf();
    let roadmap_dir2 = roadmap_dir.clone();
    let handle = tokio::task::spawn_local(async move {
        let registry = RepoRegistry::from_brain_root(&registry_path).unwrap();
        integrate_chain(
            &chain,
            &no_deps,
            &always_met,
            &admission,
            &hold,
            Duration::from_millis(10),
            None,
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
        )
        .await
    });

    // Give A.1 time to integrate and B.1 time to start waiting on the hold.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        runner.calls_for("A.1"),
        1,
        "A.1 should already be integrated"
    );
    assert_eq!(
        runner.calls_for("B.1"),
        0,
        "B.1 must not have run while held"
    );
    assert_eq!(
        lane_log_lines(&roadmap_dir).len(),
        1,
        "only A.1's lane-log line should exist while B.1 is held"
    );
    assert!(!handle.is_finished(), "the run must be paused, not failed");

    cleared.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect("run should resume and finish promptly once cleared")
        .expect("join should succeed");
    let outcomes = result.expect("chain should integrate cleanly once unheld");

    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        runner.calls_for("A.1"),
        1,
        "A.1 must not be re-run on resume"
    );
    assert_eq!(runner.calls_for("B.1"), 1);
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 2, "exactly one line per block, no duplicates");
}

// ── State-write verification ─────────────────────────────────────────────

#[tokio::test]
async fn a_corrupted_state_write_fails_the_run_loudly() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    runner.set_status_override("A.1", Some("in_progress"));
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    let err = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
    )
    .await
    .expect_err("a corrupted state write must fail the run");

    assert!(matches!(err, IntegrateError::StateWriteMismatch { .. }));
    let msg = err.to_string();
    assert!(msg.contains("A.1"));

    // The run still stops loudly, but a `bailed` line is recorded for the
    // attempt (EN.ticket.lane-log-entry-schema Task 3) — a sibling lane
    // must see that this block was tried and failed, not silence.
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["status"], "bailed");
    assert_eq!(lines[0]["block"], "A.1");
    assert!(lines[0]["note"].as_str().unwrap().contains("A.1"));
}

// ── Lane-log on-disk contract (EN.ticket.lane-log-entry-schema Task 1) ───
//
// `crates/engine-core/tests/fixtures/lane-log-contract.jsonl` holds real
// lines copied verbatim from `planning/roadmaps/*/lane-log.jsonl` — one
// `closed`, one `held`, two `bailed` — covering every status value the
// fleet's real lane logs carry. `LaneLogEntry` must deserialize every one
// of them with no field loss. As of Task 1 this is deliberately RED: the
// current struct is `{repo, block_id, integrated_at}` and Serialize-only
// (no `Deserialize` impl at all), while the fixture lines are
// `{ts, lane, repo, block, status, note}`. Task 2 reshapes the struct and
// turns this GREEN.
#[test]
fn lane_log_contract_fixture_round_trips_into_lane_log_entry() {
    let fixture = include_str!("../fixtures/lane-log-contract.jsonl");
    let mut saw_closed = false;
    let mut saw_held = false;
    let mut saw_bailed = false;

    for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
        let entry: engine_core::workflows::orchestration::integrate::LaneLogEntry =
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("fixture line failed to deserialize into LaneLogEntry: {e}\nline: {line}")
            });

        // No field loss: re-serialize and compare the parsed JSON values
        // (not raw text, since key order/whitespace need not round-trip
        // byte-for-byte) so every field on disk survives the round trip.
        let original: serde_json::Value = serde_json::from_str(line).unwrap();
        let round_tripped: serde_json::Value =
            serde_json::to_value(&entry).expect("entry must re-serialize");
        assert_eq!(
            original, round_tripped,
            "round trip must preserve every field with no loss"
        );

        match entry.status {
            engine_core::workflows::orchestration::integrate::LaneLogStatus::Closed => {
                saw_closed = true;
            }
            engine_core::workflows::orchestration::integrate::LaneLogStatus::Held => {
                saw_held = true;
            }
            engine_core::workflows::orchestration::integrate::LaneLogStatus::Bailed => {
                saw_bailed = true;
            }
        }
    }

    assert!(saw_closed, "fixture must cover status: closed");
    assert!(saw_held, "fixture must cover status: held");
    assert!(saw_bailed, "fixture must cover status: bailed");
}

// ── Task 4: readable by the REAL reader, not a reimplementation ─────────
//
// The defect this whole ticket closes is that the writer's shape and the
// reader's expectations were verified separately — the struct's own doc
// comment claimed "deliberately plain and stable" while
// `roadmap_status_discovery.py`'s `read_lane_log` silently skipped every
// line it produced. Proving that closed means driving `integrate_chain`
// (the real writer, through its real public API — a stub `FlowRunner`
// stands in for `SDLC_FLOW` itself, exactly as the other tests in this
// suite do) to write a real `lane-log.jsonl`, then shelling out to the
// REAL `scripts/roadmap_status_discovery.py`'s `read_lane_log` /
// `repos_from_lane_log` via `scripts/verify_lane_log_readable.py` (a thin,
// checked-in import-and-call wrapper — never a Rust reimplementation of
// the reader) and asserting on what that script actually returns.
//
// BEFORE/AFTER, recorded by hand against the same script (not asserted
// here, since the old struct no longer exists to construct — this is the
// direct evidence the defect existed and is now closed):
//   BEFORE (old `{repo, block_id, integrated_at}` line):
//     `{"entries": [{"repo": "repo-a", "block_id": "A.1", "integrated_at": "...”}],
//       "repos": ["repo-a"]}`
//     -- `repo` survives (as the ticket's own evidence said), but the
//        entry carries no `lane`, `block`, or `status` key at all: any
//        reader keying on those fields (which `/roadmap-status` and
//        `/consolidate-run` both do) sees nothing usable.
//   AFTER (new `{ts, lane, repo, block, status, note}` line):
//     `{"entries": [{"ts": "...", "lane": "repo-a", "repo": "repo-a",
//        "block": "A.1", "status": "closed", "note": "..."}],
//       "repos": ["repo-a"]}`
//     -- every field the readers key on is present and non-empty.

/// Locate `scripts/verify_lane_log_readable.py` relative to this crate —
/// `CARGO_MANIFEST_DIR` is `crates/engine-core`; the repo root (and its
/// `scripts/`) is two levels up.
fn verify_lane_log_readable_script() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/verify_lane_log_readable.py")
}

#[tokio::test]
async fn engine_written_lane_log_line_is_readable_by_the_real_discovery_script() {
    let script = verify_lane_log_readable_script();
    assert!(
        script.is_file(),
        "expected {} to exist (checked into this repo)",
        script.display()
    );

    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("prove-readability");
    let runner = RecordingRunner::new();
    let flow_runner = runner.into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("prove-readability-lane"),
        &|_: &StepProgress| {},
    )
    .await
    .expect("single-block chain should integrate cleanly");

    // Sanity: exactly one line was actually written before we ask the real
    // reader about it.
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 1, "expected exactly one engine-written line");

    // Now hand the REAL reader the directory the engine just wrote into —
    // no Rust-side reimplementation of `read_lane_log` in this test.
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg(&roadmap_dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn python3 {}: {e}", script.display()));

    assert!(
        output.status.success(),
        "verify_lane_log_readable.py failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "reader output was not valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });

    let entries = parsed["entries"]
        .as_array()
        .expect("entries must be an array");
    assert_eq!(
        entries.len(),
        1,
        "the real reader must see exactly one entry"
    );

    let entry = &entries[0];
    for field in ["repo", "lane", "block", "status"] {
        let value = entry
            .get(field)
            .unwrap_or_else(|| panic!("real reader's entry is missing '{field}'"))
            .as_str()
            .unwrap_or_else(|| panic!("real reader's entry field '{field}' is not a string"));
        assert!(
            !value.is_empty(),
            "real reader's entry field '{field}' must be non-empty, got {value:?}"
        );
    }
    assert_eq!(entry["repo"], "repo-a");
    assert_eq!(entry["lane"], "prove-readability-lane");
    assert_eq!(entry["block"], "A.1");
    assert_eq!(entry["status"], "closed");

    let repos = parsed["repos"].as_array().expect("repos must be an array");
    assert_eq!(
        repos.first().and_then(|v| v.as_str()),
        Some("repo-a"),
        "repos_from_lane_log must name the repo"
    );
}

// ── Cancellation: abort stops the chain BETWEEN steps ────────────────────

/// A [`FlowRunner`] that records every block it was invoked for and, the
/// moment it finishes the one block named `cancel_after`, cancels
/// `token` — deterministically simulating "a token cancelled after the
/// first step's invocation is observed" without racing a real wall-clock
/// sleep against the chain's own execution.
fn cancel_after_block(
    token: CancellationToken,
    cancel_after: &'static str,
) -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation| {
        let token = token.clone();
        let recorded = recorded.clone();
        Box::pin(async move {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            write_state(&invocation.repo_path, &invocation.block_id, "done");
            if invocation.block_id == cancel_after {
                token.cancel();
            }
            Ok(engine_contract::TaskContext {
                event: serde_json::json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    });
    (runner, calls)
}

#[tokio::test]
async fn cancellation_after_the_first_step_stops_the_chain_before_the_second_runs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(cancellation_after_the_first_step_stops_the_chain_before_the_second_runs_inner())
        .await;
}

// `integrate_chain`'s future is not `Send` (see the admission/hold tests'
// own doc comments), so this drives it via `LocalSet::spawn_local`.
async fn cancellation_after_the_first_step_stops_the_chain_before_the_second_runs_inner() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let admission = AdmissionGate::with_default_policy();
    let token = CancellationToken::new();
    let (flow_runner, calls) = cancel_after_block(token.clone(), "A.1");

    // Three steps in one repo so a naive implementation that only checked
    // cancellation once, at the very top, would still (wrongly) run all
    // three.
    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-a".to_string(), "A.2".to_string()),
        ("repo-a".to_string(), "A.3".to_string()),
    ]);

    let registry_path = registry.brain_root().to_path_buf();
    let roadmap_dir2 = roadmap_dir.clone();
    let handle = tokio::task::spawn_local(async move {
        let registry = RepoRegistry::from_brain_root(&registry_path).unwrap();
        integrate_chain(
            &chain,
            &no_deps,
            &always_met,
            &admission,
            &NeverHeld,
            Duration::from_millis(5),
            Some(&token),
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
        )
        .await
    });

    let outcomes = tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect("chain must not hang")
        .expect("task must not panic")
        .expect("a cancelled chain must return Ok, not Err");

    assert_eq!(
        outcomes.len(),
        1,
        "only A.1 should have integrated before the cancel"
    );
    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "exactly one runner invocation — A.2 and A.3 must never run"
    );
    assert_eq!(recorded[0], "A.1");

    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(
        lines.len(),
        1,
        "A.1's lane-log line stays; nothing after the cancel is appended"
    );
}

/// A chain parked on an operator hold that never clears must abort
/// promptly once cancelled — within roughly one poll tick, not after the
/// full (here, deliberately huge) `hold_poll_interval`. This is what
/// proves the token is *raced* against `wait_for_clearance`'s sleep
/// (`tokio::select!`) rather than only re-checked at the top of the loop.
struct AlwaysHeld;

impl HoldSource for AlwaysHeld {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        true
    }
}

#[tokio::test]
async fn a_chain_parked_on_a_never_clearing_hold_aborts_promptly_on_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(a_chain_parked_on_a_never_clearing_hold_aborts_promptly_on_cancel_inner())
        .await;
}

async fn a_chain_parked_on_a_never_clearing_hold_aborts_promptly_on_cancel_inner() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();
    let token = CancellationToken::new();

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    // Deliberately far longer than the timeout below asserts against —
    // if cancellation were only checked at the top of the loop (never
    // raced against the hold's own sleep), this test would have to wait
    // out the whole interval and would fail the timeout.
    let huge_poll_interval = Duration::from_secs(3600);

    let registry_path = registry.brain_root().to_path_buf();
    let roadmap_dir2 = roadmap_dir.clone();
    let token_for_task = token.clone();
    let handle = tokio::task::spawn_local(async move {
        let registry = RepoRegistry::from_brain_root(&registry_path).unwrap();
        integrate_chain(
            &chain,
            &no_deps,
            &always_met,
            &admission,
            &AlwaysHeld,
            huge_poll_interval,
            Some(&token_for_task),
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
        )
        .await
    });

    // Give the chain a moment to actually park inside `wait_for_clearance`
    // before cancelling.
    tokio::time::sleep(Duration::from_millis(20)).await;
    token.cancel();

    let outcomes = tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect(
            "a cancel against a held chain must return within one poll tick, not the full interval",
        )
        .expect("task must not panic")
        .expect("a cancelled chain must return Ok, not Err");

    assert_eq!(outcomes.len(), 0, "the held block never got to run");
    assert_eq!(
        runner.call_count(),
        0,
        "the runner must never have been invoked"
    );
}

// ── Per-step progress observer (Task 3) ──────────────────────────────────

/// A 3-step chain calls the observer exactly three times, in order, with
/// each `StepProgress` naming the right repo, block, 1-based index and the
/// chain's fixed total — a watcher must be able to tell 2-of-3 from 3-of-3.
#[tokio::test]
async fn n_step_chain_calls_the_observer_exactly_n_times_with_correct_indices() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-a".to_string(), "A.2".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let emitted: Arc<Mutex<Vec<StepProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let emitted_for_closure = emitted.clone();
    let observer = move |progress: &StepProgress| {
        emitted_for_closure.lock().unwrap().push(progress.clone());
    };

    let outcomes = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &observer,
    )
    .await
    .expect("three-step chain should integrate cleanly");

    assert_eq!(outcomes.len(), 3);

    let progress = emitted.lock().unwrap();
    assert_eq!(progress.len(), 3, "exactly one emission per completed step");

    assert_eq!(progress[0].repo, "repo-a");
    assert_eq!(progress[0].block_id, "A.1");
    assert_eq!(progress[0].index, 1);
    assert_eq!(progress[0].total, 3);
    assert_eq!(progress[0].status, "completed");

    assert_eq!(progress[1].repo, "repo-a");
    assert_eq!(progress[1].block_id, "A.2");
    assert_eq!(progress[1].index, 2);
    assert_eq!(progress[1].total, 3);

    assert_eq!(progress[2].repo, "repo-b");
    assert_eq!(progress[2].block_id, "B.1");
    assert_eq!(progress[2].index, 3);
    assert_eq!(progress[2].total, 3);
}

/// The observer fires only after the step's `lane-log.jsonl` line is on
/// disk — an observed step is always a recorded step. Asserted by reading
/// the lane log from inside the observer callback itself: at the moment
/// step k's observer runs, the log must already have k lines.
#[tokio::test]
async fn the_observer_fires_after_the_lane_log_line_is_appended() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let roadmap_dir_for_observer = roadmap_dir.clone();
    let observer = move |progress: &StepProgress| {
        let lines = lane_log_lines(&roadmap_dir_for_observer);
        assert_eq!(
            lines.len(),
            progress.index,
            "at step {}'s observer call, the lane log must already carry {} line(s)",
            progress.index,
            progress.index
        );
    };

    integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &observer,
    )
    .await
    .expect("two-step chain should integrate cleanly");
}

/// With no observer injected (a no-op closure), the chain's outcomes and
/// lane-log output are unchanged from any other run — the seam adds no
/// side effect on its own.
#[tokio::test]
async fn no_observer_injected_changes_nothing() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("close-the-loop");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    let outcomes = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
    )
    .await
    .expect("two-repo chain should integrate cleanly");

    assert_eq!(outcomes.len(), 2);
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 2, "exactly one lane-log line per block");
}
