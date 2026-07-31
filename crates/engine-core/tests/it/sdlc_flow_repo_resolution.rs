//! Integration test for `EN.3.K` task 6 — the resolved-root plumbing actually
//! drives worktree creation and policy resolution end to end, not just in
//! `setup.rs`'s own unit tests.
//!
//! Builds one fixture "brain root" (a tempdir with a `brain.toml` naming two
//! repos, `alpha` and `beta`), each a real subdirectory carrying its own
//! `planning/<spec>/tasks.json` and a `planning/harness.json` with a
//! distinguishable `sdlc.policy.output_verbosity` value, and drives the real
//! `SetupWorktreeNode` / `resolve_policy_for_run_from` against it with a
//! recording stub [`engine_core::workflows::sdlc_flow::setup::CommandRunner`]
//! so no real `git` subprocess ever spawns.
//!
//! **Residual gap this block does NOT close** (documented, not fixed, per
//! `planning/EN.3.K-dispatch-target-resolution/tasks.md`'s notes section
//! item 2): the pre-spawn `repo`/`spec_slug` 422 validation added by tasks 4-5
//! lives in `engine-serve`'s `post_events` HTTP handler, not in
//! `SpecExistsRouterNode` or `SetupWorktreeNode` themselves. A CLI/in-tree run
//! that drives the graph directly — as this test does — never passes through
//! `post_events`, so it gets no pre-spawn spec-existence check. That gap is
//! out of scope for engine-core's node/registry layer; closing it (if ever)
//! belongs to whatever entry point replaces direct graph invocation for
//! CLI-driven runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::policy::{OutputVerbosity, PolicyConfigSource};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::sdlc_flow::setup::{
    resolve_policy_for_run_from, CommandOutput, CommandRunner, SetupWorktreeNode,
};

/// A recording [`CommandRunner`]: records every `(program, args, cwd)` triple
/// it is invoked with rather than shelling out, so tests can assert on the
/// exact cwd each git invocation received.
#[allow(clippy::type_complexity)]
fn recording_runner(
    status: i32,
) -> (
    CommandRunner,
    Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>>,
) {
    let calls: Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let runner: CommandRunner = Arc::new(move |program, args, cwd| {
        calls_clone.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| (*s).to_string()).collect(),
            cwd.to_path_buf(),
        ));
        Ok(CommandOutput {
            status,
            stdout: String::new(),
            stderr: if status == 0 {
                String::new()
            } else {
                "git failed".to_string()
            },
        })
    });
    (runner, calls)
}

/// Recorded calls minus `git rev-parse HEAD` — that invocation's cwd is
/// `worktree_path` itself (already derived from the resolved root, so it
/// follows automatically and isn't the thing this test needs to isolate).
fn non_rev_parse_calls(
    calls: &[(String, Vec<String>, PathBuf)],
) -> Vec<&(String, Vec<String>, PathBuf)> {
    calls
        .iter()
        .filter(|(_, args, _)| args.first().map(String::as_str) != Some("rev-parse"))
        .collect()
}

fn empty_context(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: std::collections::HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: std::collections::HashMap::new(),
    }
}

/// A distinguishable `sdlc.policy` value for `harness.json` — `verbosity`
/// alone lets test 2 prove the resolved policy actually came from the
/// expected repo's file rather than some process-wide default.
fn harness_json_with_verbosity(verbosity: &str) -> String {
    serde_json::json!({
        "sdlc": {
            "policy": {
                "output_verbosity": verbosity
            }
        }
    })
    .to_string()
}

/// A tempdir "brain root" containing a `brain.toml` naming two repos
/// (`alpha`, `beta`), each a real subdirectory with a `planning/<spec>/
/// tasks.json` and a `planning/harness.json` carrying a distinguishable
/// `sdlc.policy.output_verbosity` value (`alpha` -> `"terse"`, `beta` ->
/// `"verbose"`).
fn two_repo_brain_root(spec_slug: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");

    for (slug, verbosity) in [("alpha", "terse"), ("beta", "verbose")] {
        let repo_root = dir.path().join(slug);
        let planning_spec = repo_root.join("planning").join(spec_slug);
        std::fs::create_dir_all(&planning_spec).expect("mkdir planning/<spec>");
        std::fs::write(planning_spec.join("tasks.json"), "[]").expect("write tasks.json");
        std::fs::write(
            repo_root.join("planning").join("harness.json"),
            harness_json_with_verbosity(verbosity),
        )
        .expect("write harness.json");
    }

    std::fs::write(
        dir.path().join("brain.toml"),
        r#"
[[repos]]
slug = "alpha"
repo_path = "alpha"

[[repos]]
slug = "beta"
repo_path = "beta"
"#,
    )
    .expect("write brain.toml");

    dir
}

// --- Test 1: SetupWorktreeNode anchors worktree_path + every git cwd to the
// resolved root, never the process cwd, never the other repo. --------------

#[tokio::test]
async fn known_repo_anchors_worktree_creation_to_its_own_resolved_root() {
    let brain = two_repo_brain_root("my-spec");
    let registry = Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));
    let alpha_root = registry.resolve("alpha").expect("alpha resolves");
    let beta_root = registry.resolve("beta").expect("beta resolves");
    assert_ne!(
        alpha_root.canonicalize().unwrap(),
        beta_root.canonicalize().unwrap(),
        "fixture sanity: alpha and beta must be distinct roots"
    );

    let (runner, calls) = recording_runner(0);
    let node = SetupWorktreeNode::new()
        .with_runner(runner)
        .with_registry(registry);
    let ctx = empty_context(serde_json::json!({
        "spec_slug": "my-spec",
        "use_worktree": true,
        "repo": "alpha",
    }));

    let out = node.process(ctx).await.expect("setup should succeed");
    let result = out.nodes.get("SetupWorktreeNode").expect("output present");
    let stamped_worktree_path = PathBuf::from(
        result["worktree_path"]
            .as_str()
            .expect("worktree_path is a string"),
    );

    assert!(
        stamped_worktree_path.starts_with(&alpha_root),
        "stamped worktree_path {stamped_worktree_path:?} should be anchored under alpha's \
         resolved root {alpha_root:?}"
    );
    assert!(
        !stamped_worktree_path.starts_with(&beta_root),
        "stamped worktree_path must never fall under beta's root"
    );
    assert_ne!(
        stamped_worktree_path
            .canonicalize()
            .unwrap_or(stamped_worktree_path.clone()),
        std::env::current_dir().unwrap(),
        "stamped worktree_path must never be the process's own cwd"
    );

    let recorded = calls.lock().unwrap();
    let checked = non_rev_parse_calls(&recorded);
    assert!(!checked.is_empty(), "expected at least one git invocation");
    for (program, _, cwd) in checked.iter() {
        assert_eq!(*program, "git");
        assert_eq!(
            cwd.canonicalize().unwrap(),
            alpha_root.canonicalize().unwrap(),
            "every git invocation's cwd must be alpha's resolved root, never the process \
             cwd and never beta's root"
        );
    }
}

// --- Test 2: resolved policy genuinely follows the resolved root, per run,
// not a process-wide constant. -----------------------------------------------

#[test]
fn resolved_policy_follows_the_resolved_root_per_run() {
    let brain = two_repo_brain_root("my-spec");
    let registry = RepoRegistry::from_brain_root(brain.path()).expect("registry builds");
    let alpha_root = registry.resolve("alpha").expect("alpha resolves");
    let beta_root = registry.resolve("beta").expect("beta resolves");

    let ctx = empty_context(serde_json::json!({ "spec_slug": "my-spec" }));

    let alpha_policy =
        resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(alpha_root.clone()))
            .expect("alpha policy resolves");
    let beta_policy =
        resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(beta_root.clone()))
            .expect("beta policy resolves");

    assert_eq!(alpha_policy.output_verbosity, OutputVerbosity::Terse);
    assert_eq!(beta_policy.output_verbosity, OutputVerbosity::Verbose);
    assert_ne!(
        alpha_policy.output_verbosity, beta_policy.output_verbosity,
        "the same ctx resolved against two different roots must yield two different \
         policies — proof that PolicyConfigSource::Worktree is genuinely per-run"
    );
}

// --- Test 3: absent `repo` is byte-identical to today ------------------------

#[tokio::test]
async fn absent_repo_stamps_relative_worktree_path_and_dot_git_cwds() {
    let (runner, calls) = recording_runner(0);
    let node = SetupWorktreeNode::new().with_runner(runner);
    let ctx = empty_context(serde_json::json!({
        "spec_slug": "my-spec",
        "use_worktree": true,
    }));

    let out = node.process(ctx).await.expect("setup should succeed");
    let result = out.nodes.get("SetupWorktreeNode").expect("output present");
    assert_eq!(
        result["worktree_path"], "trees/sdlc/my-spec",
        "no `repo` on the event must stamp the exact relative worktree_path a run produced \
         before this block existed"
    );

    let recorded = calls.lock().unwrap();
    let checked = non_rev_parse_calls(&recorded);
    assert!(!checked.is_empty());
    for (_, _, cwd) in checked.iter() {
        assert_eq!(
            cwd,
            &PathBuf::from("."),
            "absent `repo` must keep every git cwd exactly \".\""
        );
    }
}

// --- Test 4: a path-shaped `repo` is rejected by the registry, never
// resolved, no git command runs. The security-property test. ---------------

#[tokio::test]
async fn path_shaped_repo_is_rejected_and_runs_no_git_command() {
    let brain = two_repo_brain_root("my-spec");
    let registry = Arc::new(RepoRegistry::from_brain_root(brain.path()).expect("registry builds"));

    for path_like_repo in ["/tmp", "../../etc", "alpha/../beta"] {
        let (runner, calls) = recording_runner(0);
        let node = SetupWorktreeNode::new()
            .with_runner(runner)
            .with_registry(registry.clone());
        let ctx = empty_context(serde_json::json!({
            "spec_slug": "my-spec",
            "use_worktree": true,
            "repo": path_like_repo,
        }));

        let err = node
            .process(ctx)
            .await
            .expect_err("a path-shaped repo value must never resolve");
        assert!(
            err.message.contains(path_like_repo),
            "error should name the offending value '{path_like_repo}': {}",
            err.message
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "no git command should have run for rejected repo value '{path_like_repo}'"
        );
    }
}
