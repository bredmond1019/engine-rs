//! Integration suite for `EN.9.F` — coverage unit tests cannot exercise on
//! their own: an on-disk manifest reload picked up mid-run with no rebuild,
//! and semaphore-bounded queueing under a real concurrent burst.
//!
//! Every manifest fixture here is written to a `tempfile::tempdir()` — never
//! into `crates/term-core/src/detect/manifests/`, which is the exact pattern
//! the tracked `diagnostic-intake-state-json-rewritten-by-test-suite` defect
//! covers and which is still open.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use uuid::Uuid;

use engine_core::nodes::terminal::{
    AdmissionControl, AdmissionPolicy, ManifestOrigin, ManifestSource, NoMatchAlarmPolicy,
    NoMatchAlarmTracker,
};
use term_core::detect::{detect, AgentState};

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::resolve_explicit_chain;
use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, wait_for_clearance, HoldSource, IntegrateError, NeverHeld, StepProgress,
};

fn write_manifest(
    dir: &Path,
    name: &str,
    rule_name: &str,
    needle: &str,
    state: &str,
) -> std::path::PathBuf {
    let toml = format!(
        r#"
name = "{rule_name}"

[[rules]]
state = "{state}"
gate = {{ contains = "{needle}" }}
"#
    );
    let path = dir.join(name);
    fs::write(&path, toml).expect("write tempdir manifest fixture");
    path
}

/// Some macOS/CI filesystems round mtime to ~1s resolution — sleep past
/// that before rewriting so the reload path actually observes a change,
/// matching `manifest_source.rs`'s own unit-test precedent
/// (`touch_later`).
fn rewrite_manifest_later(
    dir: &Path,
    name: &str,
    rule_name: &str,
    needle: &str,
    state: &str,
) -> std::path::PathBuf {
    thread::sleep(Duration::from_millis(1100));
    write_manifest(dir, name, rule_name, needle, state)
}

/// An on-disk manifest edit takes effect on the next capture with no
/// rebuild and no process restart.
#[test]
fn on_disk_manifest_edit_is_picked_up_by_the_next_capture_with_no_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir for manifest fixture");
    let path = write_manifest(
        dir.path(),
        "claude.toml",
        "v1-manifest",
        "V1-MARKER",
        "working",
    );

    let source = ManifestSource::new(Some(path.clone()));

    // First capture: screen carries the v1 marker, classifies under the v1
    // manifest's rule.
    let resolved_v1 = source.resolve();
    assert_eq!(resolved_v1.origin, ManifestOrigin::Override(path.clone()));
    let detection_v1 = detect("...V1-MARKER on screen...", &resolved_v1.manifest);
    assert_eq!(detection_v1.state, AgentState::Working);

    // A screen carrying only the (not-yet-existing) v2 marker does not
    // match the v1 manifest.
    let pre_edit_probe = detect("...V2-MARKER on screen...", &resolved_v1.manifest);
    assert_eq!(pre_edit_probe.state, AgentState::Unknown);

    // Edit the manifest on disk mid-"run" — no process restart, no rebuild.
    rewrite_manifest_later(
        dir.path(),
        "claude.toml",
        "v2-manifest",
        "V2-MARKER",
        "idle",
    );

    // Next capture resolves the EDITED manifest and classifies accordingly.
    let resolved_v2 = source.resolve();
    assert_ne!(resolved_v2.digest, resolved_v1.digest);
    let detection_v2 = detect("...V2-MARKER on screen...", &resolved_v2.manifest);
    assert_eq!(detection_v2.state, AgentState::Idle);

    // And the OLD marker no longer matches the new manifest — proof this
    // is a real reload, not a stale cache serving the v1 rule alongside.
    let old_marker_after_edit = detect("...V1-MARKER on screen...", &resolved_v2.manifest);
    assert_eq!(old_marker_after_edit.state, AgentState::Unknown);
}

/// The no-match alarm names the override manifest's digest across a
/// reload, end to end through `ManifestSource` + `NoMatchAlarmTracker`
/// together (task 1 + task 2 wired as a real caller would use them).
#[test]
fn reload_then_no_match_alarm_names_the_currently_active_override_digest() {
    let dir = tempfile::tempdir().expect("tempdir for manifest fixture");
    let path = write_manifest(dir.path(), "claude.toml", "v1", "MATCH-V1", "working");
    let source = ManifestSource::new(Some(path));

    let tracker = NoMatchAlarmTracker::new(NoMatchAlarmPolicy {
        consecutive_unmatched_threshold: 2,
    });

    let resolved = source.resolve();
    let detection = detect("no marker present here", &resolved.manifest);
    assert_eq!(detection.state, AgentState::Unknown);
    assert_eq!(
        tracker.record_state(&resolved.manifest.name, &resolved.digest, detection.state),
        None
    );

    let resolved_again = source.resolve();
    let detection_again = detect("still no marker present", &resolved_again.manifest);
    let alarm = tracker
        .record_state(
            &resolved_again.manifest.name,
            &resolved_again.digest,
            detection_again.state,
        )
        .expect("2nd consecutive unmatched capture should raise");

    assert_eq!(alarm.manifest_digest, resolved_again.digest);
    assert_eq!(alarm.manifest_digest, resolved.digest); // unchanged manifest, same digest
}

/// A burst of terminal-node "runs" beyond the semaphore limit queues:
/// observed concurrency never exceeds the configured limit, and every run
/// eventually completes.
#[tokio::test]
async fn burst_beyond_the_semaphore_limit_queues_and_every_run_completes() {
    const LIMIT: usize = 4;
    const BURST: usize = 40;

    let control = AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: LIMIT as u32,
    });

    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(BURST);
    for _ in 0..BURST {
        let control = control.clone();
        let current = current.clone();
        let peak = peak.clone();
        let completed = completed.clone();
        handles.push(tokio::spawn(async move {
            // Simulates a terminal-node run: acquire admission, do work,
            // release on drop.
            let _permit = control.acquire().await;
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            current.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("every queued terminal-node run should complete");
    }

    assert!(
        peak.load(Ordering::SeqCst) <= LIMIT,
        "observed concurrency {} exceeded the configured limit {}",
        peak.load(Ordering::SeqCst),
        LIMIT
    );
    assert_eq!(completed.load(Ordering::SeqCst), BURST);
    assert_eq!(control.available_permits(), LIMIT);
}

/// Runs beyond the limit specifically QUEUE rather than fail: a run
/// submitted while the control is saturated is still pending (not yet
/// admitted, not errored) until an in-flight run releases its permit.
#[tokio::test]
async fn a_run_submitted_while_saturated_queues_instead_of_failing() {
    let control = AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: 1,
    });

    let first = control.acquire().await;
    assert_eq!(control.available_permits(), 0);

    let control2 = control.clone();
    let queued = tokio::spawn(async move { control2.acquire().await });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !queued.is_finished(),
        "a run beyond the limit must still be queued, not admitted"
    );

    drop(first);

    let permit = tokio::time::timeout(Duration::from_millis(500), queued)
        .await
        .expect("queued run should be admitted promptly once a permit frees up")
        .expect("queued task should join without panicking");
    assert_eq!(control.available_permits(), 0);
    drop(permit);
    assert_eq!(control.available_permits(), 1);
}

// ── EN.11.L Task 4: the starvation case, end to end ─────────────────────
//
// `AdmissionControl`'s own uncontended-burst coverage above says nothing
// about a step PARKED on an operator hold — seams.md names admission
// under contention as genuinely unspiked. These drive the real
// `integrate_chain` / `wait_for_clearance` path through a tempdir two-repo
// registry (the same fixture shape `tests/it/orchestration.rs` already
// uses), never a reimplementation of the production code.

fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    fs::create_dir_all(dir.path().join("repo-b")).unwrap();
    fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\
         [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

fn write_done_state(repo_path: &Path, block_id: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("sdlc-flow-state.json"),
        serde_json::json!({"status": "done"}).to_string(),
    )
    .unwrap();
}

/// A tempdir roadmap dir under `planning/roadmaps/<slug>/` — never a real,
/// tracked roadmap's `lane-log.jsonl`.
fn fixture_roadmap_dir(slug: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let planning_root = tempfile::tempdir().expect("tempdir");
    let roadmap_dir = planning_root.path().join("roadmaps").join(slug);
    fs::create_dir_all(&roadmap_dir).unwrap();
    (planning_root, roadmap_dir)
}

/// A minimal [`FlowRunner`] double: on every invocation, writes a
/// `"status": "done"` state file into the invocation's own `repo_path` so
/// `integrate_chain`'s state-write verification passes.
fn done_runner() -> FlowRunner {
    Arc::new(move |invocation| {
        Box::pin(async move {
            write_done_state(&invocation.repo_path, &invocation.block_id);
            Ok(engine_contract::TaskContext {
                event: serde_json::json!({}),
                nodes: std::collections::HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: std::collections::HashMap::new(),
            })
        })
    })
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

/// A [`HoldSource`] whose held-ness is flipped by an `AtomicBool` the test
/// controls directly.
struct FlagHold {
    held: Arc<AtomicBool>,
}

impl HoldSource for FlagHold {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        self.held.load(Ordering::SeqCst)
    }
}

/// Held under every check, forever — used for the deadline case, where
/// nobody ever clears it.
struct AlwaysHeld;

impl HoldSource for AlwaysHeld {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        true
    }
}

/// A [`HoldSource`] whose [`HoldSource::notified`] is backed by a real
/// `tokio::sync::Notify`, mirroring the production wiring this trait's
/// doc comment describes (`EN.9.G`'s Blocked-edge bridge is expected to
/// hold one `Notify` per watched hold and call `notify_waiters()` the
/// moment its own poll observes clearance).
struct NotifyOnClear {
    held: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl HoldSource for NotifyOnClear {
    fn is_held(&self, _repo: &str, _block_id: &str) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    fn notified(&self, _repo: &str, _block_id: &str) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(self.notify.notified())
    }
}

/// EN.11.L Task 1's acceptance criterion, driven end to end through the
/// public `integrate_chain` API rather than the module's own private unit
/// test (`integrate.rs`'s `a_held_step_does_not_starve_a_second_lane_of_
/// its_admission_permit`, which exercises the same claim from inside the
/// module — this is the black-box counterpart).
///
/// A step parked on an operator hold must consume no admission permit at
/// all: with a capacity-1 gate, chain A parks on a hold forever while
/// chain B — sharing the same gate — must still be able to acquire the
/// single permit and finish.
///
/// `integrate_chain`'s returned future is deliberately not `Send` (see
/// `execute.rs`'s doc comment on `FlowFuture`), so chain A runs on a
/// `LocalSet` task instead of `tokio::spawn` — exactly the pattern
/// `tests/it/orchestration.rs`'s `admission_at_capacity_waits_rather_than_
/// proceeding_or_failing` already uses for the same reason.
///
/// PROOF THIS TEST CAN FAIL (Task 4 acceptance criterion): reverting the
/// EN.11.L Task 1 fix — moving `let _permit = admission.acquire_for(step)
/// .await;` back to BEFORE the `wait_for_clearance` call in
/// `integrate.rs`'s `integrate_chain` — was applied to the working tree,
/// this test run, and the ordering restored (`git status --porcelain
/// crates/engine-core/src/` empty at task end, per this task's own
/// acceptance criterion). Observed failure, deterministic:
///
///   thread 'terminal_admission::a_held_step_consumes_no_permit_while_a_
///   second_lane_proceeds_at_the_ceiling' panicked at
///   crates/engine-core/tests/it/terminal_admission.rs:431:5:
///   assertion `left == right` failed: a step parked on a hold must not
///   consume an admission permit
///     left: 0
///    right: 1
///
/// i.e. chain A grabs the single permit before ever awaiting the hold, so
/// by the time this assertion runs (60ms in, chain A still parked)
/// `available_permits()` reads 0 instead of the expected 1 — exactly the
/// starvation this block exists to fix: the permit is gone for the whole
/// hold, not just the execution window.
#[tokio::test]
async fn a_held_step_consumes_no_permit_while_a_second_lane_proceeds_at_the_ceiling() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(
            a_held_step_consumes_no_permit_while_a_second_lane_proceeds_at_the_ceiling_inner(),
        )
        .await;
}

async fn a_held_step_consumes_no_permit_while_a_second_lane_proceeds_at_the_ceiling_inner() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root_a, roadmap_dir_a) = fixture_roadmap_dir("lane-a");
    let (_planning_root_b, roadmap_dir_b) = fixture_roadmap_dir("lane-b");

    let admission = AdmissionGate::new(AdmissionControl::new(AdmissionPolicy {
        max_concurrent_terminal_runs: 1,
    }));

    let held = Arc::new(AtomicBool::new(true));
    let hold = FlagHold { held: held.clone() };

    let chain_a = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    let chain_b = resolve_explicit_chain(vec![("repo-b".to_string(), "B.1".to_string())]);

    let registry_path = registry.brain_root().to_path_buf();
    let admission_a = admission.clone();
    let chain_a_task = tokio::task::spawn_local(async move {
        let registry = RepoRegistry::from_brain_root(&registry_path).unwrap();
        let runner = done_runner();
        integrate_chain(
            &chain_a,
            &no_deps,
            &always_met,
            &admission_a,
            &hold,
            Duration::from_millis(5),
            None,
            None,
            None,
            &always_flow,
            &registry,
            &runner,
            &roadmap_dir_a,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
        )
        .await
    });

    // Give chain A every chance to (wrongly) grab the only permit before
    // it reaches its held step.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        admission.available_permits(),
        1,
        "a step parked on a hold must not consume an admission permit"
    );
    assert!(
        !chain_a_task.is_finished(),
        "chain A must still be parked on its hold"
    );

    let runner_b = done_runner();
    let outcomes_b = tokio::time::timeout(
        Duration::from_millis(500),
        integrate_chain(
            &chain_b,
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
            &runner_b,
            &roadmap_dir_b,
            None,
            &|_: &StepProgress| {},
            false,
            Uuid::new_v4(),
        ),
    )
    .await
    .expect("a second lane must proceed while the first is parked on a hold, not hang")
    .expect("chain B should complete");
    assert_eq!(outcomes_b.len(), 1);
    assert_eq!(outcomes_b[0].block_id, "B.1");

    // Release chain A so it can finish and the test can join cleanly.
    held.store(false, Ordering::SeqCst);
    let outcomes_a = tokio::time::timeout(Duration::from_millis(500), chain_a_task)
        .await
        .expect("chain A should finish promptly once its hold clears")
        .expect("chain A task should join without panicking")
        .expect("chain A should complete once the hold clears");
    assert_eq!(outcomes_a.len(), 1);
    assert_eq!(outcomes_a[0].block_id, "A.1");
}

/// EN.11.L Task 2's deadline acceptance criterion: an unanswered hold
/// exceeding its deadline fails the chain loudly, naming the held block
/// and repo, rather than parking forever.
#[tokio::test]
async fn an_unanswered_hold_exceeding_its_deadline_fails_loudly_and_names_the_block() {
    let (_repos_dir, registry) = two_repo_registry();
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("lane-deadline");
    let admission = AdmissionGate::with_default_policy();
    let starting_permits = admission.available_permits();
    let chain = resolve_explicit_chain(vec![("repo-a".to_string(), "A.1".to_string())]);
    let runner = done_runner();

    let err = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &AlwaysHeld,
        Duration::from_millis(5),
        Some(Duration::from_millis(30)),
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
    .expect_err("an unanswered hold must exceed its deadline and fail loudly");

    match &err {
        IntegrateError::HoldDeadlineExceeded { repo, block_id, .. } => {
            assert_eq!(repo, "repo-a");
            assert_eq!(block_id, "A.1");
        }
        other => panic!("expected HoldDeadlineExceeded, got {other:?}"),
    }
    let message = err.to_string();
    assert!(
        message.contains("A.1") && message.contains("repo-a"),
        "the deadline error must name the held block and repo, got: {message}"
    );

    // A deadline-exceeded hold must never have consumed the admission
    // permit either — it fails before ever reaching `acquire_for`.
    assert_eq!(admission.available_permits(), starting_permits);
}

/// EN.11.L Task 2's notification acceptance criterion: a hold that clears
/// wakes `wait_for_clearance` via the notification path immediately,
/// rather than relying purely on the next poll tick. `poll_interval` is
/// set deliberately large (5s) so a purely poll-driven wait would still
/// be asleep well past this test's own timeout — only a real notification
/// wake can make it return in time.
#[tokio::test]
async fn a_cleared_hold_wakes_the_wait_via_notification_not_only_the_next_poll_tick() {
    let held = Arc::new(AtomicBool::new(true));
    let notify = Arc::new(tokio::sync::Notify::new());
    let hold = NotifyOnClear {
        held: held.clone(),
        notify: notify.clone(),
    };

    let waiter = tokio::spawn(async move {
        wait_for_clearance(&hold, "repo-a", "A.1", Duration::from_secs(5), None, None).await
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiter.is_finished(), "must still be parked on the hold");

    held.store(false, Ordering::SeqCst);
    notify.notify_waiters();

    let result = tokio::time::timeout(Duration::from_millis(300), waiter)
        .await
        .expect(
            "a cleared hold must wake the wait via notification, not wait out the 5s poll interval",
        )
        .expect("waiter task should join without panicking");
    result.expect("wait_for_clearance should return Ok once the hold clears");
}
