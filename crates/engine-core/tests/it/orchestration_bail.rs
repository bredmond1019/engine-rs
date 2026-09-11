//! `EN.17.B` task 6 — black-box integration coverage for the whole block:
//! a bail skips only its dependents, every block boundary re-reads the
//! graph, and the terminal `chain_report` always accounts for every step.
//!
//! Driven entirely through `engine_core`'s PUBLIC surface
//! (`integrate_chain_with_coord_and_policy` / `integrate_chain_with_coord`),
//! mirroring the discipline `coord_chain.rs` and `orchestration_chain.rs`
//! already use: fixture helpers are duplicated here rather than exported
//! across the crate boundary, and every fixture is a real
//! `tempfile::tempdir()` — real `brain.toml` + `planning/state.json` files,
//! never a mock.
//!
//! Tasks 1-4's own `#[cfg(test)] mod tests` inside `integrate.rs` and
//! `corpus_gates.rs` already cover the same mechanics at the unit level
//! (`skip_dependents_runs_the_independent_step_after_a_bail`,
//! `status_aware_boundary_skips_a_closed_block_unconditionally`,
//! `block_status_reports_each_terminal_status_and_the_open_control`, etc.)
//! with stub `block_status` closures. This module's own contribution is
//! driving the SAME entry point from outside the crate, and — where the
//! task calls for it — through a REAL `corpus_gates::CorpusGates` reading a
//! REAL temp `state.json`, rather than a hand-rolled stub, so the full
//! read path is exercised at least once.
//!
//! ## OBSERVED RED (task 6's own acceptance criterion)
//!
//! `orchestration_bail_independent_step_runs_after_a_bail` and
//! `orchestration_bail_status_is_reread_at_the_boundary` were confirmed to
//! fail against the pre-task-4 loop: the status-aware boundary block
//! (`integrate.rs`, the `status_skip_reason` match) was short-circuited to
//! always resolve `None` (bypassing `block_status` entirely) and the
//! skip-dependents block (the `blocking_chain_edge` computation) was
//! short-circuited to always resolve `None` too, both in the working tree.
//! `cargo nextest run -p engine-core
//! orchestration_bail_independent_step_runs_after_a_bail
//! orchestration_bail_status_is_reread_at_the_boundary` was run; both
//! panicked:
//!
//! - `orchestration_bail_status_is_reread_at_the_boundary` panicked with
//!   `StateWriteUnreadable { repo: "repo-a", block_id: "B.1", path:
//!   ".../repo-a/planning/B.1/sdlc/sdlc-flow-state.json", source: Os {
//!   code: 2, kind: NotFound, ... } }` — with the status-aware boundary
//!   gone, `B.1` was never skipped for its (step-observer-flipped) `closed`
//!   status; it fell through to dispatch, and the stub runner (which never
//!   writes a done-state file for `B.1`) left `verify_state_write` unable
//!   to find one.
//! - `orchestration_bail_independent_step_runs_after_a_bail` panicked on
//!   `assert_eq!(report.bailed, vec!["repo-a:A.1".to_string()])` with
//!   `left: ["repo-a:A.1", "repo-a:B.1"], right: ["repo-a:A.1"]` — with the
//!   skip-dependents block gone, `B.1`'s `block` edge to the bailed `A.1`
//!   was never recognized as blocking; this fixture's `is_edge_met` always
//!   answers `true`, so `check_dependencies` never objected either, and
//!   `B.1` fell through to dispatch. The stub runner does not fail `B.1`
//!   (only `A.1`), but this fixture never pre-writes `B.1`'s own done-state
//!   file (only `C.1`'s, the one step meant to close), so
//!   `verify_state_write` then failed to find one and recorded `B.1` itself
//!   as a SECOND bailed step — proving the pre-task-4 loop truly has no
//!   notion of "skip a dependent," not merely a cosmetic difference in this
//!   fixture.
//!
//! The perturbation was then reverted with `git checkout --
//! crates/engine-core/src/workflows/orchestration/integrate.rs` before this
//! task's commit. `git status --porcelain` scoped to
//! `crates/engine-core/src/` was empty afterward and remains empty in this
//! commit; no production source differs from HEAD.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::corpus_gates::{BlockPresence, CorpusGates};
use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::graph::OnBail;
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_coord, integrate_chain_with_coord_and_policy, ChainReport, NeverHeld,
    StepProgress,
};
use engine_core::WorkflowError;

// ── Shared fixture helpers — mirrors `orchestration::integrate`'s own
//    `mod tests` helpers (`two_repo_registry`, `recording_runner`,
//    `write_done_state`, `step`), duplicated here rather than exported
//    across the crate boundary; see `coord_chain.rs`'s own identical note.

fn step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        directives: None,
        ..Default::default()
    }
}

fn two_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::create_dir_all(dir.path().join("repo-b")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n\n\
         [[repos]]\nslug = \"repo-b\"\nrepo_path = \"repo-b\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

fn write_done_state(repo_path: &Path, block_id: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        json!({"status": "done"}).to_string(),
    )
    .unwrap();
}

/// A [`FlowRunner`] that records every dispatched block id (even the one
/// that fails) and fails exactly `fail_block`, succeeding for everything
/// else — the success side never writes a state file itself; callers must
/// pre-write it via [`write_done_state`] for any step expected to close,
/// matching `integrate.rs`'s own `mod tests` convention.
fn failing_and_recording_runner(fail_block: &'static str) -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation| {
        recorded.lock().unwrap().push(invocation.block_id.clone());
        Box::pin(async move {
            if invocation.block_id == fail_block {
                Err(WorkflowError::new(format!(
                    "simulated failure for {}",
                    invocation.block_id
                )))
            } else {
                Ok(engine_contract::TaskContext {
                    event: json!({}),
                    nodes: HashMap::new(),
                    metadata: json!({}),
                    node_runs: HashMap::new(),
                })
            }
        })
    });
    (runner, calls)
}

fn recording_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
    failing_and_recording_runner("")
}

/// A one-repo (`repo-a`) real brain root whose `planning/state.json` carries
/// exactly the blocks embedded in `blocks_json` (a raw, comma-separated list
/// of block objects, same convention `corpus_gates.rs`'s own `state_json`
/// test helper uses) — for [`CorpusGates::block_status`] to read for real.
fn brain_root_with_state(blocks_json: &str) -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    let planning = dir.path().join("repo-a").join("planning");
    std::fs::create_dir_all(&planning).unwrap();
    std::fs::write(
        planning.join("state.json"),
        format!(
            r#"{{
    "repo": "repo",
    "kind": "project",
    "updated": "2026-08-18",
    "tracks": [
        {{ "title": "wave 1", "blocks": [ {blocks_json} ] }}
    ]
}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

/// The full, unabbreviated parameter list `integrate_chain_with_coord_and_policy`
/// takes for everything this module leaves at its permissive/no-op default:
/// no campaign budget, no cancellation token, no coord handle, no forwarded
/// child policy, `is_met` always true unless a case overrides it, worktree +
/// auto-PR both on (irrelevant — the stub runner never shells out to git).
#[allow(clippy::too_many_arguments)]
async fn run_chain(
    chain: &[ChainStep],
    resolve_deps: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    registry: &RepoRegistry,
    run_flow: &FlowRunner,
    roadmap_dir: &Path,
    on_bail: OnBail,
    block_status: &dyn Fn(&str, &str) -> BlockPresence,
    report: &mut ChainReport,
    step_observer: &(dyn Fn(&StepProgress) + Send + Sync + 'static),
) -> Result<
    Vec<engine_core::workflows::orchestration::execute::ExecutionOutcome>,
    engine_core::workflows::orchestration::integrate::IntegrateError,
> {
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let is_met = |_repo: &str, _id: &str| true;
    let admission = AdmissionGate::with_default_policy();
    integrate_chain_with_coord_and_policy(
        chain,
        resolve_deps,
        &is_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &resolve_engine,
        registry,
        run_flow,
        roadmap_dir,
        None,
        step_observer,
        false,
        true,
        uuid::Uuid::new_v4(),
        &|_repo: &str, _id: &str| {},
        None,
        None,
        None,
        on_bail,
        block_status,
        report,
    )
    .await
}

fn open_status(_repo: &str, _block_id: &str) -> BlockPresence {
    BlockPresence::Row("open".to_string())
}

// ── (1) An independent step still runs after a bail ─────────────────────

#[tokio::test]
async fn orchestration_bail_independent_step_runs_after_a_bail() {
    let (dir, registry) = two_repo_registry();
    write_done_state(&dir.path().join("repo-b"), "C.1");
    let (runner, _calls) = failing_and_recording_runner("A.1");
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        if repo == "repo-a" && id == "B.1" {
            vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
            }]
        } else {
            Vec::new()
        }
    };
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "B.1"),
        step("repo-b", "C.1"),
    ];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &open_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("SkipDependents keeps the chain going past a bail");

    assert_eq!(outcomes.len(), 1, "only the independent step C.1 ran");
    assert_eq!(outcomes[0].block_id, "C.1");
    assert_eq!(report.bailed, vec!["repo-a:A.1".to_string()]);
    assert_eq!(report.closed, vec!["repo-b:C.1".to_string()]);
}

// ── (2) A dependent is skipped, with a block pointer at the bailed step ──

#[tokio::test]
async fn orchestration_bail_dependent_is_skipped_with_block_pointer() {
    let (_dir, registry) = two_repo_registry();
    let (runner, _calls) = failing_and_recording_runner("A.1");
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        if repo == "repo-a" && id == "B.1" {
            vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
            }]
        } else {
            Vec::new()
        }
    };
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![step("repo-a", "A.1"), step("repo-a", "B.1")];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &open_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("SkipDependents completes with nothing left to run");

    assert!(outcomes.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].block, "repo-a:B.1");
    assert_eq!(
        report.skipped[0].blocked_by,
        Some(json!({"type": "block", "repo": "repo-a", "id": "A.1"})),
        "the pointer must name the bailed step's own edge"
    );
}

// ── (3) The skip is transitive through a chain of `block` edges ─────────

#[tokio::test]
async fn orchestration_bail_skip_is_transitive() {
    let (dir, registry) = two_repo_registry();
    write_done_state(&dir.path().join("repo-b"), "D.1");
    let (runner, _calls) = failing_and_recording_runner("A.1");
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        match (repo, id) {
            ("repo-a", "B.1") => vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
            }],
            ("repo-a", "C.1") => vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "B.1".to_string(),
            }],
            _ => Vec::new(),
        }
    };
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "B.1"),
        step("repo-a", "C.1"),
        step("repo-b", "D.1"),
    ];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &open_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("independent D.1 still runs");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].block_id, "D.1");
    assert_eq!(report.bailed, vec!["repo-a:A.1".to_string()]);
    let skipped_blocks: Vec<&str> = report.skipped.iter().map(|s| s.block.as_str()).collect();
    assert_eq!(
        skipped_blocks,
        vec!["repo-a:B.1", "repo-a:C.1"],
        "B.1 skips directly on A.1; C.1 skips transitively through B.1"
    );
    assert_eq!(
        report.skipped[1].blocked_by,
        Some(json!({"type": "block", "repo": "repo-a", "id": "B.1"})),
        "C.1's pointer names B.1 — the step immediately blocking it — not A.1"
    );
}

// ── (4) Every terminal status is never started; `open` is the control ───

#[tokio::test]
async fn orchestration_bail_each_terminal_status_is_never_started() {
    let (dir, registry) = brain_root_with_state(
        r#"{"id": "A.1", "title": "a1", "status": "closed"},
           {"id": "A.2", "title": "a2", "status": "wontfix"},
           {"id": "A.3", "title": "a3", "status": "superseded"},
           {"id": "A.4", "title": "a4", "status": "deferred"},
           {"id": "A.5", "title": "a5", "status": "open"}"#,
    );
    write_done_state(&dir.path().join("repo-a"), "A.5");
    let gates = CorpusGates::new(Arc::new(registry.clone()));
    let block_status = |repo: &str, block_id: &str| gates.block_status(repo, block_id);
    let (runner, calls) = recording_runner();
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "A.2"),
        step("repo-a", "A.3"),
        step("repo-a", "A.4"),
        step("repo-a", "A.5"),
    ];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::StopChain,
        &block_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("terminal-status blocks are skipped, never a chain failure");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].block_id, "A.5",
        "the open control still dispatches"
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &["A.5".to_string()],
        "A.1-A.4 must never have reached the runner"
    );
    assert_eq!(report.skipped.len(), 4);
    for (skipped, expected_status) in
        report
            .skipped
            .iter()
            .zip(["closed", "wontfix", "superseded", "deferred"])
    {
        assert!(
            skipped.reason.contains(expected_status),
            "expected reason to name '{expected_status}': {}",
            skipped.reason
        );
        assert!(skipped.blocked_by.is_none());
    }
}

// ── (5) Status is re-read at the boundary, not at chain resolution ──────

#[tokio::test]
async fn orchestration_bail_status_is_reread_at_the_boundary() {
    let (dir, registry) = two_repo_registry();
    write_done_state(&dir.path().join("repo-a"), "A.1");
    let (runner, _calls) = recording_runner();
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let roadmap_dir = tempfile::tempdir().unwrap();
    // At the moment the chain is BUILT, B.1 is "open" — if the boundary
    // check baked its decision in up front, B.1 would dispatch. Instead the
    // step observer flips it to "closed" the instant A.1 finishes, and the
    // boundary check for B.1 must see THAT value, not the one that was true
    // when the chain started.
    let b1_status: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("open"));
    let flip_on_a1 = {
        let b1_status = b1_status.clone();
        move |progress: &StepProgress| {
            if progress.block_id == "A.1" {
                *b1_status.lock().unwrap() = "closed";
            }
        }
    };
    let block_status = {
        let b1_status = b1_status.clone();
        move |_repo: &str, block_id: &str| -> BlockPresence {
            if block_id == "B.1" {
                BlockPresence::Row((*b1_status.lock().unwrap()).to_string())
            } else {
                BlockPresence::Row("open".to_string())
            }
        }
    };
    let chain = vec![step("repo-a", "A.1"), step("repo-a", "B.1")];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::StopChain,
        &block_status,
        &mut report,
        &flip_on_a1,
    )
    .await
    .expect("a status-skipped step is not a chain failure");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].block_id, "A.1");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].block, "repo-a:B.1");
    assert!(
        report.skipped[0].reason.contains("closed"),
        "B.1 must have been skipped for the status observed AT its own \
         boundary (closed), not the one true when the chain was built (open): {}",
        report.skipped[0].reason
    );
}

// ── (6) A block demoted to backlog[] (absent from tracks[].blocks[]) ────

#[tokio::test]
async fn orchestration_bail_parked_block_is_skipped() {
    let (dir, registry) =
        brain_root_with_state(r#"{"id": "A.5", "title": "a5", "status": "open"}"#);
    write_done_state(&dir.path().join("repo-a"), "A.5");
    let gates = CorpusGates::new(Arc::new(registry.clone()));
    let block_status = |repo: &str, block_id: &str| gates.block_status(repo, block_id);
    let (runner, _calls) = recording_runner();
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![step("repo-a", "GHOST"), step("repo-a", "A.5")];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::StopChain,
        &block_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("a parked block is skipped, not a chain failure");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].block_id, "A.5");
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].block, "repo-a:GHOST");
    assert!(report.skipped[0].reason.contains("not in tracks"));
}

// ── (7) An unmet edge OUTSIDE the chain skips only its own step ─────────

#[tokio::test]
async fn orchestration_bail_unmet_external_edge_skips_only_its_step() {
    let (dir, registry) = two_repo_registry();
    write_done_state(&dir.path().join("repo-b"), "C.1");
    let (runner, calls) = recording_runner();
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        if repo == "repo-a" && id == "B.1" {
            vec![DependencyEdge::External {
                what: "waiting on the ops mirror".to_string(),
            }]
        } else {
            Vec::new()
        }
    };
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![step("repo-a", "B.1"), step("repo-b", "C.1")];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &open_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("SkipDependents keeps going past an unmet edge outside the chain");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].block_id, "C.1");
    assert!(
        !calls.lock().unwrap().contains(&"B.1".to_string()),
        "B.1 must never have reached the runner"
    );
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].block, "repo-a:B.1");
    assert_eq!(
        report.skipped[0].blocked_by,
        Some(json!({"type": "external", "what": "waiting on the ops mirror"}))
    );
    assert_eq!(report.closed, vec!["repo-b:C.1".to_string()]);
}

// ── (8) `state.json` is a pure read, never mutated by a chain run ───────

#[tokio::test]
async fn orchestration_bail_state_json_is_untouched() {
    let (dir, registry) = brain_root_with_state(
        r#"{"id": "A.1", "title": "a1", "status": "closed"},
           {"id": "A.5", "title": "a5", "status": "open"}"#,
    );
    write_done_state(&dir.path().join("repo-a"), "A.5");
    let state_path = dir
        .path()
        .join("repo-a")
        .join("planning")
        .join("state.json");
    let before = std::fs::read(&state_path).expect("read before");

    let gates = CorpusGates::new(Arc::new(registry.clone()));
    let block_status = |repo: &str, block_id: &str| gates.block_status(repo, block_id);
    let (runner, _calls) = recording_runner();
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.5")];
    let mut report = ChainReport::default();

    run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &block_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("chain completes");

    let after = std::fs::read(&state_path).expect("read after");
    assert_eq!(before, after, "a chain run must never mutate state.json");
}

// ── (9) `closed + bailed + skipped` always sums to the chain length ─────

#[tokio::test]
async fn orchestration_bail_chain_report_sums_to_chain_length() {
    let (dir, registry) = two_repo_registry();
    write_done_state(&dir.path().join("repo-b"), "E.1");
    let (runner, _calls) = failing_and_recording_runner("A.1");
    let resolve_deps = |repo: &str, id: &str| -> Vec<DependencyEdge> {
        match (repo, id) {
            ("repo-a", "B.1") => vec![DependencyEdge::Block {
                repo: "repo-a".to_string(),
                block_id: "A.1".to_string(),
            }],
            ("repo-a", "D.1") => vec![DependencyEdge::External {
                what: "unmet outside-chain fact".to_string(),
            }],
            _ => Vec::new(),
        }
    };
    let block_status = |_repo: &str, block_id: &str| -> BlockPresence {
        if block_id == "C.1" {
            BlockPresence::Row("closed".to_string())
        } else {
            BlockPresence::Row("open".to_string())
        }
    };
    let roadmap_dir = tempfile::tempdir().unwrap();
    // A.1 bails; B.1 skips (dependent); C.1 skips (status boundary); D.1
    // skips (unmet external edge); E.1 is independent and runs.
    let chain = vec![
        step("repo-a", "A.1"),
        step("repo-a", "B.1"),
        step("repo-a", "C.1"),
        step("repo-a", "D.1"),
        step("repo-b", "E.1"),
    ];
    let mut report = ChainReport::default();

    let outcomes = run_chain(
        &chain,
        &resolve_deps,
        &registry,
        &runner,
        roadmap_dir.path(),
        OnBail::SkipDependents,
        &block_status,
        &mut report,
        &|_: &StepProgress| {},
    )
    .await
    .expect("SkipDependents runs the whole chain to completion");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        report.closed.len() + report.bailed.len() + report.skipped.len(),
        chain.len(),
        "closed({}) + bailed({}) + skipped({}) must sum to the chain's own \
         step count ({}) under SkipDependents: {report:?}",
        report.closed.len(),
        report.bailed.len(),
        report.skipped.len(),
        chain.len(),
    );
}

// ── (10) The built-in `StopChain` default is unchanged by this block ────

#[tokio::test]
async fn orchestration_bail_stop_chain_default_is_unchanged() {
    let (_dir, registry) = two_repo_registry();
    let (runner, calls) = failing_and_recording_runner("A.1");
    let resolve_deps = |_repo: &str, _id: &str| Vec::new();
    let is_met = |_repo: &str, _id: &str| true;
    let admission = AdmissionGate::with_default_policy();
    let roadmap_dir = tempfile::tempdir().unwrap();
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let chain = vec![step("repo-a", "A.1"), step("repo-b", "B.1")];

    // `integrate_chain_with_coord` — the pre-`EN.17.B` public entry point,
    // never touched by task 4 — always resolves `OnBail::StopChain`
    // internally; there is no way to pass anything else through it.
    let err = integrate_chain_with_coord(
        &chain,
        &resolve_deps,
        &is_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(1),
        None,
        None,
        None,
        &resolve_engine,
        &registry,
        &runner,
        roadmap_dir.path(),
        None,
        &|_: &StepProgress| {},
        false,
        true,
        uuid::Uuid::new_v4(),
        &|_repo: &str, _id: &str| {},
        None,
        None,
        None,
    )
    .await
    .expect_err("the first bail still ends the whole chain by default");

    assert!(err.to_string().contains("A.1"));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &["A.1".to_string()],
        "B.1 must never have been dispatched under the built-in default"
    );
}
