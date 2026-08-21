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
//! behaviour "should" eventually be — see the block record's amendment note. Two of the
//! four cases are still wrong today and are asserted wrong, each carrying an inline
//! comment naming the block that flips it (never asserted BEFORE that block lands: this
//! is `EN.11.B` task 1, scaffolding only — cases live in later tasks of this same spec):
//!
//! - (a) COMPOSITION (smoke-run.md §3.4) — still WRONG. Flipped by `EN.11.C`.
//! - (b) FAILED STEP INVISIBLE (smoke-run.md §3.2) — still WRONG. Flipped by `EN.11.D`.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use engine_contract::{NodeRun, NodeRunStatus};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::resolve_explicit_chain;
use engine_core::workflows::orchestration::execute::{
    execute_step, EngineKind, FlowInvocation, FlowRunner,
};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, verify_state_write, IntegrateError, NeverHeld, StepProgress,
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
#[allow(dead_code)] // wired up by later tasks in this spec (case (d))
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
    #[allow(dead_code)] // wired up by later tasks in this spec (cases (b)/(c))
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
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
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

// ── Case (b): FAILED STEP INVISIBLE — still WRONG today ───────────────────
//
// smoke-run.md §3.2: `Workflow::walk`'s `if failed { break; }` still falls
// through to `Ok(ctx)` — a run whose `SetupWorktreeNode` failed is reported
// as a *successful* `SDLC_FLOW` invocation, with the failure recorded only
// inside `ctx.node_runs`. `execute_step` (`execute.rs`) does not inspect
// `node_runs` at all: it only distinguishes `run_flow`'s own `Ok`/`Err`.
// The ONLY thing that stops the chain on a failed step today is
// `verify_state_write` failing to find the state file `wrap_up.rs` never
// got to write.
#[tokio::test]
// WRONG TODAY — flipped by EN.11.D (runs after this block in
// lane-engine-rs.txt).
async fn a_failed_setup_worktree_step_is_invisible_to_execute_step_today() {
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

    // 1. `execute_step` itself cannot see the failure — it only looks at
    //    `run_flow`'s own `Ok`/`Err`, never at `ctx.node_runs`.
    let outcome = execute_step(&chain[0], &always_flow, &registry, &failing_runner)
        .await
        .expect(
            "WRONG-TODAY: execute_step must return Ok even though \
             SetupWorktreeNode failed inside node_runs — flips by EN.11.D",
        );
    assert_eq!(
        outcome.ctx.node_runs["SetupWorktreeNode"].status,
        NodeRunStatus::Failed,
        "the failure IS present in node_runs — execute_step just never looks there"
    );

    // 2. The only thing that stops the chain is verify_state_write, because
    //    the run never reached wrap_up.rs and so never wrote the state
    //    file `verify_state_write` looks for.
    let err = verify_state_write(&outcome)
        .expect_err("verify_state_write must fail: no state file was ever written");
    assert!(
        matches!(err, IntegrateError::StateWriteUnreadable { .. }),
        "expected StateWriteUnreadable (absent state file), got {err:?}"
    );

    // 3. Driving the full chain through integrate_chain confirms the same
    //    thing end to end: the chain DOES stop, but via verify_state_write's
    //    IntegrateError, never via execute_step/ExecuteError — that is what
    //    "invisible" means here.
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-b-case-b");
    let admission = AdmissionGate::with_default_policy();
    let chain_err = integrate_chain(
        &chain,
        &no_deps,
        &always_met,
        &admission,
        &NeverHeld,
        Duration::from_millis(5),
        None,
        &always_flow,
        &registry,
        &failing_runner,
        &roadmap_dir,
        None,
        &|_: &StepProgress| {},
    )
    .await
    .expect_err("the chain must not report success for a failed SetupWorktreeNode");
    assert!(
        matches!(chain_err, IntegrateError::StateWriteUnreadable { .. }),
        "the chain must stop via verify_state_write, not via ExecuteError — \
         got {chain_err:?}"
    );

    let _ = repo_path; // kept alive for the fixture's Drop ordering
}
