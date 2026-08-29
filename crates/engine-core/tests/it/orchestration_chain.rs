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
//! - (a) COMPOSITION (smoke-run.md §3.4) — now FIXED, by `EN.11.C`.
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
//! - **(a)** [`block_n_plus_1s_tree_contains_block_ns_work`] (as of `EN.11.B` task 4,
//!   named `block_n_plus_1s_tree_lacks_block_ns_work_today` and asserted at its
//!   then-current WRONG value) — perturbed `integrate_chain`'s per-step loop
//!   (`integrate.rs`) to push each step's completed branch tip to `origin/main` and
//!   fetch it back (simulating the chaining `EN.11.C` would add), immediately after
//!   `verify_state_write` succeeds. Observed failure: `panicked at
//!   .../orchestration_chain.rs:456:5: WRONG-TODAY: B2's tree must NOT contain B1's
//!   marker, because B2's branch was cut from origin/main, not from B1's tip — flips
//!   by EN.11.C`. `EN.11.C` task 3 has since flipped this case to its FIXED value —
//!   see that block's own commit for the current perturbation record.
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

use engine_contract::{JournalDecisionKind, JournalRow, NodeRun, NodeRunStatus};
use engine_core::budget::Budget;
use engine_core::cancellation::CancellationToken;
use engine_core::nodes::{RecallNode, StubHttpGet, RECALL_NODE_NAME};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::{resolve_explicit_chain, ChainStep, StepKind};
use engine_core::workflows::orchestration::execute::{
    execute_step, EngineKind, ExecuteError, ExecutionOutcome, FlowInvocation, FlowRunner,
};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain, integrate_chain_with_dispatch, verify_state_write, IntegrateError,
    LaneLogStatus, NeverHeld, StepProgress,
};
use engine_core::workflows::recall::RECALL_WORKFLOW_TYPE;
use engine_core::{BrainConfig, Dispatcher, HttpGet, NodeRegistry, Workflow};

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
    // `EN.11.F` task 5: a block's simulated cost, used only by the
    // campaign-ceiling cases below — absent a set override, a block
    // reports NO cost figure at all (empty `ctx.nodes`, matching every
    // earlier task's fixture exactly), never a silent `$0`.
    cost_overrides: Arc<Mutex<HashMap<String, f64>>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            status_overrides: Arc::new(Mutex::new(HashMap::new())),
            cost_overrides: Arc::new(Mutex::new(HashMap::new())),
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

    /// Give `block_id`'s simulated child run one reporting node with
    /// `cost_usd: cost` — the only way this double can produce a non-`None`
    /// `ExecutionOutcome::cost_usd` (`execute.rs::step_spend` requires at
    /// least one `ctx.nodes` entry carrying a `"cost_usd"` number). Used by
    /// `EN.11.F` task 5's campaign-ceiling cases.
    fn set_cost_override(&self, block_id: &str, cost: f64) {
        self.cost_overrides
            .lock()
            .unwrap()
            .insert(block_id.to_string(), cost);
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

                let mut nodes = HashMap::new();
                if let Some(cost) = this
                    .cost_overrides
                    .lock()
                    .unwrap()
                    .get(&invocation.block_id)
                    .copied()
                {
                    nodes.insert(
                        invocation.block_id.clone(),
                        serde_json::json!({"cost_usd": cost}),
                    );
                }
                // `EN.11.C` task 1: the real `PullRequestNode` stamps the
                // branch it pushed onto `ctx.nodes["PullRequestNode"]`, on
                // both the `auto_pr: true` and `auto_pr: false` shapes —
                // this double mirrors that so `integrate_chain`'s merge
                // stage (`resolve_merge_branch`) has a branch name to find,
                // exactly like a real `SDLC_FLOW` run would leave behind.
                nodes.insert(
                    "PullRequestNode".to_string(),
                    serde_json::json!({
                        "branch_name": branch,
                        "pr_url": null,
                        "skipped": true,
                    }),
                );

                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes,
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
            cancellation_token: None,
            budget: None,
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

// ── Case (a): COMPOSITION — now FIXED by EN.11.C ──────────────────────────
//
// smoke-run.md §3.4: a two-block chain over the SAME repo should cut block
// N+1's branch from a tree that actually contains block N's work. Each
// block's branch is still cut fresh from `origin/main` (`RecordingRunner`
// mirrors real `SDLC_FLOW`'s branch discipline exactly:
// `checkout -B sdlc/<block> origin/main` — that per-step discipline does
// NOT change), but `integrate_chain`'s new merge stage (`EN.11.C` task 2)
// now merges each just-integrated step's branch into `main` and pushes it
// before the next step starts, so block N+1's `origin/main` — and
// therefore its own branch — carries block N's work. See EN.11.C's block
// record for the fix.
#[tokio::test]
async fn block_n_plus_1s_tree_contains_block_ns_work() {
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

    // Check out B2's branch and inspect its tree. B1 integrated first, so
    // `integrate_chain`'s merge stage landed `sdlc/B1` on `origin/main`
    // before B2's step ever ran — `sdlc/B2` was cut fresh from that now-
    // updated `origin/main` (per-step branch discipline is unchanged), so
    // it carries B1's marker file too.
    run_git(&repo_path, &["checkout", "-q", "sdlc/B2"]);
    let b2_marker = repo_path.join(RecordingRunner::marker_file("B2"));
    let b1_marker_on_b2 = repo_path.join(RecordingRunner::marker_file("B1"));
    assert!(
        b2_marker.exists(),
        "B2's own marker must exist on its own branch"
    );
    assert!(
        b1_marker_on_b2.exists(),
        "FIXED: B2's tree must contain B1's marker — EN.11.C's merge stage \
         landed sdlc/B1 on origin/main before B2's branch was cut"
    );

    // Ancestry confirms the same fact at the git-graph level: B1's branch
    // tip is now an ancestor of B2's branch tip.
    let is_ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", "sdlc/B1", "sdlc/B2"])
        .current_dir(&repo_path)
        .status()
        .expect("spawn git merge-base --is-ancestor")
        .success();
    assert!(
        is_ancestor,
        "FIXED: sdlc/B1 must be an ancestor of sdlc/B2 — EN.11.C composes the chain"
    );

    // `main` itself also carries B1's marker — the merge stage pushed it
    // there, not merely into B2's branch.
    run_git(&repo_path, &["checkout", "-q", "main"]);
    assert!(
        repo_path.join(RecordingRunner::marker_file("B1")).exists(),
        "main must carry B1's marker after the merge stage pushed it"
    );
}

/// Gate-capable-of-failing companion to
/// [`block_n_plus_1s_tree_contains_block_ns_work`] (base-template D68
/// constraint 4): builds the identical fixture and cuts two branches BY
/// HAND, both from `origin/main`, without ever driving `integrate_chain` —
/// exactly the pre-`EN.11.C` shape (no merge stage ever ran). The same
/// composition probe (marker presence + `merge-base --is-ancestor`) must
/// report NOT-composed against this hand-built state, proving the probe
/// above actually distinguishes the composed world from the uncomposed one
/// rather than passing vacuously.
#[tokio::test]
async fn composition_probe_reports_not_composed_without_the_merge_stage() {
    let (_brain_root, _bare_root, _registry, repo_path) = single_repo_fixture("smoke-repo");

    // Cut sdlc/B1 from origin/main, commit a marker, push it — but never
    // merge it back into main, and never let `integrate_chain` touch it.
    run_git(
        &repo_path,
        &["checkout", "-q", "-B", "sdlc/B1", "origin/main"],
    );
    std::fs::write(
        repo_path.join(RecordingRunner::marker_file("B1")),
        "B1: marker\n",
    )
    .expect("write B1 marker");
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "B1: marker"]);
    run_git(&repo_path, &["push", "-q", "origin", "sdlc/B1"]);

    // Cut sdlc/B2 from origin/main too — origin/main was never updated
    // with B1's work, so this is exactly the pre-EN.11.C shape.
    run_git(
        &repo_path,
        &["checkout", "-q", "-B", "sdlc/B2", "origin/main"],
    );
    std::fs::write(
        repo_path.join(RecordingRunner::marker_file("B2")),
        "B2: marker\n",
    )
    .expect("write B2 marker");
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "B2: marker"]);
    run_git(&repo_path, &["push", "-q", "origin", "sdlc/B2"]);

    run_git(&repo_path, &["checkout", "-q", "sdlc/B2"]);
    let b1_marker_on_b2 = repo_path.join(RecordingRunner::marker_file("B1"));
    assert!(
        !b1_marker_on_b2.exists(),
        "NOT-composed: without the merge stage, B2's tree must not carry B1's marker"
    );
    let is_ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", "sdlc/B1", "sdlc/B2"])
        .current_dir(&repo_path)
        .status()
        .expect("spawn git merge-base --is-ancestor")
        .success();
    assert!(
        !is_ancestor,
        "NOT-composed: without the merge stage, sdlc/B1 must not be an ancestor of sdlc/B2"
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
        None,
        None,
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

    let required_keys: std::collections::BTreeSet<&str> =
        ["ts", "lane", "repo", "block", "status", "note"]
            .into_iter()
            .collect();
    // EN.11.A task 5 adds `run_id`, `writer`, `build_sha` as additive,
    // `skip_serializing_if = "Option::is_none"` identity fields — legal on
    // an engine-written line. Anything else is a renamed or unexpected key.
    let allowed_extra_keys: std::collections::BTreeSet<&str> =
        ["run_id", "writer", "build_sha"].into_iter().collect();

    let mut statuses = Vec::new();
    for line in &lines {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("each lane-log line must be valid JSON");
        let obj = value.as_object().expect("each line must be a JSON object");

        // FIXED core shape {ts, lane, repo, block, status, note} plus only
        // the known-additive identity keys — NOT the old {repo, block_id,
        // integrated_at}. A renamed or unrecognized field fails this
        // assertion.
        let keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert!(
            keys.is_superset(&required_keys),
            "lane-log line must carry at least {{ts, lane, repo, block, status, note}}, got {obj:?}"
        );
        let extra: std::collections::BTreeSet<&str> =
            keys.difference(&required_keys).copied().collect();
        assert!(
            extra.is_subset(&allowed_extra_keys),
            "lane-log line carries unrecognized keys beyond the fixed shape and the known \
             additive identity fields {{run_id, writer, build_sha}}: extra={extra:?}, got {obj:?}"
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

// ── EN.11.F task 5: abort at a block boundary + the campaign ceiling ─────
//
// HONESTY REQUIREMENT (this block's `carryover_context`:
// `orchestration-workflow-never-driven-a-real-chain` stands): every case
// below drives `integrate_chain` over `single_repo_fixture`'s tempdir git
// repos through `RecordingRunner`, exactly like every earlier task in this
// fixture. A green result here proves `integrate_chain`'s own control flow
// — the cancellation check, the campaign ceiling check, and the
// commit/halt bookkeeping — behaves as specified. It is NOT evidence that
// a real chain, driving real `claude` subprocesses through `engine-serve`,
// was ever actually aborted; that remains unproven and the carryover is
// NOT cleared by this task. The un-gateable "no `claude` subprocess
// survives 30s after an abort" criterion is covered separately (task 6:
// a `pgrep` recipe run/recorded in the block's run notes), never by a
// fixture test.
//
// The block's fifth AC — "an abort of an unknown or already-finished
// campaign returns a stable error, never a 500, never a hang" — is
// integration-tested at the HTTP surface in
// `crates/engine-serve/src/abort.rs` (`EN.11.F` task 2:
// `abort_unknown_campaign_returns_404` and friends), which is where that
// behaviour actually lives; `integrate_chain` itself has no notion of an
// "unknown campaign", only a token it is or is not handed. Not duplicated
// here.

/// A [`FlowRunner`] wrapper that triggers `token.cancel()` immediately
/// after `after_block_id`'s invocation completes — simulating an operator
/// abort landing in the gap between one block finishing and the next
/// block's boundary check, which is exactly the window Fork 1's decided
/// semantics (`integrate_chain`'s "Cancellation stops the chain BETWEEN
/// steps only" doc) says an abort can and cannot reach.
fn cancel_after(
    base: FlowRunner,
    token: CancellationToken,
    after_block_id: &'static str,
) -> FlowRunner {
    Arc::new(move |invocation| {
        let base = base.clone();
        let token = token.clone();
        Box::pin(async move {
            let block_id = invocation.block_id.clone();
            let result = (base)(invocation).await;
            if block_id == after_block_id {
                token.cancel();
            }
            result
        })
    })
}

/// AC: "Aborting a running campaign stops the chain at the next block
/// boundary" + "the in-flight block at abort time still finishes and
/// commits" (Fork 1's decided semantics — an abort that discards
/// in-flight work is a FAIL, not an over-delivery).
///
/// The cancellation token is triggered the instant block 1's invocation
/// returns (see [`cancel_after`]), so by the time `integrate_chain`'s loop
/// reaches the boundary before block 2, the token already reads cancelled.
/// Block 1 must therefore be fully committed (its branch pushed, its
/// state file written, its `closed` lane-log line on disk) while block 2
/// is never dispatched at all.
#[tokio::test]
async fn abort_between_blocks_leaves_block_one_committed_and_block_two_unstarted() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-f-abort");
    let admission = AdmissionGate::with_default_policy();

    let runner = RecordingRunner::new();
    let token = CancellationToken::new();
    let flow_runner = cancel_after(runner.clone().into_runner(), token.clone(), "TF.1");

    let chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "TF.1".to_string()),
        ("smoke-repo".to_string(), "TF.2".to_string()),
    ]);

    let outcomes = integrate_chain(
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
        &roadmap_dir,
        Some("en-11-f-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("a cancellation win is Ok, not Err — never rolled back");

    // Only block 1 ran; block 2 was never even dispatched to the runner.
    assert_eq!(outcomes.len(), 1, "only TF.1 should have been integrated");
    assert_eq!(outcomes[0].block_id, "TF.1");
    assert_eq!(runner.calls_for("TF.1"), 1);
    assert_eq!(
        runner.calls_for("TF.2"),
        0,
        "TF.2 must never be dispatched after the cancellation win"
    );

    // Block 1's work is REALLY committed: a real branch, pushed, with its
    // marker commit — never rolled back by the abort.
    assert!(
        Command::new("git")
            .args(["rev-parse", "--verify", "sdlc/TF.1"])
            .current_dir(&repo_path)
            .output()
            .expect("git rev-parse")
            .status
            .success(),
        "TF.1's branch must exist — the in-flight block still finishes and commits"
    );
    let state_a = repo_path
        .join("planning")
        .join("TF.1")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    assert!(state_a.exists(), "TF.1's state file must be written");

    // Block 2's branch was never cut at all.
    let tf2_branch = Command::new("git")
        .args(["rev-parse", "--verify", "sdlc/TF.2"])
        .current_dir(&repo_path)
        .output()
        .expect("git rev-parse");
    assert!(
        !tf2_branch.status.success(),
        "TF.2's branch must not exist — it was never started"
    );

    // The lane-log record distinguishes "TF.1 closed" from "TF.2 never
    // started because of an explicit cancellation" — never collapsed into
    // a single ambiguous line.
    let contents = std::fs::read_to_string(roadmap_dir.join("lane-log.jsonl"))
        .expect("lane-log.jsonl must exist");
    let lines: Vec<serde_json::Value> = contents
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();
    assert_eq!(lines.len(), 2, "one closed line + one cancelled line");
    assert_eq!(lines[0]["block"], "TF.1");
    assert_eq!(lines[0]["status"], "closed");
    assert_eq!(lines[1]["block"], "TF.2");
    assert_eq!(lines[1]["status"], "cancelled");
}

/// AC: "A campaign ceiling set BELOW one block's cost halts the chain at
/// the block boundary rather than after the whole chain, and the halt
/// reason names the cap that tripped" + "the campaign budget is checked
/// more than once per campaign ... a test with a two-block chain shows the
/// check running at both boundaries".
///
/// The cap is set to EXACTLY block 1's reported cost. If the boundary
/// check only ran once, up front, against a still-empty ledger (spend =
/// 0 < cap), the chain would run both blocks to completion — so a halt
/// before block 2 is only reachable if: (1) the FIRST boundary check ran
/// and allowed block 1 through (spend 0 < cap), (2) block 1's cost was
/// correctly folded into the campaign ledger afterward, and (3) the
/// SECOND boundary check ran again, against the now-nonzero ledger, and
/// halted (spend >= cap). That is "checked at both boundaries", made
/// observable rather than assumed.
#[tokio::test]
async fn campaign_ceiling_below_one_blocks_cost_halts_at_first_boundary() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-f-ceiling");
    let admission = AdmissionGate::with_default_policy();

    let runner = RecordingRunner::new();
    runner.set_cost_override("TC.1", 5.0);
    // TC.2 is never reached, so its cost is irrelevant — no override set.
    let flow_runner = runner.clone().into_runner();

    let budget = Budget {
        max_total_tokens: None,
        max_cost_usd: Some(5.0), // exactly TC.1's cost
    };

    let chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "TC.1".to_string()),
        ("smoke-repo".to_string(), "TC.2".to_string()),
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
        Some(&budget),
        &always_flow,
        &registry,
        &flow_runner,
        &roadmap_dir,
        Some("en-11-f-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect("a budget halt is Ok, not Err — never rolled back");

    // Block 1 ran (boundary 1's check allowed it, spend 0 < cap 5.0);
    // block 2 never dispatched (boundary 2's check halted, spend 5.0 >=
    // cap 5.0) — proving BOTH boundary checks ran, not just the first.
    assert_eq!(outcomes.len(), 1, "only TC.1 should have been integrated");
    assert_eq!(runner.calls_for("TC.1"), 1);
    assert_eq!(
        runner.calls_for("TC.2"),
        0,
        "the ceiling must halt the chain BEFORE TC.2's boundary, not after \
         the whole chain finishes"
    );

    // TC.1's work is still committed — a budget halt is not a rollback.
    let state_a = repo_path
        .join("planning")
        .join("TC.1")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    assert!(state_a.exists(), "TC.1's state file must be written");

    let contents = std::fs::read_to_string(roadmap_dir.join("lane-log.jsonl"))
        .expect("lane-log.jsonl must exist");
    let lines: Vec<serde_json::Value> = contents
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();
    assert_eq!(lines.len(), 2, "one closed line + one budget_halted line");
    assert_eq!(lines[0]["block"], "TC.1");
    assert_eq!(lines[0]["status"], "closed");
    assert_eq!(lines[1]["block"], "TC.2");
    assert_eq!(lines[1]["status"], "budget_halted");
    let note = lines[1]["note"].as_str().expect("note must be a string");
    assert!(
        note.contains("max_cost_usd"),
        "the halt reason must name the cap that tripped: {note}"
    );
}

// ── EN.11.C task 3: the merge stage itself ───────────────────────────────

/// Wraps `base` so that, for exactly `block_id`, the returned `ctx`'s
/// `PullRequestNode.branch_name` is overwritten to `bogus_branch` — a
/// branch name the fixture never actually pushes. Used to drive
/// `integrate_chain`'s merge stage into `merge_step_branch`'s failure path
/// (an unmergeable, in this case nonexistent, ref) without needing a real
/// merge conflict.
fn bad_branch_for(
    base: FlowRunner,
    block_id: &'static str,
    bogus_branch: &'static str,
) -> FlowRunner {
    Arc::new(move |invocation| {
        let base = base.clone();
        let id = invocation.block_id.clone();
        Box::pin(async move {
            let mut ctx = (base)(invocation).await?;
            if id == block_id {
                ctx.nodes.insert(
                    "PullRequestNode".to_string(),
                    serde_json::json!({
                        "branch_name": bogus_branch,
                        "pr_url": null,
                        "skipped": true,
                    }),
                );
            }
            Ok(ctx)
        })
    })
}

/// AC: "A step whose merge cannot be completed returns the new
/// `IntegrateError` variant, and the error's `Display` output contains the
/// git stderr; the lane log holds a `bailed` line and no `closed` line for
/// that block." Block `MB.1` integrates normally (its own state write and
/// lane-log `closed` line happen before the merge stage runs — the merge
/// stage is the LAST thing before that `closed` line is appended, so `MB.1`
/// itself never reaches this failure), but `MB.1`'s reported branch name is
/// overwritten to one that was never pushed, so `merge_step_branch`'s
/// `git merge --no-ff <bogus branch>` fails with a nonzero exit and real
/// git stderr.
#[tokio::test]
async fn an_unmergeable_step_fails_the_step_and_never_closes_it() {
    let (_brain_root, _bare_root, registry, _repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-c-merge-fail");
    let admission = AdmissionGate::with_default_policy();

    let runner = RecordingRunner::new();
    let flow_runner = bad_branch_for(
        runner.clone().into_runner(),
        "MB.1",
        "sdlc/this-branch-was-never-pushed",
    );

    let chain = resolve_explicit_chain(vec![("smoke-repo".to_string(), "MB.1".to_string())]);

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
        Some("en-11-c-merge-fail-lane"),
        &|_: &StepProgress| {},
        false,
        Uuid::new_v4(),
    )
    .await
    .expect_err("a merge onto a nonexistent branch must fail the step");

    match &err {
        IntegrateError::StepMergeFailed {
            block_id, stderr, ..
        } => {
            assert_eq!(block_id, "MB.1");
            assert!(
                !stderr.is_empty(),
                "the git stderr must reach the error, never be swallowed"
            );
        }
        other => panic!("expected IntegrateError::StepMergeFailed, got: {other:?}"),
    }
    assert!(
        err.to_string().len()
            > "block 'MB.1' (repo 'smoke-repo') integrated but its branch \
             'sdlc/this-branch-was-never-pushed' could not be merged into main and pushed: "
                .len(),
        "Display output must carry the actual git stderr text, not just the static prefix: {err}"
    );

    let contents = std::fs::read_to_string(roadmap_dir.join("lane-log.jsonl"))
        .expect("lane-log.jsonl must exist after the bailed line");
    let lines: Vec<serde_json::Value> = contents
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();
    assert_eq!(lines.len(), 1, "exactly one lane-log line for MB.1");
    assert_eq!(lines[0]["block"], "MB.1");
    assert_eq!(
        lines[0]["status"], "bailed",
        "an unmergeable step must never be recorded as closed"
    );
}

/// AC: "The `planning/` symlink still resolves into the canonical vault
/// inside a worktree after the merge stage runs" (base-template D50 /
/// seams.md seam 9). Replaces the fixture repo's real, committed
/// `planning/` directory with a symlink into a separate tempdir vault —
/// exactly this repo's own on-disk shape — then drives a two-block chain
/// through the merge stage and asserts the symlink still resolves to the
/// same vault afterward.
#[tokio::test]
async fn planning_symlink_still_resolves_into_its_vault_after_the_merge_stage() {
    let (_brain_root, _bare_root, registry, repo_path) = single_repo_fixture("smoke-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-11-c-symlink");
    let admission = AdmissionGate::with_default_policy();

    let vault = tempfile::tempdir().expect("vault tempdir");
    std::fs::write(
        vault.path().join("harness.json"),
        r#"{"validation":{"checks":[]}}"#,
    )
    .expect("write vault harness.json");
    std::fs::remove_dir_all(repo_path.join("planning")).expect("remove real planning dir");
    std::os::unix::fs::symlink(vault.path(), repo_path.join("planning"))
        .expect("symlink planning into the vault");
    run_git(&repo_path, &["add", "-A"]);
    run_git(
        &repo_path,
        &["commit", "-q", "-m", "planning -> vault symlink"],
    );
    run_git(&repo_path, &["push", "-q", "origin", "main"]);

    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let chain = resolve_explicit_chain(vec![
        ("smoke-repo".to_string(), "SL.1".to_string()),
        ("smoke-repo".to_string(), "SL.2".to_string()),
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
    .expect("two-block chain over a symlinked planning/ should integrate cleanly");

    run_git(&repo_path, &["checkout", "-q", "main"]);
    let planning_path = repo_path.join("planning");
    let metadata =
        std::fs::symlink_metadata(&planning_path).expect("stat the planning path on main");
    assert!(
        metadata.file_type().is_symlink(),
        "planning must still be a symlink after the merge stage, not a real directory"
    );
    let target = std::fs::read_link(&planning_path).expect("read the planning symlink's target");
    assert_eq!(
        target,
        vault.path(),
        "planning symlink must still resolve into its original vault tempdir"
    );
}

// ── `EN.12.L` task 5: `[dispatch RECALL, block]` branches on the brain's answer ──
//
// AC4's load-bearing test. Drives the REAL `RecallNode` over the injectable
// `StubHttpGet` seam (never `RecallStubNode`, which `integrate.rs`'s own
// task-4 unit tests use to isolate the branching logic from the HTTP
// transport) through the actual `RECALL` registered workflow
// (`engine_core::workflows::recall`), end to end via
// `integrate_chain_with_dispatch` — the one public entry point that can
// integrate a mixed `[dispatch, block]` chain at all. Hermetic: the stub
// never contacts a live Brain, matching how `PersistToBrainNode` is tested
// and D23's Consequences.

/// A one-node stand-in that seeds a fixed, non-empty query string onto
/// `ctx.nodes` before the real `RecallNode` runs. `integrate_chain_with_
/// dispatch` hands every dispatch step's factory the same fixed `{}`
/// event (`integrate.rs`, `EN.12.L` task 4) — a real, unbound `RecallNode`
/// reading `ctx.event` directly would therefore always fail to resolve a
/// query before it ever reaches the `HttpGet` seam under test, regardless
/// of the stubbed recall body. This node stands in for whatever upstream
/// step supplies the query in a real deployment, so `RecallNode::
/// with_input_from` — its existing, already-shipped bound-query path — has
/// something to read, and the test actually exercises the real node's HTTP
/// round trip rather than being unable to reach it at all.
struct QuerySeedNode;

#[async_trait::async_trait]
impl engine_core::Node for QuerySeedNode {
    async fn process(
        &self,
        mut ctx: engine_contract::TaskContext,
    ) -> Result<engine_contract::TaskContext, engine_core::NodeError> {
        ctx.nodes.insert(
            "QuerySeed".to_string(),
            serde_json::json!({ "query": "stub-query" }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "QuerySeed"
    }
}

/// A `Dispatcher` with `RECALL_WORKFLOW_TYPE` registered against a real
/// `RecallNode` (bound to [`QuerySeedNode`]'s output, per its module doc)
/// whose `HttpGet` seam is `http_get` — a `StubHttpGet` in every case
/// below, so the gated suite never contacts a live Brain.
fn recall_dispatcher(http_get: Arc<dyn HttpGet>) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    let mut nodes = HashMap::new();
    nodes.insert(
        "QuerySeed".to_string(),
        engine_core::NodeConfig::new("QuerySeed", vec![RECALL_NODE_NAME.to_string()]),
    );
    nodes.insert(
        RECALL_NODE_NAME.to_string(),
        engine_core::NodeConfig::new(RECALL_NODE_NAME, vec![]),
    );
    let schema = engine_core::WorkflowSchema::new(RECALL_WORKFLOW_TYPE, "QuerySeed", nodes);
    dispatcher.register(
        schema.clone(),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(QuerySeedNode));
            registry.register(Box::new(
                RecallNode::new(BrainConfig::new("http://stub.invalid", None))
                    .with_input_from("QuerySeed")
                    .with_http_get(http_get.clone()),
            ));
            Ok(Workflow::new(registry, schema.clone()))
        }),
    );
    dispatcher
}

/// A recall-shaped body with `count` results, each carrying an ascending
/// `score` — mirrors the fixture shape `crates/engine-core/tests/fixtures/
/// recall_response.json` pins, field-for-field, so the stub body a real
/// `RecallResponse` deserializes cleanly.
fn recall_body(count: usize) -> serde_json::Value {
    let results: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            serde_json::json!({
                "doc_id": null,
                "file_path": "docs/example.md",
                "title": null,
                "section": null,
                "content": "example content",
                "score": 0.5 + i as f64 * 0.1,
                "via": "semantic",
            })
        })
        .collect();
    serde_json::json!({ "query": "stub-query", "count": count, "results": results })
}

fn recall_dispatch_step(repo: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: RECALL_WORKFLOW_TYPE.to_string(),
        kind: StepKind::Dispatch,
        ..Default::default()
    }
}

fn block_step(repo: &str, block_id: &str) -> ChainStep {
    ChainStep {
        repo: repo.to_string(),
        block_id: block_id.to_string(),
        kind: StepKind::Block,
        ..Default::default()
    }
}

type RecordingJournalSink = (
    Arc<dyn Fn(JournalRow) + Send + Sync>,
    Arc<Mutex<Vec<JournalRow>>>,
);

fn recording_journal_sink() -> RecordingJournalSink {
    let rows: Arc<Mutex<Vec<JournalRow>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = rows.clone();
    let sink: Arc<dyn Fn(JournalRow) + Send + Sync> = Arc::new(move |row: JournalRow| {
        recorder.lock().unwrap().push(row);
    });
    (sink, rows)
}

/// Run a `[dispatch RECALL, block]` chain once against `http_get`, returning
/// the `ExecutionOutcome`s and the recorded journal rows — the shared driver
/// for the two "different brain answer, different outcome" cases below, so
/// the only thing that differs between them is the stubbed body.
async fn run_recall_then_block_chain(
    http_get: Arc<dyn HttpGet>,
) -> Result<(Vec<ExecutionOutcome>, Vec<JournalRow>), IntegrateError> {
    let (_brain_root, _bare_root, registry, _repo_path) = single_repo_fixture("recall-repo");
    let (_planning_root, roadmap_dir) = fixture_roadmap_dir("en-12-l-task5");
    let runner = RecordingRunner::new();
    let flow_runner = runner.clone().into_runner();
    let admission = AdmissionGate::with_default_policy();
    let dispatcher = recall_dispatcher(http_get);
    let (sink, rows) = recording_journal_sink();
    let sink_fn = sink.clone();

    let chain = vec![
        recall_dispatch_step("recall-repo"),
        block_step("recall-repo", "B1"),
    ];

    let outcomes = integrate_chain_with_dispatch(
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
        Some(&move |row: JournalRow| sink_fn(row)),
        &dispatcher,
    )
    .await?;

    let recorded = rows.lock().unwrap().clone();
    Ok((outcomes, recorded))
}

/// The load-bearing AC4 test: the SAME `[dispatch RECALL, block]` chain,
/// run twice, differing ONLY in the stubbed recall body — an empty
/// `results` array vs. a one-result array. The second step runs in one
/// case and is skipped in the other; the `RecallConsulted` row's `branch`
/// field distinguishes the two runs and is recorded before the following
/// step's own journal row.
#[tokio::test]
async fn a_recall_then_block_chain_takes_a_different_branch_on_a_different_recall_body() {
    let empty_stub: Arc<dyn HttpGet> = Arc::new(StubHttpGet::succeeding(recall_body(0)));
    let (empty_outcomes, empty_rows) = run_recall_then_block_chain(empty_stub)
        .await
        .expect("an empty recall result must skip the next step, not fail the chain");

    assert!(
        empty_outcomes.is_empty(),
        "the skipped block step must produce no ExecutionOutcome: {empty_outcomes:?}"
    );
    let empty_recall_row = empty_rows
        .iter()
        .find(|r| r.kind == JournalDecisionKind::RecallConsulted)
        .expect("a RecallConsulted row must be present for the empty-result run");
    assert_eq!(
        empty_recall_row.detail.get("branch"),
        Some(&serde_json::json!("skipped-next")),
        "an empty recall result must journal branch: skipped-next: {empty_recall_row:?}"
    );
    assert!(
        !empty_rows.iter().any(|r| r.step == "B1"),
        "a skipped block step must have no journal row of its own: {empty_rows:?}"
    );

    let nonempty_stub: Arc<dyn HttpGet> = Arc::new(StubHttpGet::succeeding(recall_body(1)));
    let (nonempty_outcomes, nonempty_rows) = run_recall_then_block_chain(nonempty_stub)
        .await
        .expect("a non-empty recall result must let the next step run");

    assert_eq!(
        nonempty_outcomes.len(),
        1,
        "the block step must have run and produced one ExecutionOutcome: {nonempty_outcomes:?}"
    );
    let recall_row_idx = nonempty_rows
        .iter()
        .position(|r| r.kind == JournalDecisionKind::RecallConsulted)
        .expect("a RecallConsulted row must be present for the non-empty-result run");
    let block_row_idx = nonempty_rows
        .iter()
        .position(|r| r.step == "B1")
        .expect("the block step's own journal row must be present");
    assert!(
        recall_row_idx < block_row_idx,
        "the RecallConsulted row must be recorded before the following step's own row: \
         {nonempty_rows:?}"
    );
    assert_eq!(
        nonempty_rows[recall_row_idx].detail.get("branch"),
        Some(&serde_json::json!("ran-next")),
        "a non-empty recall result must journal branch: ran-next: {:?}",
        nonempty_rows[recall_row_idx]
    );

    // The one thing that differed between the two runs was the stubbed
    // recall body — everything else about the chain, the dispatcher
    // registration, and the driver was identical.
    assert_ne!(
        empty_recall_row.detail.get("branch"),
        nonempty_rows[recall_row_idx].detail.get("branch"),
        "the two runs must have taken different branches"
    );
}

/// A failing `RECALL` step (the real `RecallNode` over a `StubHttpGet` that
/// always errors, standing in for an unreachable Brain) bails the chain via
/// the existing dispatch error path — it must never be silently treated as
/// `count == 0`, so the following block step must never run OR be recorded
/// as skipped; the chain simply stops.
#[tokio::test]
async fn a_failing_recall_step_bails_the_chain_via_the_real_recall_node() {
    let failing_stub: Arc<dyn HttpGet> = Arc::new(StubHttpGet::failing("brain unreachable (stub)"));

    let err = run_recall_then_block_chain(failing_stub)
        .await
        .expect_err("a failing RecallNode must bail the chain, not skip past it");

    assert!(
        matches!(err, IntegrateError::Dispatch(_)),
        "a failing RECALL step must surface as a dispatch error: {err:?}"
    );
}
