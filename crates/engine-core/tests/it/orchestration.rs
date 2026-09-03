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

use uuid::Uuid;

use engine_contract::TaskContext;
use engine_core::cancellation::CancellationToken;
use engine_core::policy::permission::{resolve_permission_profile, GatedAction, PermissionProfile};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::{
    resolve_explicit_chain, resolve_lane_chain, ChainError, ChainStep, StepKind,
};
use engine_core::workflows::orchestration::debrief::{brief_names_every_bail, StubJournalReader};
use engine_core::workflows::orchestration::dispatch::DispatchStepError;
use engine_core::workflows::orchestration::execute::{
    execute_step, EngineKind, ExecuteError, FlowInvocation, FlowRunner,
};
use engine_core::workflows::orchestration::gates::{
    check_permission_gate, check_step_with_frontier_advice, load_frontier, AdmissionGate,
    DependencyEdge, FrontierError, GateError, OperatorGateRequest, PermissionGateError,
};
use engine_core::workflows::orchestration::graph::{
    debrief_registry, debrief_schema, OrchestrationRunNode, NODE_NAME,
};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, integrate_chain_with_dispatch, HoldSource, IntegrateError, NeverHeld,
    StepProgress,
};
use engine_core::{
    Dispatcher, Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema,
};

use engine_core::nodes::channel_transport::StubChannelTransport;
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

/// Like [`two_repo_registry`], but the fixture `brain.toml` ALSO declares a real
/// `[permission_profiles]` table (`EN.12.C` task 6) — the same three-level shape as
/// `permission_profiles_brain.toml` (`permission.rs` task 1's own fixture), except
/// `default` is parameterized so a test can stand up a `locked`-resolving registry
/// and an `unrestricted`-resolving one side by side without two near-duplicate
/// fixture files.
fn two_repo_registry_with_profile(default: &str) -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        format!(
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
             [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n\
             \n\
             [permission_profiles]\n\
             never_allowed = [\"clear_operator_gate\"]\n\
             default = \"{default}\"\n\
             \n\
             [permission_profiles.levels.locked]\n\
             id = \"locked\"\n\
             mini_install = false\n\
             main_push = false\n\
             cross_repo_write = false\n\
             \n\
             [permission_profiles.levels.standard]\n\
             id = \"standard\"\n\
             mini_install = false\n\
             main_push = true\n\
             cross_repo_write = true\n\
             \n\
             [permission_profiles.levels.unrestricted]\n\
             id = \"unrestricted\"\n\
             mini_install = true\n\
             main_push = true\n\
             cross_repo_write = true\n"
        ),
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
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
        vec![DependencyEdge::Block {
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
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
            None,
            None,
            &always_flow,
            &registry,
            &flow_runner2,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
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
            None,
            None,
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
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
            // `EN.11.F` task 4: this fixture predates the cancellation/
            // budget-halt statuses (they exist only in-process, produced
            // by `integrate_chain` itself — no hand-written fixture line
            // uses them), so this round-trip test has nothing to flag for
            // either.
            engine_core::workflows::orchestration::integrate::LaneLogStatus::Cancelled
            | engine_core::workflows::orchestration::integrate::LaneLogStatus::BudgetHalted => {}
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

/// Walk up from `start` looking for a `brain.toml` — the marker of the
/// company-brain vault root that `scripts/verify_lane_log_readable.py`
/// itself needs above this repo to locate
/// `scripts/roadmap_status_discovery.py`. Returns the first ancestor
/// directory (inclusive of `start`) that contains one, or `None` if the
/// walk reaches the filesystem root without finding it.
fn find_brain_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("brain.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

#[tokio::test]
async fn engine_written_lane_log_line_is_readable_by_the_real_discovery_script_when_brain_root_present(
) {
    let script = verify_lane_log_readable_script();
    // Genuine invariant, not the skip gate: the script is checked into this
    // repo and is therefore always present regardless of brain-vault
    // context. The actual environment-dependent guard is the brain-root
    // walk below, mirroring `round_trip.rs`'s
    // `fixture_matches_orchestrator_owned_original_when_sibling_checkout_present`.
    assert!(
        script.is_file(),
        "expected {} to exist (checked into this repo)",
        script.display()
    );

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(_brain_root) = find_brain_root(manifest_dir) else {
        eprintln!(
            "skipping: no brain.toml found walking up from {}",
            manifest_dir.display()
        );
        return;
    };

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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("prove-readability-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
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
            None,
            Some(&token),
            None,
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
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

    // `EN.11.F` task 4: a cancellation win is now itself recorded — one
    // `closed` line for A.1 (unchanged), plus one `cancelled` line naming
    // A.2, the block that never started. Before task 4 a cancel win left
    // the chain's record silent about *why* it stopped short; the block's
    // AC requires a clean abort, a budget halt, and a failure to be three
    // distinguishable terminal states in that record, so the cancel is no
    // longer a silent stop.
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(
        lines.len(),
        2,
        "A.1's `closed` line stays, plus one `cancelled` line naming the block that never started"
    );
    assert_eq!(lines[0]["status"], "closed");
    assert_eq!(lines[0]["block"], "A.1");
    assert_eq!(lines[1]["status"], "cancelled");
    assert_eq!(lines[1]["block"], "A.2");
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
            None,
            Some(&token_for_task),
            None,
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir2,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &observer,
        false,
        Uuid::new_v4(),
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &observer,
        false,
        Uuid::new_v4(),
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
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("two-repo chain should integrate cleanly");

    assert_eq!(outcomes.len(), 2);
    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 2, "exactly one lane-log line per block");
}

// ── Isolation invariance matrix (EN.ticket.orchestration-isolation-passthrough) ──
//
// Drives the full ORCHESTRATION stack end to end (event -> `resolve_policy_for_run_from`
// -> `execute::resolve_isolation` -> the stamped `ctx.nodes[NODE_NAME]["blocks"]`) for
// every (repo x setting) cell of the isolation policy's invariance matrix, asserting the
// same fact the production caller reads: the per-step resolved `use_worktree` stamped
// into the node's result. Table-driven rather than one test per cell so the matrix shape
// itself — three repo rows, five settings columns — reads directly off the test.

/// A tempdir brain root with three repos wired for the isolation matrix:
/// - `base-template` — row 1, always worktree, structurally unreachable by any knob.
/// - `hq` — resolves its `repo_path` to the brain root itself (mirrors HQ, where the
///   chain's own repo IS the brain root) — row 2, always in place.
/// - `ordinary-repo` — row 3, follows the resolved `default_use_worktree`.
fn isolation_matrix_brain_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("base-template")).unwrap();
    std::fs::create_dir_all(dir.path().join("ordinary-repo")).unwrap();
    std::fs::create_dir_all(
        dir.path()
            .join("planning")
            .join("roadmaps")
            .join("isolation-matrix"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"base-template\"\nrepo_path = \"base-template\"\n\
         [[repos]]\nslug = \"ordinary-repo\"\nrepo_path = \"ordinary-repo\"\n\
         [[repos]]\nslug = \"hq\"\nrepo_path = \".\"\n",
    )
    .unwrap();
    dir
}

/// A `FlowRunner` that satisfies `integrate_chain`'s state-write verification (writes
/// `"done"` into the invocation's own `repo_path`) and otherwise records nothing — the
/// matrix reads isolation off the node's stamped `ctx.nodes` result, not off this
/// double, so it stays minimal.
fn isolation_matrix_run_flow() -> FlowRunner {
    Arc::new(move |invocation: FlowInvocation| {
        Box::pin(async move {
            write_state(&invocation.repo_path, &invocation.block_id, "done");
            Ok(engine_contract::TaskContext {
                event: serde_json::json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    })
}

/// Run one ORCHESTRATION step for `repo_slug` with `event_overlay` merged into the
/// event, and return the `use_worktree` the node actually stamped for that step.
async fn matrix_cell_use_worktree(
    dir: &Path,
    repo_slug: &str,
    block_id: &str,
    event_overlay: serde_json::Value,
) -> bool {
    let node = OrchestrationRunNode::new().with_run_flow(isolation_matrix_run_flow());
    let mut event = serde_json::json!({
        "brain_root": dir,
        "blocks": [{ "repo": repo_slug, "block_id": block_id }],
        "roadmap_slug": "isolation-matrix",
    });
    if let (Some(event_obj), Some(overlay_obj)) = (event.as_object_mut(), event_overlay.as_object())
    {
        for (key, value) in overlay_obj {
            event_obj.insert(key.clone(), value.clone());
        }
    }
    let ctx = TaskContext {
        event,
        nodes: std::collections::HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: std::collections::HashMap::new(),
    };
    let out = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("process should succeed for {repo_slug}/{block_id}: {err}"));
    out.nodes[NODE_NAME]["blocks"][0]["use_worktree"]
        .as_bool()
        .expect("stamped use_worktree should be a bool")
}

/// The five ways a run can reach a resolved `default_use_worktree`, matched against the
/// three repo rows below. `"default_false"`/`"default_true"` set the fallback via a
/// `planning/harness.json` `orchestration.policy.default_use_worktree` entry (the
/// `source`-read layer `resolve_policy_for_run_from` consults); the two profiles set it
/// via the built-in bundles; the event override sets it inline on the event itself, one
/// layer higher than every other setting here.
enum Setting {
    DefaultFalse,
    DefaultTrue,
    ProfileCheapFast,
    ProfileThorough,
    EventOverride(bool),
}

impl Setting {
    fn write_harness_default(&self, dir: &Path) {
        let harness_path = dir.join("planning").join("harness.json");
        let value = match self {
            Setting::DefaultFalse => Some(false),
            Setting::DefaultTrue => Some(true),
            // Profiles and the event override do not go through the
            // harness.json `orchestration.policy` layer at all, so no file
            // is written for them — leaving that layer absent proves the
            // profile/event layers are what actually carried the setting.
            Setting::ProfileCheapFast | Setting::ProfileThorough | Setting::EventOverride(_) => {
                None
            }
        };
        if let Some(default_use_worktree) = value {
            std::fs::write(
                &harness_path,
                serde_json::json!({
                    "orchestration": {
                        "policy": { "default_use_worktree": default_use_worktree }
                    }
                })
                .to_string(),
            )
            .unwrap();
        } else if harness_path.exists() {
            std::fs::remove_file(&harness_path).unwrap();
        }
    }

    fn event_overlay(&self) -> serde_json::Value {
        match self {
            Setting::DefaultFalse | Setting::DefaultTrue => serde_json::json!({}),
            Setting::ProfileCheapFast => serde_json::json!({ "profile": "cheap-fast" }),
            Setting::ProfileThorough => serde_json::json!({ "profile": "thorough" }),
            Setting::EventOverride(value) => serde_json::json!({
                "policy": { "default_use_worktree": value }
            }),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Setting::DefaultFalse => "default-false",
            Setting::DefaultTrue => "default-true",
            Setting::ProfileCheapFast => "profile-cheap-fast",
            Setting::ProfileThorough => "profile-thorough",
            Setting::EventOverride(true) => "event-override-true",
            Setting::EventOverride(false) => "event-override-false",
        }
    }
}

/// The 15-cell invariance matrix: {`base-template`, the brain root, an ordinary repo} x
/// {default false, default true, profile `cheap-fast`, profile `thorough`, an inline
/// event `policy` override}. Both non-negotiable rows (`base-template`, the brain root)
/// must resolve identically across every column — that is the assertion that
/// distinguishes "the policy is structurally enforced" from "the default happens to
/// agree with it today". `ordinary-repo` is expected to track the setting's resolved
/// `default_use_worktree` exactly.
#[tokio::test]
async fn isolation_invariance_matrix_across_all_fifteen_cells() {
    let dir = isolation_matrix_brain_root();

    let settings = [
        Setting::DefaultFalse,
        Setting::DefaultTrue,
        Setting::ProfileCheapFast,
        Setting::ProfileThorough,
        Setting::EventOverride(true),
    ];

    // Ordinary-repo's expected resolution per setting, in the same order as
    // `settings` above — this is the ONE row allowed to vary across the
    // settings axis. `cheap-fast` now resolves true: EN.ticket.orchestration-worktree-by-default
    // flipped isolation from a cost knob to a correctness precondition (deliberate exception to
    // CLAUDE.md standing rule 6), so cheap-fast no longer economises on this axis.
    let ordinary_expected = [false, true, true, true, true];

    for (i, setting) in settings.iter().enumerate() {
        setting.write_harness_default(dir.path());
        let overlay = setting.event_overlay();
        let label = setting.label();

        let base_template = matrix_cell_use_worktree(
            dir.path(),
            "base-template",
            &format!("BT.{i}"),
            overlay.clone(),
        )
        .await;
        assert!(
            base_template,
            "base-template must resolve to worktree=true under setting {label}"
        );

        let brain_root =
            matrix_cell_use_worktree(dir.path(), "hq", &format!("HQ.{i}"), overlay.clone()).await;
        assert!(
            !brain_root,
            "the brain root must resolve to worktree=false under setting {label}"
        );

        let ordinary =
            matrix_cell_use_worktree(dir.path(), "ordinary-repo", &format!("ORD.{i}"), overlay)
                .await;
        assert_eq!(
            ordinary, ordinary_expected[i],
            "ordinary-repo should resolve to {} under setting {label}, got {ordinary}",
            ordinary_expected[i]
        );
    }

    // Clean up the harness.json this test wrote so it never leaks into a
    // sibling test running against the same tempdir contents.
    let harness_path = dir.path().join("planning").join("harness.json");
    if harness_path.exists() {
        std::fs::remove_file(&harness_path).unwrap();
    }
}

/// Isolation-by-default pin (EN.ticket.orchestration-worktree-by-default): a chain wired
/// with no isolation overrides at all — no `harness.json` `orchestration.policy` entry,
/// no `profile`, no event `policy` override — now seeds `use_worktree: true` for an
/// ordinary repo. This is a DELIBERATE exception to CLAUDE.md standing rule 6's
/// behavior-stability clause: isolation was reclassified from a cost/quality knob to a
/// correctness precondition after an unstated-policy ORCHESTRATION dispatch ran in-place
/// in a shared checkout a concurrent session was also using, silently landing that run's
/// commits on the other session's branch. A safe default costs a worktree; an unsafe one
/// costs a cherry-pick recovery and a dead PR. This test previously pinned the OLD
/// (in-place) behavior — it now pins the new one, on purpose.
#[tokio::test]
async fn a_run_with_no_isolation_overrides_seeds_in_place_unchanged_from_today() {
    let dir = isolation_matrix_brain_root();

    let use_worktree = matrix_cell_use_worktree(
        dir.path(),
        "ordinary-repo",
        "NOOVERRIDE.1",
        serde_json::json!({}),
    )
    .await;

    assert!(
        use_worktree,
        "an override-free run against an ordinary repo must isolate by default: \
         default_use_worktree is a correctness precondition, not a cost knob \
         (EN.ticket.orchestration-worktree-by-default)"
    );
}

// ── Per-step cost/token attribution does not bleed across steps (EN.11.G task 3) ──

/// A [`FlowRunner`] test double that returns a DIFFERENT known cost/token
/// figure per `block_id` — the smallest fixture that can catch bleed. A
/// single-step test cannot: with only one step there is nothing for a
/// second step's figures to leak into, so the two-step shape here is load
/// bearing, not incidental.
fn runner_with_per_block_usage(figures: Vec<(&'static str, f64, u64, u64)>) -> FlowRunner {
    let figures: std::collections::HashMap<String, (f64, u64, u64)> = figures
        .into_iter()
        .map(|(block_id, cost, input, output)| (block_id.to_string(), (cost, input, output)))
        .collect();
    let figures = Arc::new(figures);
    Arc::new(move |invocation: FlowInvocation| {
        let figures = figures.clone();
        Box::pin(async move {
            let (cost_usd, input, output) = *figures
                .get(&invocation.block_id)
                .expect("fixture should have a figure for every block in the chain");
            write_state(&invocation.repo_path, &invocation.block_id, "done");
            let node_name = format!("{}Node", invocation.block_id);
            let mut nodes = std::collections::HashMap::new();
            nodes.insert(node_name.clone(), serde_json::json!({"cost_usd": cost_usd}));
            let mut node_runs = std::collections::HashMap::new();
            node_runs.insert(
                node_name,
                engine_contract::NodeRun {
                    status: engine_contract::NodeRunStatus::Success,
                    started_at: None,
                    completed_at: None,
                    error: None,
                    input: None,
                    usage: Some(engine_contract::Usage {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        model: "claude-sonnet-4-5".to_string(),
                    }),
                },
            );
            Ok(TaskContext {
                event: serde_json::json!({}),
                nodes,
                metadata: serde_json::json!({}),
                node_runs,
            })
        })
    })
}

/// Proves per-step attribution over a REAL two-step chain: each step's
/// child reports a different cost/token figure, and each step's own
/// `ExecutionOutcome` must carry exactly its own figure — never the other
/// step's, and never a sum of both (which would look like correct
/// "rollup" arithmetic while actually being step-B's figure bleeding into
/// step-A's read).
///
/// **Bleed-test-can-fail check (task 3 methodology, not left in the
/// production tree — CLAUDE.md D8 completeness self-check / task AC
/// "no production file may differ from HEAD at task end"):** to confirm
/// this test actually catches attribution bleed rather than passing
/// vacuously, `step_spend` in `execute.rs` was temporarily rewritten to
/// route through a single `static` `BudgetLedger` shared across every
/// call (i.e. step B's `step_spend` folded step A's already-recorded
/// cost/tokens into its own reading) instead of building a fresh ledger
/// from that step's own `ctx` each time. Run against that deliberately
/// bugged build, this test failed with:
///
/// ```text
/// assertion `left == right` failed: step B's cost must be its own figure, not \
/// bled from step A
///   left: Some(4.75)
///  right: Some(3.5)
/// ```
///
/// (`4.75` == `1.25 + 3.5`, i.e. step A's `1.25` had leaked into step B's
/// reading — exactly the bleed this test exists to catch.) The temporary
/// `execute.rs` edit was then reverted with `git checkout -- execute.rs`
/// and the suite re-run clean; no production file differs from this
/// task's starting `HEAD`.
#[tokio::test]
async fn two_step_chain_attributes_cost_and_tokens_to_the_step_that_spent_them_without_bleed() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("no-bleed");
    let admission = AdmissionGate::with_default_policy();

    let runner = runner_with_per_block_usage(vec![("A.1", 1.25, 100, 200), ("B.1", 3.50, 10, 20)]);

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
        None,
        None,
        &always_flow,
        &registry,
        &runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("two-step chain with distinct per-step usage should integrate cleanly");

    assert_eq!(outcomes.len(), 2);

    assert_eq!(
        outcomes[0].cost_usd,
        Some(1.25),
        "step A's cost must be its own figure, not padded by step B"
    );
    assert_eq!(
        outcomes[0].total_tokens, 300,
        "step A's tokens must be its own figure, not padded by step B"
    );

    assert_eq!(
        outcomes[1].cost_usd,
        Some(3.50),
        "step B's cost must be its own figure, not bled from step A"
    );
    assert_eq!(
        outcomes[1].total_tokens, 30,
        "step B's tokens must be its own figure, not bled from step A"
    );

    // The two steps' figures are actually distinct — a bleed bug that
    // duplicated step A's ledger into step B (or vice versa) would collapse
    // this into a false equality.
    assert_ne!(outcomes[0].cost_usd, outcomes[1].cost_usd);
    assert_ne!(outcomes[0].total_tokens, outcomes[1].total_tokens);
}

// ── `EN.11.K` Task 3 — frontier/`corpus_gates` parity fixture ─────────────
//
// The AC "the engine's lane-head startability answer matches `mev frontier --json`
// for the same lane" is un-gateable by a normal in-repo assertion for the same reason
// `corpus_gates_parity.rs` names for its own AC: `mev` is a separate repo
// (`core/mev`) and an installed binary, not something this workspace can invoke.
// This module stands in for that AC the only way available: a checked-in
// `lane-frontier.json` fixture reproduced verbatim from a real `mev frontier --json`
// run, with mev's recorded answer cross-checked against
// [`check_step_with_frontier_advice`] and [`GateError`] for the same lane head.
//
// # Provenance (fill this in again if the fixture below ever changes)
//
// - **Binary**: the *installed* `mev` at `~/.cargo/bin/mev` (NOT a source build of
//   `core/mev` run via `cargo run`) — `which mev` resolves there on this machine, per
//   `corpus_gates_parity.rs`'s own recorded provenance for the same machine.
// - **Version**: `mev --version` -> `mev 0.1.0`.
// - **Invocation**: `mev frontier <brain-root> --json`, run against a fixture tree with
//   one lane file per case under `planning/roadmaps/orchestration-extensions/
//   lane-engine-rs.txt`, naming `EN.11.E` as the lane's sole block — mirroring the
//   live condition this block's spec section "Why" records: at spec time the real
//   `lane-frontier.json`'s engine-rs head is `EN.11.E`, reported `startable: true` with
//   `unmet_gates: []`, while `EN.11.E` is in fact HELD on an operator decision the
//   graph cannot express. The JSON below is that recorded entry, reproduced verbatim.
//
// # Coverage
//
// Three cases the AC names: agreement (frontier startable, per-edge check has nothing
// outstanding either — both signals concur), disagreement (frontier says startable,
// but the live per-edge check still refuses — the refusal must win, never the
// frontier's optimism), and the absent-file case (a missing `lane-frontier.json` must
// fail loudly, naming the path, never a silent `false` or `true`).

/// Recorded from `mev frontier <fixture-root> --json` against a single-lane fixture
/// tree whose only block is `EN.11.E` (see module doc for the exact invocation and
/// provenance). Reproduced here verbatim — this is the real measured shape named in
/// the block's `what`: `{derived_at, entries[], gate_ranks[]}`, one entry for the
/// `orchestration-extensions`/`engine-rs` lane, segment 0.
const RECORDED_FRONTIER: &str = r#"{
    "derived_at": "2026-08-21T00:00:00-07:00",
    "entries": [
        {
            "roadmap": "orchestration-extensions",
            "lane": "engine-rs",
            "segment": 0,
            "repo": "engine-rs",
            "key": "engine-rs:EN.11.E",
            "id": "EN.11.E",
            "title": "Frontier for startability",
            "status": "open",
            "unmet_blocks": [],
            "unmet_gates": [],
            "startable": true
        }
    ],
    "gate_ranks": []
}"#;

fn engine_rs_lane_head_step() -> ChainStep {
    ChainStep {
        repo: "engine-rs".to_string(),
        block_id: "EN.11.E".to_string(),
        directives: None,
        roadmap: Some("orchestration-extensions".to_string()),
        lane: Some("engine-rs".to_string()),
        segment: Some(0),
        kind: StepKind::Block,
    }
}

/// Write [`RECORDED_FRONTIER`] to a tempdir path standing in for a repo's
/// `planning/lane-frontier.json` and load it back through the reader under test, so
/// every case here exercises the real parse path, not a hand-built [`FrontierArtifact`].
fn write_recorded_frontier(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("lane-frontier.json");
    std::fs::write(&path, RECORDED_FRONTIER).expect("write recorded frontier fixture");
    path
}

#[test]
fn frontier_parity_agreement_both_signals_concur_and_the_step_may_start() {
    // mev: engine-rs's lane head EN.11.E is startable=true, unmet_gates=[]. Stand the
    // live per-edge check in with an agreeing double (nothing outstanding) — the
    // agreement case named in the AC.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_recorded_frontier(&dir);
    let artifact = load_frontier(&path).expect("recorded fixture must parse");

    let step = engine_rs_lane_head_step();
    let (gate_result, head) =
        check_step_with_frontier_advice(&step, Some(&artifact), &no_deps, &always_met);

    let head = head.expect("frontier entry for engine-rs's lane head must be found");
    assert_eq!(head.id, "EN.11.E");
    assert!(
        head.startable,
        "mev's recorded answer for this lane head is startable:true"
    );
    assert!(head.unmet_gates.is_empty());
    assert!(
        gate_result.is_ok(),
        "both signals agree the step may start: {gate_result:?}"
    );
}

#[test]
fn frontier_parity_disagreement_per_edge_refusal_wins_over_frontier_optimism() {
    // Live condition named in the block's Why: mev's own recorded engine-rs head is
    // startable:true with unmet_gates:[] while EN.11.E is in fact HELD on an operator
    // decision the graph cannot express. Stand the live per-edge check in with a
    // refusing double — the disagreement case named in the AC. The refusal must win.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_recorded_frontier(&dir);
    let artifact = load_frontier(&path).expect("recorded fixture must parse");

    let step = engine_rs_lane_head_step();
    let held_operator_edge = |_repo: &str, _block_id: &str| {
        vec![DependencyEdge::Operator {
            slug: "d20-contract-authorship".to_string(),
        }]
    };
    let nothing_met = |_repo: &str, _block_id: &str| false;

    let (gate_result, head) =
        check_step_with_frontier_advice(&step, Some(&artifact), &held_operator_edge, &nothing_met);

    let head = head.expect("frontier entry must still be found even though it disagrees");
    assert!(
        head.startable,
        "mev's recorded answer for this lane head is startable:true — the frontier's \
         own optimism must be visible even as the gate refuses"
    );
    assert!(head.unmet_gates.is_empty());
    assert!(
        gate_result.is_err(),
        "the live per-edge check must refuse despite frontier startable:true, never the \
         other way around"
    );
    match gate_result.unwrap_err() {
        GateError::UnmetDependency {
            edge: DependencyEdge::Operator { slug },
            ..
        } => assert_eq!(slug, "d20-contract-authorship"),
        other => panic!("expected an Operator UnmetDependency naming the held gate, got {other:?}"),
    }
}

#[test]
fn frontier_parity_absent_file_fails_loudly_naming_the_path() {
    // The third case the AC names: a missing lane-frontier.json must fail loudly and
    // name the path — never a silent "not startable" (which reads as an ordinary hold)
    // and never a silent "startable".
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist").join("lane-frontier.json");

    let err = load_frontier(&path).expect_err("a missing file must not parse to anything");
    match &err {
        FrontierError::Missing { path: p } => assert_eq!(p, &path),
        other => panic!("expected FrontierError::Missing, got {other:?}"),
    }
    let msg = err.to_string();
    assert!(
        msg.contains(&path.display().to_string()),
        "the failure must name the missing path: {msg}"
    );
}

// ── `EN.12.E` Task 6 — mixed `[dispatch, block]` chain, end to end ────────
//
// `chain.rs` and `integrate.rs` each carry thorough unit coverage for the
// dispatch step already (see `dispatch.rs`'s own `#[cfg(test)]` module and
// `integrate.rs`'s `integrate_chain_with_dispatch` tests). What is missing
// is a case that drives the WHOLE read path — a real `lane-segments.json`
// fixture, through `resolve_lane_chain`, into `integrate_chain_with_dispatch`
// — so a regression at any seam between those modules (not just inside one
// of them) is caught here in the integration suite, per the block's own
// `testing_strategy`.

/// A single-node marker workflow a research-style dispatch step can be
/// dispatched to — mirrors `dispatch.rs`'s own fixture rather than
/// reimplementing it, since this suite drives the same modules through
/// their public API.
struct MixedChainMarkerNode;

#[async_trait::async_trait]
impl Node for MixedChainMarkerNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "research": "done" }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "MixedChainMarkerNode"
    }
}

fn mixed_chain_fixture_schema(workflow_type: &str) -> WorkflowSchema {
    let mut nodes = std::collections::HashMap::new();
    nodes.insert(
        "MixedChainMarkerNode".to_string(),
        NodeConfig::new("MixedChainMarkerNode", vec![]),
    );
    WorkflowSchema::new(workflow_type, "MixedChainMarkerNode", nodes)
}

/// A `Dispatcher` with `"RESEARCH_AGENT"` registered against the marker
/// workflow above — enough to exercise a real dispatch step's routing
/// without a real registered production workflow.
fn dispatcher_with_research_agent() -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        mixed_chain_fixture_schema("RESEARCH_AGENT"),
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(MixedChainMarkerNode));
            Ok(Workflow::new(
                registry,
                mixed_chain_fixture_schema("RESEARCH_AGENT"),
            ))
        }),
    );
    dispatcher
}

fn recording_journal_sink() -> (
    Arc<dyn Fn(engine_contract::JournalRow) + Send + Sync>,
    Arc<Mutex<Vec<engine_contract::JournalRow>>>,
) {
    let rows: Arc<Mutex<Vec<engine_contract::JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_rows = rows.clone();
    let sink: Arc<dyn Fn(engine_contract::JournalRow) + Send + Sync> =
        Arc::new(move |row: engine_contract::JournalRow| sink_rows.lock().unwrap().push(row));
    (sink, rows)
}

/// A `[research, block]` mixed chain, parsed from a real `lane-segments.json`
/// fixture via `resolve_lane_chain` (the same read path mev's real output
/// goes through), completes with both steps reported: the dispatch step's
/// result is in the journal before the block step runs, and the block step
/// still produces its usual one `lane-log.jsonl` line — the dispatch step
/// writes none. Acceptance criterion 1 of `EN.12.E`.
#[tokio::test]
async fn mixed_research_block_chain_journals_research_before_block_runs() {
    let (repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("mixed-research-block");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();
    let dispatcher = dispatcher_with_research_agent();
    let (sink, rows) = recording_journal_sink();
    let sink_fn = sink.clone();
    let _ = &repos_dir;

    let lane_segments_dir = tempfile::tempdir().unwrap();
    let json = r#"{
        "blocks": [
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "RESEARCH_AGENT", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "kind": "dispatch"},
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 2, "segment": 1, "position": 1, "origin_roadmap": null}
        ]
    }"#;
    let path = lane_segments_dir.path().join("lane-segments.json");
    std::fs::write(&path, json).unwrap();

    let chain = resolve_lane_chain(&path, "r", "l", &|_| false)
        .expect("a [dispatch, block] lane must resolve");
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].kind, StepKind::Dispatch);
    assert_eq!(chain[1].kind, StepKind::Block);

    let outcomes = integrate_chain_with_dispatch(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        roadmap_dir.as_path(),
        Some("l"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
        Some(&move |row| sink_fn(row)),
        &dispatcher,
    )
    .await
    .expect("a registered [research, block] chain should integrate end to end");

    assert_eq!(
        outcomes.len(),
        1,
        "only the block step produces an ExecutionOutcome; the dispatch step does not: {outcomes:?}"
    );
    assert_eq!(
        runner.call_count(),
        1,
        "only the block step reaches the SDLC runner"
    );

    let recorded = rows.lock().unwrap();
    let research_idx = recorded
        .iter()
        .position(|r| {
            r.kind == engine_contract::JournalDecisionKind::StepIntegrated
                && r.step == "RESEARCH_AGENT"
        })
        .expect("the research step's journal row must be present");
    let block_idx = recorded
        .iter()
        .position(|r| {
            r.kind == engine_contract::JournalDecisionKind::StepIntegrated && r.step == "A.1"
        })
        .expect("the block step's journal row must be present");
    assert!(
        research_idx < block_idx,
        "the research step's journal row must land before the block step's: {recorded:?}"
    );

    let lane_log = lane_log_lines(&roadmap_dir);
    assert_eq!(
        lane_log.len(),
        1,
        "the dispatch step must write no lane-log.jsonl line; only the block step does: \
         {lane_log:?}"
    );
}

/// A dispatch step naming an unregistered workflow key stops the chain with
/// a named diagnostic, reached through the full integration path (a real
/// lane-segments fixture, not just the unit-level `Dispatcher` calls in
/// `integrate.rs`'s own tests) — acceptance criterion 5 of `EN.12.E`.
#[tokio::test]
async fn unregistered_dispatch_key_stops_the_chain_through_the_integration_path() {
    let (repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("mixed-unregistered-dispatch");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();
    // No workflow keys registered at all — "NOT_REGISTERED" must not resolve.
    let dispatcher = Dispatcher::new();
    let (sink, rows) = recording_journal_sink();
    let sink_fn = sink.clone();
    let _ = &repos_dir;

    let lane_segments_dir = tempfile::tempdir().unwrap();
    let json = r#"{
        "blocks": [
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "NOT_REGISTERED", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "kind": "dispatch"},
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 2, "segment": 1, "position": 1, "origin_roadmap": null}
        ]
    }"#;
    let path = lane_segments_dir.path().join("lane-segments.json");
    std::fs::write(&path, json).unwrap();

    let chain = resolve_lane_chain(&path, "r", "l", &|_| false)
        .expect("the lane must still resolve to a chain — the failure is at integrate time");

    let err = integrate_chain_with_dispatch(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        roadmap_dir.as_path(),
        Some("l"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
        Some(&move |row| sink_fn(row)),
        &dispatcher,
    )
    .await
    .expect_err("an unregistered workflow key must stop the chain, never fall through to block");

    match &err {
        IntegrateError::Dispatch(DispatchStepError::UnknownWorkflowKey {
            workflow_key, ..
        }) => {
            assert_eq!(workflow_key, "NOT_REGISTERED");
        }
        other => panic!("expected IntegrateError::Dispatch(UnknownWorkflowKey), got {other:?}"),
    }
    assert_eq!(
        runner.call_count(),
        0,
        "the block step must never run once the dispatch step stops the chain"
    );

    let recorded = rows.lock().unwrap();
    assert!(
        recorded.iter().any(
            |r| r.kind == engine_contract::JournalDecisionKind::StepBailed
                && r.step == "NOT_REGISTERED"
        ),
        "expected a StepBailed row naming the unregistered key: {recorded:?}"
    );
}

/// The forward-compatibility parse case from Task 1, exercised end to end
/// through `resolve_lane_chain` (not just `chain.rs`'s own unit test): an
/// unrecognised field on a lane-segment's `directives` must still parse,
/// and the recognised sibling fields alongside it must be carried through
/// correctly onto the resolved `ChainStep`.
#[test]
fn forward_compatible_lane_segment_field_parses_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let json = r#"{
        "blocks": [
            {"roadmap": "r", "lane": "l", "repo": "repo-a", "id": "A.1", "line": 1, "segment": 0, "position": 0, "origin_roadmap": null, "kind": "block", "directives": {"held_until": "OTHER.1", "some_future_mev_field": "value"}}
        ]
    }"#;
    let path = dir.path().join("lane-segments.json");
    std::fs::write(&path, json).unwrap();

    let steps = resolve_lane_chain(&path, "r", "l", &|_| false)
        .expect("an unrecognised forward-compatible field must not hard-fail the whole file");

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].kind, StepKind::Block);
    assert_eq!(
        steps[0]
            .directives
            .as_ref()
            .and_then(|d| d.held_until.clone()),
        Some("OTHER.1".to_string()),
        "the recognised directive fields must still parse correctly alongside the unknown one"
    );
}

// ── `DEBRIEF` integration coverage (`EN.12.G` task 5) ────────────────────
//
// Four checked-in campaign journal fixtures — clean, bailed, operator-
// waiting, empty — driven through the actual registered `DEBRIEF`
// workflow (`debrief_schema`/`debrief_registry` + `Workflow::run`), not
// just `DebriefNode::process` directly, so this suite also exercises the
// graph assembly `EN.12.G` task 4 wired in. `StubJournalReader` plus
// `StubChannelTransport` keep every test hermetic: no database, no live
// `CONTENT_PIPELINE` dispatch.

fn debrief_row(
    campaign_id: &str,
    step: &str,
    kind: engine_contract::JournalDecisionKind,
    reason: &str,
    offset_secs: i64,
) -> engine_contract::JournalRow {
    engine_contract::JournalRow {
        id: Uuid::new_v4(),
        campaign_id: campaign_id.to_string(),
        run_id: Uuid::new_v4(),
        step: step.to_string(),
        kind,
        reason: reason.to_string(),
        detail: serde_json::json!({}),
        created_at: chrono::Utc::now() + chrono::Duration::seconds(offset_secs),
    }
}

/// Run the real registered `DEBRIEF` workflow (schema + registry, `EN.12.G`
/// task 4) over `rows` for `campaign_id`, hermetically — a
/// `StubJournalReader` seeded with `rows` and a succeeding
/// `StubChannelTransport`. Returns the rendered brief and the transport's
/// recorded calls.
async fn run_debrief_workflow(
    campaign_id: Uuid,
    rows: Vec<engine_contract::JournalRow>,
) -> (String, Arc<StubChannelTransport>) {
    let transport = Arc::new(StubChannelTransport::succeeding());
    let registry = debrief_registry(
        Arc::new(StubJournalReader::succeeding(rows)),
        transport.clone(),
        None,
    );
    let workflow = Workflow::new_validated(registry, debrief_schema())
        .expect("DEBRIEF declared graph must pass WorkflowValidator::validate");

    let ctx = workflow
        .run(serde_json::json!(campaign_id.to_string()), Box::new(|_| {}))
        .await
        .expect("DEBRIEF must run to completion");

    let recorded = &ctx.nodes[engine_core::workflows::orchestration::debrief::DEBRIEF_NODE_NAME];
    let brief = recorded["brief"].as_str().unwrap().to_string();
    (brief, transport)
}

/// Fixture (a): a clean multi-step campaign — every step named, in
/// `created_at` order.
#[tokio::test]
async fn debrief_fixture_clean_campaign_names_every_step_in_order() {
    let campaign_id = Uuid::new_v4();
    let campaign = campaign_id.to_string();
    let rows = vec![
        debrief_row(
            &campaign,
            "provision",
            engine_contract::JournalDecisionKind::StepIntegrated,
            "ok",
            0,
        ),
        debrief_row(
            &campaign,
            "build",
            engine_contract::JournalDecisionKind::StepIntegrated,
            "ok",
            5,
        ),
        debrief_row(
            &campaign,
            "deploy",
            engine_contract::JournalDecisionKind::StepIntegrated,
            "ok",
            10,
        ),
    ];

    let (brief, transport) = run_debrief_workflow(campaign_id, rows).await;

    let provision_pos = brief.find("provision").expect("provision named");
    let build_pos = brief.find("build").expect("build named");
    let deploy_pos = brief.find("deploy").expect("deploy named");
    assert!(provision_pos < build_pos && build_pos < deploy_pos);
    assert!(
        brief.to_lowercase().contains("clean run"),
        "a fully clean campaign must read as a clean success: {brief}"
    );
    assert_eq!(
        transport.calls().len(),
        1,
        "the rendered digest must be dispatched to CONTENT_PIPELINE exactly once"
    );
}

/// Fixture (b) — THE LOAD-BEARING TEST. Per carryover
/// `gate-scope-must-be-shown-capable-of-failing`: a check whose inputs
/// both come from the artifact under test returns the same green a real
/// check returns, so this test does not merely assert the real renderer
/// names the bail — it separately builds "a renderer that omits the bail"
/// (a hand-stripped brief string with the reason text removed) and shows
/// `brief_names_every_bail` return `false` against it. That demonstrates
/// the check can fail before trusting that it passing on the real
/// renderer means anything.
#[tokio::test]
async fn debrief_fixture_bailed_campaign_names_the_bail_and_the_check_is_shown_capable_of_failing()
{
    let campaign_id = Uuid::new_v4();
    let campaign = campaign_id.to_string();
    let rows = vec![
        debrief_row(
            &campaign,
            "build",
            engine_contract::JournalDecisionKind::StepIntegrated,
            "ok",
            0,
        ),
        debrief_row(
            &campaign,
            "publish",
            engine_contract::JournalDecisionKind::StepBailed,
            "publish target unreachable: connection refused",
            5,
        ),
    ];

    // (1) The real renderer, run end to end through the registered
    // workflow, names the bail and its reason and does NOT read clean.
    let (brief, _transport) = run_debrief_workflow(campaign_id, rows.clone()).await;
    assert!(brief.contains("publish"));
    assert!(brief.contains("publish target unreachable: connection refused"));
    assert!(
        !brief.to_lowercase().contains("clean run"),
        "a bailed campaign must not read as a clean success: {brief}"
    );

    // (2) DEMONSTRATION the check is capable of failing: a renderer that
    // omits the bail's reason text must be rejected by
    // `brief_names_every_bail`, the same function `DebriefNode::process`
    // gates on before writing a row.
    let renderer_that_hides_the_failure =
        "2 step(s) ran:\n- [build] integrated: ok\n- [publish] BAILED".to_string();
    assert!(
        !brief_names_every_bail(&renderer_that_hides_the_failure, &rows),
        "the bail-naming check must be able to return false, not just true"
    );
}

/// Fixture (c): an operator-waiting item. An operator hold that is still
/// open when a campaign is otherwise finished is recorded as a
/// `GateRefused` journal row (mirrors `integrate.rs`'s "operator hold
/// deadline exceeded" write path) — this test asserts that row's reason
/// text survives into the rendered brief, not silently dropped.
#[tokio::test]
async fn debrief_fixture_operator_waiting_item_survives_into_the_brief() {
    let campaign_id = Uuid::new_v4();
    let campaign = campaign_id.to_string();
    let rows = vec![
        debrief_row(
            &campaign,
            "review",
            engine_contract::JournalDecisionKind::StepIntegrated,
            "ok",
            0,
        ),
        debrief_row(
            &campaign,
            "ship",
            engine_contract::JournalDecisionKind::GateRefused,
            "waiting on operator clearance: block still under an operator hold",
            5,
        ),
    ];

    let (brief, _transport) = run_debrief_workflow(campaign_id, rows).await;

    assert!(brief.contains("ship"));
    assert!(brief.contains("waiting on operator clearance: block still under an operator hold"));
}

/// Fixture (d): an empty campaign — zero journal rows — produces a brief
/// saying nothing ran, never an empty or absent response, and a
/// `DebriefRendered` row is still written (AC3).
#[tokio::test]
async fn debrief_fixture_empty_campaign_says_nothing_ran_not_empty_or_absent() {
    let campaign_id = Uuid::new_v4();

    let (brief, transport) = run_debrief_workflow(campaign_id, vec![]).await;

    assert!(!brief.is_empty());
    assert!(brief.to_lowercase().contains("no steps ran"));
    assert_eq!(
        transport.calls().len(),
        1,
        "even an empty campaign dispatches its (non-empty) brief"
    );
}

/// AC6 / the EN.12.F decoupling: a debrief renders for a campaign produced
/// by an ORDINARY explicit-block-list chain (`resolve_explicit_chain` +
/// `integrate_chain_with_dispatch`, no `dispatch`-kind step anywhere) —
/// with no conductor present anywhere in the fixture. The real journal
/// rows that chain produces are fed straight into a `StubJournalReader`
/// and rendered through the registered `DEBRIEF` workflow.
#[tokio::test]
async fn debrief_renders_for_a_campaign_produced_by_an_explicit_block_list_chain_with_no_conductor()
{
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("debrief-no-conductor");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();
    let campaign_id = Uuid::new_v4();
    let (sink, rows) = recording_journal_sink();
    let sink_fn = sink.clone();
    // No dispatch-kind steps and no workflow keys registered — an
    // ordinary two-block chain, exactly the shape produced by a plain
    // `/orchestrate` run with no conductor anywhere.
    let dispatcher = Dispatcher::new();

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);

    integrate_chain_with_dispatch(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        roadmap_dir.as_path(),
        None,
        &|_: &StepProgress| {},
        false,
        campaign_id,
        Some(&move |row| sink_fn(row)),
        &dispatcher,
    )
    .await
    .expect("an ordinary explicit-block-list chain with no conductor must integrate cleanly");

    let campaign_rows: Vec<engine_contract::JournalRow> = rows.lock().unwrap().clone();
    assert!(
        !campaign_rows.is_empty(),
        "the chain must have produced real journal rows to feed the debrief"
    );
    assert!(
        campaign_rows
            .iter()
            .all(|r| !r.step.to_uppercase().contains("CONDUCTOR")),
        "no step in this fixture may resemble a conductor: {campaign_rows:?}"
    );

    let (brief, transport) = run_debrief_workflow(campaign_id, campaign_rows).await;

    assert!(brief.contains("A.1"));
    assert!(brief.contains("B.1"));
    assert!(
        !brief.to_uppercase().contains("CONDUCTOR"),
        "the rendered brief must not reference a conductor: {brief}"
    );
    assert_eq!(transport.calls().len(), 1);
}

// ── EN.12.C task 6: end-to-end coverage of the permission-profile gate ────
//
// Both paths through `gates::check_permission_gate` — deny-and-stop,
// permit-and-proceed — driven against the REAL production functions
// (`policy::permission::resolve_permission_profile`, `gates::check_permission_gate`,
// `integrate::integrate_chain`, `execute::execute_step`), never a re-derivation of
// their logic. `author_operator_edge` stands in for the mev CLI invocation
// (`docs/permission-profiles.md`'s seam) as an in-process recording closure — this
// suite is hermetic per the task's own requirement and writes to no real
// `state.json` and shells out to no real `mev` binary.

/// Path A: a graded action under `locked` raises the operator edge and stops the
/// chain — asserted end to end by composing the real `resolve_permission_profile`
/// (against a fixture `brain.toml` with no `[permission_profiles]` table, which
/// fails closed to `Locked`) with the real `check_permission_gate`, then proving no
/// step ever reached `run_flow`.
///
/// Demonstrated capable of failing: `permission::decide(Locked, InstallOnMini)` is
/// the ONLY thing `check_permission_gate` consults for this verdict. If a future
/// edit to that decision matrix ever permitted `InstallOnMini` under `Locked`, the
/// `match` below would take the `Ok(())` arm, `proceeded` would end up containing
/// `"A.1"`, and `assert!(proceeded.is_empty(), ...)` would fail — this is not a
/// tautology over a mock, both sides of the assertion come from real production
/// code (`resolve_permission_profile` + `check_permission_gate`), not from the
/// test's own fixture data.
#[test]
fn path_a_locked_profile_denies_a_graded_action_and_the_chain_never_proceeds() {
    let (_repos_dir, registry) = two_repo_registry();
    let (profile, _resolution_error) =
        resolve_permission_profile(&registry.brain_root().join("brain.toml"));
    assert_eq!(
        profile,
        PermissionProfile::Locked,
        "the fixture brain.toml declares no [permission_profiles] table, so \
         resolution must fail closed to Locked — if this assertion itself fails, \
         Path A is not actually exercising the tight profile it claims to"
    );

    let chain = resolve_explicit_chain(vec![
        ("repo-a".to_string(), "A.1".to_string()),
        ("repo-b".to_string(), "B.1".to_string()),
    ]);
    let runner = RecordingRunner::new();
    let _flow_runner = runner.clone().into_runner(); // never invoked in this path

    let edge_calls: Arc<Mutex<Vec<OperatorGateRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = edge_calls.clone();
    // Stands in for the mev CLI invocation (`docs/permission-profiles.md`'s seam) —
    // an in-process recording closure, never a real `mev` process.
    let author_operator_edge = move |edge: &OperatorGateRequest| {
        recorder.lock().unwrap().push(edge.clone());
        Ok(())
    };

    // Mirrors what a real caller of `check_permission_gate` does at each chain
    // boundary: gate the graded action before the step is ever dispatched, and
    // stop iterating the moment one denies.
    let mut proceeded = Vec::new();
    for step in &chain {
        match check_permission_gate(
            step,
            GatedAction::InstallOnMini,
            profile,
            &author_operator_edge,
        ) {
            Ok(()) => proceeded.push(step.block_id.clone()),
            Err(PermissionGateError::Denied { .. }) => break,
            Err(other) => panic!("unexpected permission-gate failure: {other}"),
        }
    }

    assert!(
        proceeded.is_empty(),
        "a Locked profile must deny InstallOnMini before the first step even \
         proceeds: {proceeded:?}"
    );
    let recorded = edge_calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "exactly one operator-gate edge must be authored, for the first denied step"
    );
    assert_eq!(recorded[0].slug, "permission-install_on_mini");
    assert_eq!(
        runner.call_count(),
        0,
        "a denied graded action must stop the chain — no block may ever run"
    );
}

/// Path B: the SAME action under `unrestricted` proceeds to completion — the
/// contrast with Path A that proves `InstallOnMini` is graded rather than
/// universally forbidden or universally allowed. Runs the real
/// `check_permission_gate` first (asserting it permits and never authors an edge),
/// then drives the real `integrate_chain` to completion so the chain's own record
/// of the run exists to inspect.
#[tokio::test]
async fn path_b_the_same_action_under_unrestricted_permits_and_the_chain_completes() {
    let (repos_dir, registry) = two_repo_registry_with_profile("unrestricted");
    let (profile, resolution_error) =
        resolve_permission_profile(&registry.brain_root().join("brain.toml"));
    assert_eq!(profile, PermissionProfile::Unrestricted);
    assert_eq!(resolution_error, None);

    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);

    let edge_calls: Arc<Mutex<Vec<OperatorGateRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = edge_calls.clone();
    let author_operator_edge = move |edge: &OperatorGateRequest| {
        recorder.lock().unwrap().push(edge.clone());
        Ok(())
    };

    let gate_result = check_permission_gate(
        &chain[0],
        GatedAction::InstallOnMini,
        profile,
        &author_operator_edge,
    );
    assert!(
        gate_result.is_ok(),
        "unrestricted must permit InstallOnMini — the same action Path A denied \
         under locked: {gate_result:?}"
    );
    assert!(
        edge_calls.lock().unwrap().is_empty(),
        "a permitted action must never author an operator-gate edge"
    );

    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("permission-path-b");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let outcomes = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("the same action under unrestricted must complete the chain");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].repo_path, repos_dir.path().join("repo-a"));
    assert_eq!(
        runner.call_count(),
        1,
        "the chain actually proceeded and ran the block, unlike Path A"
    );

    let lines = lane_log_lines(&roadmap_dir);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["profile"], "unrestricted");
}

/// Path C: the run record written by both `locked`- and `unrestricted`-configured
/// runs carries the resolved profile identifier in force — driven twice through the
/// real `integrate_chain`, once per profile, reading back the actual
/// `lane-log.jsonl` line each run produced (never a hand-constructed record).
///
/// Demonstrated capable of failing: if `integrate.rs`'s `closed`-entry call site
/// ever dropped its `.with_permission_profile(...)` stamp, `lines[0]["profile"]`
/// would be JSON `null` for both iterations and every `assert_eq!` below would fail
/// against the literal `"locked"`/`"unrestricted"` strings.
#[tokio::test]
async fn path_c_the_run_record_written_under_each_profile_carries_that_profiles_identifier() {
    for default in ["locked", "unrestricted"] {
        let (_repos_dir, registry) = two_repo_registry_with_profile(default);
        let (_planning_root, roadmap_dir) =
            fixture_roadmap_dir(&format!("permission-path-c-{default}"));
        let runner = RecordingRunner::new();
        let flow_runner = runner.clone().into_runner();
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
            None,
            None,
            &always_flow,
            &registry,
            &flow_runner,
            &roadmap_dir,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
        )
        .await
        .unwrap_or_else(|err| panic!("chain under profile '{default}' should integrate: {err}"));

        let lines = lane_log_lines(&roadmap_dir);
        assert_eq!(lines.len(), 1, "one closed record for profile '{default}'");
        assert_eq!(
            lines[0]["profile"], default,
            "the closed record written under a '{default}'-configured brain.toml \
             must carry that same profile identifier"
        );
    }
}

/// Path D: a `locked` parent cannot produce an `unrestricted` child — asserted end
/// to end through the real `execute_step` (the same function `integrate_chain`
/// calls internally for every block-kind step), never through
/// `resolve_child_permission_profile` in isolation. Proves both that the call
/// fails with the typed widening error AND that no child ever ran.
///
/// Demonstrated capable of failing: if `execute_step` stopped calling
/// `resolve_child_permission_profile` before building a `FlowInvocation` — or that
/// function stopped comparing ranks and returned `requested` unconditionally — this
/// call would succeed, `runner.call_count()` would be `1` instead of `0`, and both
/// the `expect_err` and the trailing `assert_eq!` would fail.
#[tokio::test]
async fn path_d_a_locked_parent_cannot_produce_an_unrestricted_child() {
    let (_repos_dir, registry) = two_repo_registry();
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let step = ChainStep {
        repo: "repo-a".to_string(),
        block_id: "A.1".to_string(),
        ..Default::default()
    };

    let err = execute_step(
        &step,
        &always_flow,
        &registry,
        &flow_runner,
        false,
        true,
        Uuid::new_v4(),
        None,
        None,
        PermissionProfile::Locked,
        Some(PermissionProfile::Unrestricted),
    )
    .await
    .expect_err("a Locked parent must refuse to produce an Unrestricted child");

    match err {
        ExecuteError::ProfileWidening {
            repo,
            block_id,
            source,
        } => {
            assert_eq!(repo, "repo-a");
            assert_eq!(block_id, "A.1");
            assert_eq!(source.parent, PermissionProfile::Locked);
            assert_eq!(source.requested, PermissionProfile::Unrestricted);
        }
        other => panic!("expected ProfileWidening, got {other:?}"),
    }

    assert_eq!(
        runner.call_count(),
        0,
        "the rejected widening must never invoke the flow runner — no child ran"
    );
}
