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

use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::execute::FlowRunner;

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

    /// The marker filename this runner writes into the branch it cuts —
    /// exposed so a test can read it back after checking out a given
    /// branch without duplicating the literal string.
    #[allow(dead_code)] // wired up by later tasks in this spec (case (a))
    const MARKER_FILE: &'static str = "MARKER.txt";

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
                std::fs::write(invocation.repo_path.join(Self::MARKER_FILE), marker)
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
