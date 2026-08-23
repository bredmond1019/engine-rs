//! `EN.11.B` — the smoke fixture, checked in.
//!
//! The smoke run (`smoke-run.md`, 2026-08-19) was the spike that first drove the
//! ORCHESTRATION workflow's `resolve_explicit_chain` / `integrate_chain` over a real,
//! two-block chain built on real temp git repos. Its harness was deleted after the run;
//! this module makes the fixture permanent so the four behaviours it observed have
//! regression coverage, and so later blocks that flip a behaviour have a fixture to flip
//! it against instead of re-inventing one.
//!
//! Every case is asserted at its **current, as-of-2026-08-21** value, never at what the
//! behaviour "should" eventually be — see the block record's amendment note. One of the
//! four cases is still wrong today and is asserted wrong, carrying an inline comment
//! naming the block that flips it (never asserted BEFORE that block lands: this
//! is `EN.11.B` task 1, scaffolding only — cases live in later tasks of this same spec):
//!
//! - (a) COMPOSITION (smoke-run.md §3.4) — still WRONG. Flipped by `EN.11.C`.
//! - (b) FAILED STEP INVISIBLE (smoke-run.md §3.2) — now FIXED, by `EN.11.D`.
//! - (c) RED AUTHORITATIVE BUILD (smoke-run.md §3.3) — now FIXED, by
//!   `EN.ticket.final-validation-failure-must-block` (closed 2026-08-20).
//! - (d) LANE-LOG SHAPE (smoke-run.md §3.7) — now FIXED, by
//!   `EN.ticket.lane-log-entry-schema` (closed 2026-08-20).
//!
//! # Task 1 — fixture scaffolding
//!
//! This task builds the reusable pieces later tasks assert against:
//! - [`real_git_repo_with_bare_origin`] / [`single_repo_fixture`] — a REAL git repo (not a
//!   bare directory stand-in) with a real bare `origin`, so cutting `sdlc/<block>` from
//!   `origin/main` is a real git operation, exactly matching smoke-run.md §1's recipe.
//! - [`RecordingRunner`] — a [`FlowRunner`] double that mimics `SDLC_FLOW`'s branch
//!   discipline on the happy path: `git checkout -B sdlc/<block> origin/main`, a marker
//!   commit, a push, then (unless overridden) a `"status": "done"` state-file write at
//!   exactly the path `wrap_up.rs::state_path_for` looks for.
//!
//! Everything here is written under a `tempfile::TempDir`. Nothing is ever written under
//! this repo's own `planning/` tree — see the `diagnostic-intake-state-json-rewritten-by-
//! test-suite` carryover for the failure shape this fixture must not repeat.
//!
//! # Task 4 — the gate-scope guard: each case shown capable of failing
//!
//! The `gate-scope-must-be-shown-capable-of-failing` carryover names the anti-pattern
//! this fixture is most exposed to: a check whose inputs both come from the artifact
//! under test, and which therefore cannot fail. For each of the four cases below, the
//! named production symbol was perturbed by hand in the working tree, the case was run
//! and observed to fail with the message shown, and the perturbation was then reverted
//! (`git checkout --`) before this task's commit — `git status --porcelain` scoped to
//! `crates/engine-core/src/` was empty at that point and remains empty in this commit.
//! No production source differs from HEAD.
//!
//! - **(a)** [`block_n_plus_1s_tree_lacks_block_ns_work_today`] — perturbed
//!   `integrate_chain`'s per-step loop (`integrate.rs`) to push each step's completed
//!   branch tip to `origin/main` and fetch it back (simulating the chaining `EN.11.C`
//!   will add), immediately after `verify_state_write` succeeds. Observed failure:
//!   `panicked at .../orchestration_chain.rs:456:5: WRONG-TODAY: B2's tree must NOT
//!   contain B1's marker, because B2's branch was cut from origin/main, not from B1's
//!   tip — flips by EN.11.C`.
//! - **(b)** [`a_failed_setup_worktree_step_stops_the_chain_via_execute_step`] —
//!   `EN.11.D` Task 4: perturbed `execute_step`'s `if derive_terminal_status(&ctx) ==
//!   "failed"` guard (`execute.rs`) to `if false && derive_terminal_status(&ctx) ==
//!   "failed"`, short-circuiting the now-shipped check so a failed `node_runs` entry is
//!   silently ignored again, exactly like the pre-`EN.11.D` behaviour. Observed failure:
//!   `panicked at crates/engine-core/tests/it/orchestration_chain.rs:570:10: execute_step
//!   must fail a step whose node_runs record a failed node: ExecutionOutcome { repo:
//!   "smoke-repo", ..., block_id: "B1", ctx: TaskContext { ..., node_runs:
//!   {"SetupWorktreeNode": NodeRun { status: Failed, ...,
//!   error: Some("worktree setup failed for B1"), ... }} } }`. Reverted with
//!   `git checkout --` before this task's commit.
//! - **(c)** [`red_authoritative_build_is_now_rejected_by_verify_state_write`] —
//!   perturbed `verify_state_write`'s `final_validation.all_passed == Some(false)`
//!   branch (`integrate.rs`) to `false && all_passed == Some(false)`, short-circuiting
//!   the gate so it never fires. Observed failure: `panicked at
//!   .../orchestration_chain.rs:645:48: FIXED: a red authoritative build
//!   (all_passed:false) must be rejected even though status reads done —
//!   EN.ticket.final-validation-failure-must-block: ()`.
//! - **(d)** [`lane_log_lines_use_the_fixed_ts_lane_repo_block_status_note_shape`] —
//!   perturbed `LaneLogEntry` (`integrate.rs`) with `#[serde(rename = "block_id")]` on
//!   the `block` field, reintroducing the retired key name. Observed failure:
//!   `panicked at .../orchestration_chain.rs:772:9: assertion `left == right` failed:
//!   FIXED shape only — lane-log line must carry exactly {ts, lane, repo, block,
//!   status, note}, got {"block_id": String("D.1"), ...} left: {"block_id", "lane",
//!   "note", "repo", "status", "ts"} right: {"block", "lane", "note", "repo", "status",
//!   "ts"}`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

use engine_contract::{NodeRun, NodeRunStatus};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::resolve_explicit_chain;
use engine_core::workflows::orchestration::execute::{
    execute_step, EngineKind, ExecuteError, ExecutionOutcome, FlowInvocation, FlowRunner,
};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, verify_state_write, IntegrateError, LaneLogStatus, NeverHeld, StepProgress,
};

// ── Git process helpers ──────────────────────────────────────────────────

/// Run `git <args>` with `cwd` as the working directory and panic with the
/// command and its stderr on a non-zero exit — every caller in this fixture
/// needs the operation to have actually happened, not merely been attempted.
fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `git {}` in {cwd:?}: {e}", args.join(" ")));
    assert!(
        output.status.success(),
        "`git {}` in {cwd:?} failed:\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The current tip of `branch` in `repo_path`, as a full commit hash — used
/// to prove a branch is a REAL git ref, not merely a file this fixture wrote.
fn rev_parse(repo_path: &Path, branch: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", branch])
        .current_dir(repo_path)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git rev-parse in {repo_path:?}: {e}"));
    assert!(
        output.status.success(),
        "git rev-parse --verify {branch} in {repo_path:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// ── Fixture repo construction ────────────────────────────────────────────

/// Build ONE real git repo with a real bare `origin`: `git init`, an initial
/// commit (a trivial `planning/harness.json` plus `README.md`), and a push
/// of `main` to a bare remote — matching smoke-run.md §1's fixture recipe
/// exactly, so a `checkout -B sdlc/<block> origin/main` inside it is a real
/// operation rather than a stand-in. `repo_path` must already exist and be
/// empty; the bare origin lives in its own, separate tempdir.
fn real_git_repo_with_bare_origin(repo_path: &Path) -> tempfile::TempDir {
    let bare_root = tempfile::tempdir().expect("bare-origin tempdir");
    let bare_path = bare_root.path().join("origin.git");
    let init_bare = Command::new("git")
        .args([
            "init",
            "-q",
            "--bare",
            bare_path.to_str().expect("utf8 path"),
        ])
        .output()
        .expect("spawn git init --bare");
    assert!(
        init_bare.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&init_bare.stderr)
    );

    run_git(repo_path, &["init", "-q"]);
    // Unborn-branch rename: works before the first commit regardless of the
    // installed git's `init.defaultBranch`, so the fixture never depends on
    // host config for which branch name `origin/main` ends up meaning.
    run_git(repo_path, &["checkout", "-q", "-b", "main"]);
    run_git(
        repo_path,
        &["config", "user.email", "fixture@example.invalid"],
    );
    run_git(repo_path, &["config", "user.name", "EN.11.B Fixture"]);

    std::fs::create_dir_all(repo_path.join("planning")).expect("mkdir planning");
    std::fs::write(
        repo_path.join("planning").join("harness.json"),
        r#"{"validation":{"checks":[]}}"#,
    )
    .expect("write harness.json");
    std::fs::write(repo_path.join("README.md"), "initial\n").expect("write README.md");

    run_git(repo_path, &["add", "-A"]);
    run_git(repo_path, &["commit", "-q", "-m", "initial commit"]);
    run_git(
        repo_path,
        &[
            "remote",
            "add",
            "origin",
            bare_path.to_str().expect("utf8 path"),
        ],
    );
    run_git(repo_path, &["push", "-q", "origin", "main"]);

    bare_root
}

/// A tempdir `brain.toml` + [`RepoRegistry`] holding exactly one repo slug,
/// whose directory is a REAL git repo with a real bare `origin`
/// ([`real_git_repo_with_bare_origin`]). Mirrors `orchestration.rs`'s
/// `two_repo_registry` SHAPE (tempdir root, `brain.toml`, `RepoRegistry`),
/// but with one real-git repo instead of two empty directories — this
/// fixture's whole point is that branch cuts are real operations.
///
/// Returns every tempdir that must outlive the fixture (brain root, bare
/// origin), the registry, and the repo's absolute path.
fn single_repo_fixture(
    slug: &str,
) -> (tempfile::TempDir, tempfile::TempDir, RepoRegistry, PathBuf) {
    let brain_root = tempfile::tempdir().expect("brain-root tempdir");
    let repo_path = brain_root.path().join(slug);
    std::fs::create_dir_all(&repo_path).expect("mkdir repo dir");
    let bare_root = real_git_repo_with_bare_origin(&repo_path);

    std::fs::write(
        brain_root.path().join("brain.toml"),
        format!("[[repos]]\nslug = \"{slug}\"\nrepo_path = \"{slug}\"\n"),
    )
    .expect("write brain.toml");
    let registry = RepoRegistry::from_brain_root(brain_root.path()).expect("registry");

    (brain_root, bare_root, registry, repo_path)
}

/// Write `repo_path/planning/{block_id}/sdlc/sdlc-flow-state.json` with
/// `{"status": status}` — the exact path `wrap_up.rs::state_path_for` (and
/// `integrate::state_path_for`, which mirrors it) reads. Mirrors
/// `orchestration.rs`'s `write_state` helper.
fn write_state(repo_path: &Path, block_id: &str, status: &str) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).expect("mkdir state dir");
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        serde_json::json!({"status": status}).to_string(),
    )
    .expect("write state file");
}

/// Build a tempdir roadmap directory under `planning/roadmaps/<slug>/` so
/// `append_lane_log_line` has somewhere to write that is never a real,
/// tracked roadmap. Mirrors `orchestration.rs`'s `fixture_roadmap_dir`.
fn fixture_roadmap_dir(slug: &str) -> (tempfile::TempDir, PathBuf) {
    let planning_root = tempfile::tempdir().expect("planning-root tempdir");
    let roadmap_dir = planning_root.path().join("roadmaps").join(slug);
    std::fs::create_dir_all(&roadmap_dir).expect("mkdir roadmap dir");
    (planning_root, roadmap_dir)
}

// ── The RecordingRunner double ───────────────────────────────────────────

/// A [`FlowRunner`] test double that mimics `SDLC_FLOW`'s branch discipline
/// on the happy path (smoke-run.md §1/§2): for every invocation it cuts
/// `sdlc/<block_id>` from `origin/main` — NEVER from a sibling block's
/// branch, which is exactly what makes case (a) (`EN.11.C`) observable —
/// writes and commits a marker file naming the block, pushes the branch,
/// and (unless overridden) writes a `"status": "done"` state file at the
/// path [`write_state`] uses.
///
/// Every invocation is recorded so a test can assert exactly which
/// `(repo, block_id, repo_path)` the chain actually drove the runner with,
/// not merely what the caller intended.
#[derive(Clone)]
struct RecordingRunner {
    calls: Arc<Mutex<Vec<(String, String, PathBuf)>>>,
    status_overrides: Arc<Mutex<HashMap<String, Option<String>>>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            status_overrides: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Force `block_id`'s state-file status to `status` instead of the
    /// default `"done"`. `None` skips writing the state file entirely —
    /// used by later tasks to reproduce a run that never reached
    /// `wrap_up.rs`.
    fn set_status_override(&self, block_id: &str, status: Option<&str>) {
        self.status_overrides
            .lock()
            .unwrap()
            .insert(block_id.to_string(), status.map(str::to_string));
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    #[allow(dead_code)] // wired up by later tasks in this spec
    fn calls_for(&self, block_id: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, id, _)| id == block_id)
            .count()
    }

    /// The per-block marker filename this runner writes into the branch it
    /// cuts — `"{block_id}.marker"`, one file per block rather than one
    /// shared name, so a branch that DID compose onto a prior block's tip
    /// would carry every ancestor's marker file, while a branch cut fresh
    /// from `origin/main` carries only its own. That is exactly the signal
    /// case (a) reads.
    fn marker_file(block_id: &str) -> String {
        format!("{block_id}.marker")
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

                // Real `SDLC_FLOW` branch discipline: cut fresh from
                // `origin/main`, never from a sibling block's branch tip.
                // This is smoke-run.md §3.4's exact recipe, and it is what
                // makes case (a)'s WRONG-today assertion possible to make.
                let branch = format!("sdlc/{}", invocation.block_id);
                run_git(
                    &invocation.repo_path,
                    &["checkout", "-q", "-B", &branch, "origin/main"],
                );

                let marker = format!("{}: marker\n", invocation.block_id);
                std::fs::write(
                    invocation
                        .repo_path
                        .join(Self::marker_file(&invocation.block_id)),
                    marker,
                )
                .expect("write marker file");
                run_git(&invocation.repo_path, &["add", "-A"]);
                run_git(
                    &invocation.repo_path,
                    &[
                        "commit",
                        "-q",
                        "-m",
                        &format!("{}: marker", invocation.block_id),
                    ],
                );
                run_git(&invocation.repo_path, &["push", "-q", "origin", &branch]);

                let status = this
                    .status_overrides
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
                    nodes: HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: HashMap::new(),
                })
            })
        })
    }
}

// ── Task 1 sanity: the scaffolding itself works ──────────────────────────

/// Proves the scaffolding, not production behaviour: a two-block chain
/// driven through [`RecordingRunner`] over a real git repo actually cuts
/// two real branches, both from `origin/main` (never from each other), and
/// leaves `origin/main` untouched. Cases (a)-(d) — which assert on
/// PRODUCTION `integrate_chain`/`verify_state_write` behaviour rather than
/// on this fixture's own plumbing — are later tasks in this spec.
#[tokio::test]
async fn recording_runner_cuts_a_real_branch_per_block_from_origin_main() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();

    let repo_root = registry.resolve("smoke-repo").expect("resolve smoke-repo");
    assert_eq!(repo_root, repo_path);

    let main_before = rev_parse(&repo_path, "origin/main");

    for block_id in ["SM.1", "SM.2"] {
        let invocation = engine_core::workflows::orchestration::execute::FlowInvocation {
            repo: "smoke-repo".to_string(),
            repo_path: repo_path.clone(),
            block_id: block_id.to_string(),
            use_worktree: false,
            campaign_id: Uuid::new_v4(),
            engine: EngineKind::Flow,
        };
        (flow_runner)(invocation)
            .await
            .unwrap_or_else(|e| panic!("runner invocation for {block_id} failed: {e}"));
    }

    assert_eq!(runner.call_count(), 2);

    // Both branches are real refs, each cut from the SAME `origin/main`
    // commit (not chained onto each other) — smoke-run.md §3.4's recipe.
    assert_eq!(rev_parse(&repo_path, "sdlc/SM.1^1"), main_before);
    assert_eq!(rev_parse(&repo_path, "sdlc/SM.2^1"), main_before);

    // origin/main itself was never mutated by either block's push.
    assert_eq!(rev_parse(&repo_path, "origin/main"), main_before);

    // Each block wrote its own state file at the path production reads.
    let state_a = repo_path
        .join("planning")
        .join("SM.1")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    let state_b = repo_path
        .join("planning")
        .join("SM.2")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    assert!(state_a.exists(), "SM.1 state file should exist");
    assert!(state_b.exists(), "SM.2 state file should exist");
    let state_a_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_a).unwrap()).unwrap();
    assert_eq!(state_a_json["status"], "done");
}

// ── Chain-level driver helpers (shared by cases (a)-(d)) ─────────────────

fn no_deps(_repo: &str, _id: &str) -> Vec<DependencyEdge> {
    Vec::new()
}

fn always_met(_repo: &str, _id: &str) -> bool {
    true
}

fn always_flow(_repo: &str, _id: &str) -> EngineKind {
    EngineKind::Flow
}

// ── Case (a): COMPOSITION — still WRONG today ─────────────────────────────
//
// smoke-run.md §3.4: a two-block chain over the SAME repo should, in the
// fixed world, cut block N+1's branch from block N's tip so the second
// block's tree contains the first block's work. It does not — `EngineRunner`
// cuts every block's branch from `origin/main`, so block N+1's tree is
// missing block N's marker entirely. `integrate_chain` calls `execute_step`
// per step (never chaining one step's branch into the next), and
// `RecordingRunner` mirrors real `SDLC_FLOW`'s branch discipline exactly
// (`checkout -B sdlc/<block> origin/main`), so this is production's actual
// behaviour, not an artifact of the double.
#[tokio::test]
// WRONG TODAY — flipped by EN.11.C (not in this lane; EN.11.C depends on
// this block).
async fn block_n_plus_1s_tree_lacks_block_ns_work_today() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-b-case-a");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();

    let chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "B1".to_string()),
        ("smoke-repo".to_string(), "B2".to_string()),
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
    .expect("two-block chain over one repo should integrate cleanly");
    assert_eq!(outcomes.len(), 2);

    // Check out B2's branch and inspect its tree. In the FIXED world (once
    // EN.11.C lands) `sdlc/B2` would be cut from `sdlc/B1`'s tip and would
    // therefore carry B1's marker file too. Today it does not: `sdlc/B2`
    // was cut from `origin/main`, exactly like `sdlc/B1` was, so B1's
    // marker never reaches B2's tree.
    run_git(&repo_path, &["checkout", "-q", "sdlc/B2"]);
    let b2_marker = repo_path.join(RecordingRunner::marker_file("B2"));
    let b1_marker_on_b2 = repo_path.join(RecordingRunner::marker_file("B1"));
    assert!(
        b2_marker.exists(),
        "B2's own marker must exist on its own branch"
    );
    assert!(
        !b1_marker_on_b2.exists(),
        "WRONG-TODAY: B2's tree must NOT contain B1's marker, because B2's \
         branch was cut from origin/main, not from B1's tip — flips by EN.11.C"
    );

    // Ancestry confirms the same fact at the git-graph level: B1's branch
    // tip is not an ancestor of B2's branch tip.
    let is_ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", "sdlc/B1", "sdlc/B2"])
        .current_dir(&repo_path)
        .status()
        .expect("spawn git merge-base --is-ancestor")
        .success();
    assert!(
        !is_ancestor,
        "WRONG-TODAY: sdlc/B1 must NOT be an ancestor of sdlc/B2 — flips by EN.11.C"
    );
}

// ── Case (b): FAILED STEP INVISIBLE — now FIXED ────────────────────────────
//
// smoke-run.md §3.2: `Workflow::walk`'s `if failed { break; }` falls through
// to `Ok(ctx)` — a run whose `SetupWorktreeNode` failed is reported as a
// *successful* `SDLC_FLOW` invocation, with the failure recorded only
// inside `ctx.node_runs`. `execute_step` (`execute.rs`) now reads
// `ctx.node_runs` via the shared `derive_terminal_status` and returns
// `ExecuteError::ChildFailed` naming the failing node, so the chain stops
// on the step itself rather than relying on `verify_state_write` to notice
// the state file `wrap_up.rs` never got to write.
#[tokio::test]
// FIXED by EN.11.D — see EN.11.B's original comment history for the
// pre-fix behaviour this case used to assert.
async fn a_failed_setup_worktree_step_stops_the_chain_via_execute_step() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");

    // A `FlowRunner` double that reproduces exactly the shape
    // `Workflow::walk` produces when a node fails: `Ok(ctx)`, with
    // `ctx.node_runs["SetupWorktreeNode"]` recording `NodeRunStatus::Failed`
    // — and, because the run never reached `wrap_up.rs`, no state file is
    // written at all.
    let failing_runner: FlowRunner = Arc::new(|invocation: FlowInvocation| {
        Box::pin(async move {
            let mut node_runs = HashMap::new();
            node_runs.insert(
                "SetupWorktreeNode".to_string(),
                NodeRun {
                    status: NodeRunStatus::Failed,
                    started_at: None,
                    completed_at: None,
                    error: Some(format!("worktree setup failed for {}", invocation.block_id)),
                    input: None,
                    usage: None,
                },
            );
            Ok(engine_contract::TaskContext {
                event: serde_json::json!({}),
                nodes: HashMap::new(),
                metadata: serde_json::json!({}),
                node_runs,
            })
        })
    });

    let chain = resolve_explicit_chain(vec![("smoke-repo".to_string(), "B1".to_string())]);

    // 1. `execute_step` now sees the failure directly: it reads
    //    `ctx.node_runs` via the shared `derive_terminal_status` and fails
    //    the step itself, naming the block and the failing node, WITHOUT
    //    ever needing to reach `verify_state_write`.
    let err = execute_step(
        &chain[0],
        &always_flow,
        &registry,
        &failing_runner,
        false,
        Uuid::new_v4(),
    )
    .await
    .expect_err("execute_step must fail a step whose node_runs record a failed node");
    match &err {
        ExecuteError::ChildFailed {
            repo,
            block_id,
            failing_node,
        } => {
            assert_eq!(repo, "smoke-repo");
            assert_eq!(block_id, "B1");
            assert_eq!(failing_node, "SetupWorktreeNode");
        }
        other => panic!("expected ExecuteError::ChildFailed, got {other:?}"),
    }

    // 2. Driving a TWO-step chain through `integrate_chain` confirms the
    //    chain does not merely fail — it stops before ever invoking the
    //    second step. `combined_runner` fails only "B1"; if the chain
    //    advanced to "B2" it would succeed, so a call count of 1 is direct
    //    evidence the chain did not advance past the failed step.
    let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = call_count.clone();
    let combined_runner: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let failing_runner = failing_runner.clone();
        Box::pin(async move {
            if invocation.block_id == "B1" {
                failing_runner(invocation).await
            } else {
                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes: HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: HashMap::new(),
                })
            }
        })
    });

    let two_step_chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "B1".to_string()),
        ("smoke-repo".to_string(), "B2".to_string()),
    ]);
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-b-case-b");
    let admission = AdmissionGate::with_default_policy();
    let chain_err = integrate_chain(
        &two_step_chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        &always_flow,
        &registry,
        &combined_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect_err("the chain must not report success for a failed SetupWorktreeNode");
    match &chain_err {
        IntegrateError::Execute(ExecuteError::ChildFailed {
            repo,
            block_id,
            failing_node,
        }) => {
            assert_eq!(repo, "smoke-repo");
            assert_eq!(block_id, "B1");
            assert_eq!(failing_node, "SetupWorktreeNode");
        }
        other => panic!(
            "the chain must stop via ExecuteError::ChildFailed naming the \
             failed block and node, got {other:?}"
        ),
    }
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the chain must NOT advance to B2 after B1 fails"
    );

    let _ = repo_path; // kept alive for the fixture's Drop ordering
}

// ── Case (c): RED AUTHORITATIVE BUILD — now FIXED ──────────────────────────
//
// smoke-run.md §3.3 originally observed a `"status": "done"` state file
// with `final_validation.all_passed: false` being admitted as a successful
// block close. That bug is closed: `verify_state_write` now rejects such a
// file with `IntegrateError::FinalValidationGateFailed`, independently of
// `wrap_up.rs`'s own in-engine guard, so a state file written by an older
// engine build or by the JS `/sdlc-flow` is still caught. This fixture
// therefore asserts the FIXED value, never the pre-fix one — see the block
// record's 2026-08-21 amendment.

/// A synthetic [`ExecutionOutcome`] whose `repo_path` is a fresh tempdir
/// carrying only the one state file `body` describes — `verify_state_write`
/// reads nothing else, so this is a faithful, minimal stand-in for a real
/// `SDLC_FLOW` run's result without driving the whole chain.
fn outcome_with_state_file(
    block_id: &str,
    body: &serde_json::Value,
) -> (tempfile::TempDir, ExecutionOutcome) {
    let repo_root = tempfile::tempdir().expect("state-file tempdir");
    write_state_json(repo_root.path(), block_id, body);
    let outcome = ExecutionOutcome {
        repo: "smoke-repo".to_string(),
        repo_path: repo_root.path().to_path_buf(),
        block_id: block_id.to_string(),
        engine: EngineKind::Flow,
        ctx: engine_contract::TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        },
        use_worktree: false,
        campaign_id: Uuid::new_v4(),
        cost_usd: None,
        total_tokens: 0,
    };
    (repo_root, outcome)
}

/// Like [`write_state`] but takes an arbitrary JSON body instead of a bare
/// `{"status": ..}` — needed here so a case can add `final_validation`
/// alongside `"status"`.
fn write_state_json(repo_path: &Path, block_id: &str, body: &serde_json::Value) {
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).expect("mkdir state dir");
    std::fs::write(
        dir.join("sdlc-flow-state.json"),
        serde_json::to_string(body).expect("serialize state body"),
    )
    .expect("write state file");
}

#[tokio::test]
// FIXED by EN.ticket.final-validation-failure-must-block (closed
// 2026-08-20).
async fn red_authoritative_build_is_now_rejected_by_verify_state_write() {
    // `status: "done"` + `final_validation.all_passed: false` must now be
    // REJECTED — this is the flip EN.ticket.final-validation-failure-must-
    // block made. A pre-fix assertion of `Ok` here would be exactly the
    // green-on-landing-but-wrong fixture this block's amendment exists to
    // prevent.
    let (_tmp, red_outcome) = outcome_with_state_file(
        "C.1",
        &serde_json::json!({
            "status": "done",
            "final_validation": {
                "all_passed": false,
                "failure_summary": "cargo clippy failed with 2 warnings",
            },
        }),
    );
    let err = verify_state_write(&red_outcome).expect_err(
        "FIXED: a red authoritative build (all_passed:false) must be rejected \
         even though status reads done — EN.ticket.final-validation-failure-must-block",
    );
    match err {
        IntegrateError::FinalValidationGateFailed {
            ref failure_summary,
            ..
        } => {
            assert_eq!(failure_summary, "cargo clippy failed with 2 warnings");
        }
        other => panic!("expected FinalValidationGateFailed, got {other:?}"),
    }

    // Must-still-pass case 1: `all_passed: true` is a normal successful
    // close and must still verify Ok.
    let (_tmp2, green_outcome) = outcome_with_state_file(
        "C.2",
        &serde_json::json!({
            "status": "done",
            "final_validation": {"all_passed": true},
        }),
    );
    assert!(
        verify_state_write(&green_outcome).is_ok(),
        "a green authoritative build (all_passed:true) must still verify Ok"
    );

    // Must-still-pass case 2: `final_validation` entirely absent (a
    // JS-written state file, or one from before EN.3.E) must still verify
    // Ok — only an EXPLICIT `false` is a failure, never absence.
    let (_tmp3, no_gate_outcome) =
        outcome_with_state_file("C.3", &serde_json::json!({"status": "done"}));
    assert!(
        verify_state_write(&no_gate_outcome).is_ok(),
        "a state file with no final_validation key at all must still verify Ok \
         — absence is not itself a failure"
    );
}

// ── Case (d): LANE-LOG SHAPE — now FIXED ───────────────────────────────────
//
// smoke-run.md §3.7 originally observed lane-log lines shaped
// `{repo, block_id, integrated_at}`. That shape is retired:
// `EN.ticket.lane-log-entry-schema` reshaped every appended line to
// `{ts, lane, repo, block, status, note}` with a typed `LaneLogStatus`.
// This fixture asserts the FIXED shape by driving the real chain end to
// end (not `LaneLogEntry` in isolation) so it also proves `integrate_chain`
// itself appends lines in that shape, not merely that the type can be
// constructed that way.
#[tokio::test]
// FIXED by EN.ticket.lane-log-entry-schema (closed 2026-08-20).
async fn lane_log_lines_use_the_fixed_ts_lane_repo_block_status_note_shape() {
    let (_brain_root, _bare_root, registry, _repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-b-case-d");
    let admission = AdmissionGate::with_default_policy();

    // First: a two-block chain that closes cleanly, producing two
    // `closed` lines.
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "D.1".to_string()),
        ("smoke-repo".to_string(), "D.2".to_string()),
    ]);
    integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("en-11-b-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("two-block chain should close cleanly");

    // Second: a block whose state write never happens (RecordingRunner
    // overridden to skip it), so `verify_state_write` fails and the chain
    // appends a `bailed` line instead of `closed`.
    runner.set_status_override("D.3", None);
    let chain_fail = resolve_explicit_chain(vec![("smoke-repo".to_string(), "D.3".to_string())]);
    integrate_chain(
        &chain_fail,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        None,
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("en-11-b-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect_err("D.3 never writes a state file, so the chain must stop");

    let contents = std::fs::read_to_string(roadmap_dir.join("lane-log.jsonl"))
        .expect("lane-log.jsonl must exist after three appended lines");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3, "two closed lines + one bailed line");

    let expected_keys: std::collections::BTreeSet<&str> =
        ["ts", "lane", "repo", "block", "status", "note"]
            .into_iter()
            .collect();

    let mut statuses = Vec::new();
    for line in &lines {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("each lane-log line must be valid JSON");
        let obj = value.as_object().expect("each line must be a JSON object");

        // FIXED: the key set is EXACTLY {ts, lane, repo, block, status,
        // note} — NOT the old {repo, block_id, integrated_at}. An added or
        // renamed field fails this assertion.
        let keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            keys, expected_keys,
            "FIXED shape only — lane-log line must carry exactly \
             {{ts, lane, repo, block, status, note}}, got {obj:?}"
        );
        assert!(
            !obj.contains_key("block_id") && !obj.contains_key("integrated_at"),
            "the OLD {{repo, block_id, integrated_at}} shape must not reappear: {obj:?}"
        );

        let status_str = obj["status"].as_str().expect("status must be a string");
        let status: LaneLogStatus = serde_json::from_value(value["status"].clone())
            .expect("status must be a LaneLogStatus");
        statuses.push(status);
        assert!(
            status_str == "closed" || status_str == "bailed",
            "unexpected status value: {status_str}"
        );
    }

    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == LaneLogStatus::Closed)
            .count(),
        2,
        "D.1 and D.2 each produce exactly one closed line"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == LaneLogStatus::Bailed)
            .count(),
        1,
        "D.3's missing state write produces exactly one bailed line"
    );
}
