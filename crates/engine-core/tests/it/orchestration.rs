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

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::{
    resolve_explicit_chain, resolve_lane_chain, ChainError,
};
use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, HoldSource, IntegrateError, NeverHeld,
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
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
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
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
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
            &always_flow,
            &registry,
            &flow_runner2,
            &roadmap_dir2,
            None,
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
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
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
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
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
