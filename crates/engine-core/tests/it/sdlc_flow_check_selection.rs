//! Integration coverage for `EN.3.D` (check-selection parity): drives the
//! REAL [`TestTaskNode::process`] end to end with a recording
//! [`CommandRunner`] and asserts on the exact command strings that reach the
//! subprocess boundary, at both `test_depth` settings and under a per-task
//! `validation_commands` override.
//!
//! Unlike the pure `select_task_checks` unit tests in `task_loop.rs`'s own
//! `#[cfg(test)] mod tests`, this module proves the wiring actually reaches
//! `TestTaskNode::process` -> `run_checks` -> the injected runner, not just
//! the free function in isolation.
//!
//! Fixture harness mirrors this repo's own `planning/harness.json`: four
//! gating checks — `fmt`/`clippy` (no `fastCommand`), `test` (`command` +
//! `fastCommand`), and `build` (`perTask: false`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::policy::stamp_resolved_policy;
use engine_core::workflows::sdlc_flow::policy::{SdlcPolicy, TestDepth};
use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
use engine_core::workflows::sdlc_flow::task_loop::TestTaskNode;
use engine_core::workflows::sdlc_flow::{CommandOutput, CommandRunner};
use serde_json::json;

fn temp_worktree() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-check-selection-it-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    // Make the fixture an actual git worktree that HAS a change in it.
    // `TestTaskNode`'s write-verification guard now asks
    // `git status --porcelain` unconditionally (it can no longer be
    // short-circuited by an empty `modified_files` claim), so a bare
    // directory — where `git status` fails and reports nothing — would read
    // as "the implement work never reached this tree" and fail every
    // check-SELECTION test for a reason none of them are about. The seed
    // file is required: git does not report an empty directory, so
    // `git init` over an empty `planning/` still yields empty porcelain.
    std::fs::write(dir.join("planning").join(".worktree-seed"), "").unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .output()
        .expect("git init should succeed for the test worktree fixture");
    dir
}

/// Writes a `planning/harness.json` mirroring this repo's own: `fmt` +
/// `clippy` (no `fastCommand`), `test` (`command` + `fastCommand`), `build`
/// (`perTask: false`, excluded from every per-task run regardless of depth).
fn write_fixture_harness(worktree: &Path) {
    let harness = json!({
        "validation": {
            "checks": [
                { "name": "fmt", "kind": "command", "command": "cargo fmt --check", "gates": true },
                { "name": "clippy", "kind": "command", "command": "cargo clippy -- -D warnings", "gates": true },
                {
                    "name": "test",
                    "kind": "command",
                    "command": "cargo nextest run --workspace",
                    "fastCommand": "cargo nextest run --lib --workspace",
                    "gates": true
                },
                {
                    "name": "build",
                    "kind": "command",
                    "command": "cargo build --release",
                    "gates": true,
                    "perTask": false
                }
            ]
        }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// Builds a `ctx` carrying the durable `LoadTaskStateNode` state, a
/// `TaskQueueRouterNode`-shaped current-task snapshot, and a
/// `SetupWorktreeNode` worktree path — everything `TestTaskNode::process`
/// resolves from — then stamps `policy` under the resolved-policy identity
/// via the public `stamp_resolved_policy` seam.
fn ctx_for(worktree: &Path, task: &SDLCTask, policy: &SdlcPolicy) -> TaskContext {
    let mut state = SDLCState::new("fixture-spec");
    state.tasks = vec![task.clone()];

    let mut ctx = TaskContext {
        event: json!({ "spec_slug": "fixture-spec" }),
        nodes: std::collections::HashMap::new(),
        metadata: json!({}),
        node_runs: std::collections::HashMap::new(),
    };
    ctx.nodes.insert(
        "LoadTaskStateNode".to_string(),
        serde_json::to_value(&state).unwrap(),
    );
    ctx.nodes.insert(
        "TaskQueueRouterNode".to_string(),
        json!({
            "current_task_id": task.task_id,
            "title": task.title,
            "description": task.description,
            "acceptance_criteria": task.acceptance_criteria,
            "attempt_count": task.attempt_count,
            "max_attempts": task.max_attempts,
        }),
    );
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
    stamp_resolved_policy(&mut ctx, policy).expect("policy stamps");
    ctx
}

/// A recording [`CommandRunner`] that always succeeds and records every
/// `sh -c <command>` invocation's `<command>` string, in order.
///
/// `TestTaskNode`'s write-verification guard also probes the worktree with
/// a direct `git status --porcelain` on every run. That is not an `sh -c`
/// check invocation, so it is answered here — with a NON-EMPTY porcelain
/// line, modelling the ordinary case these tests are about, a task that did
/// write — and deliberately kept out of the recorded list.
fn recording_command_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();
    let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        if let Some(command) = args.get(1) {
            recorded_clone.lock().unwrap().push((*command).to_string());
        }
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    (runner, recorded)
}

fn policy_with_depth(depth: TestDepth) -> SdlcPolicy {
    SdlcPolicy {
        test_depth: depth,
        ..SdlcPolicy::default()
    }
}

#[tokio::test]
async fn full_depth_runs_command_and_excludes_per_task_false_build() {
    let worktree = temp_worktree();
    write_fixture_harness(&worktree);
    let task = SDLCTask::new(1, "One", "d1");
    let ctx = ctx_for(&worktree, &task, &policy_with_depth(TestDepth::Full));

    let (runner, recorded) = recording_command_runner();
    let node = TestTaskNode::new().with_runner(runner);
    let out = node.process(ctx).await.expect("process should succeed");

    assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![
            "cargo fmt --check",
            "cargo clippy -- -D warnings",
            "cargo nextest run --workspace",
        ]
    );
}

#[tokio::test]
async fn fast_depth_substitutes_fast_command_and_excludes_per_task_false_build() {
    let worktree = temp_worktree();
    write_fixture_harness(&worktree);
    let task = SDLCTask::new(1, "One", "d1");
    let ctx = ctx_for(&worktree, &task, &policy_with_depth(TestDepth::Fast));

    let (runner, recorded) = recording_command_runner();
    let node = TestTaskNode::new().with_runner(runner);
    let out = node.process(ctx).await.expect("process should succeed");

    assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![
            "cargo fmt --check",
            "cargo clippy -- -D warnings",
            "cargo nextest run --lib --workspace",
        ]
    );
}

#[tokio::test]
async fn task_validation_commands_override_is_the_only_thing_run_at_both_depths() {
    for depth in [TestDepth::Full, TestDepth::Fast] {
        let worktree = temp_worktree();
        write_fixture_harness(&worktree);
        let mut task = SDLCTask::new(1, "One", "d1");
        task.validation_commands = vec![
            "test -f docs/foo.md".to_string(),
            "grep -q bar docs/foo.md".to_string(),
        ];
        let ctx = ctx_for(&worktree, &task, &policy_with_depth(depth));

        let (runner, recorded) = recording_command_runner();
        let node = TestTaskNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec!["test -f docs/foo.md", "grep -q bar docs/foo.md"],
            "depth {depth:?} must not affect the override branch"
        );
        assert_eq!(
            out.nodes["TestTaskNode"]["check_source"],
            json!("task_validation_commands")
        );
        assert!(out.nodes["TestTaskNode"]["excluded_checks"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn telemetry_stamp_reflects_depth_source_and_excluded_checks() {
    let worktree = temp_worktree();
    write_fixture_harness(&worktree);
    let task = SDLCTask::new(1, "One", "d1");

    // Full depth, harness source: `build` excluded by `perTask: false`.
    let ctx = ctx_for(&worktree, &task, &policy_with_depth(TestDepth::Full));
    let (runner, _recorded) = recording_command_runner();
    let out = TestTaskNode::new()
        .with_runner(runner)
        .process(ctx)
        .await
        .expect("process should succeed");
    assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("full"));
    assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
    assert_eq!(
        out.nodes["TestTaskNode"]["excluded_checks"],
        json!(["build"])
    );

    // Fast depth, harness source: same exclusion, different depth stamp.
    let ctx = ctx_for(&worktree, &task, &policy_with_depth(TestDepth::Fast));
    let (runner, _recorded) = recording_command_runner();
    let out = TestTaskNode::new()
        .with_runner(runner)
        .process(ctx)
        .await
        .expect("process should succeed");
    assert_eq!(out.nodes["TestTaskNode"]["test_depth"], json!("fast"));
    assert_eq!(out.nodes["TestTaskNode"]["check_source"], json!("harness"));
    assert_eq!(
        out.nodes["TestTaskNode"]["excluded_checks"],
        json!(["build"])
    );

    // Override source: nothing excluded, `excluded_checks` empty.
    let mut override_task = SDLCTask::new(1, "One", "d1");
    override_task.validation_commands = vec!["true".to_string()];
    let ctx = ctx_for(
        &worktree,
        &override_task,
        &policy_with_depth(TestDepth::Full),
    );
    let (runner, _recorded) = recording_command_runner();
    let out = TestTaskNode::new()
        .with_runner(runner)
        .process(ctx)
        .await
        .expect("process should succeed");
    assert_eq!(
        out.nodes["TestTaskNode"]["check_source"],
        json!("task_validation_commands")
    );
    assert!(out.nodes["TestTaskNode"]["excluded_checks"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn no_harness_and_no_validation_commands_is_gating_harness_missing_failure() {
    let worktree = temp_worktree();
    // No `planning/harness.json` written.
    let task = SDLCTask::new(1, "One", "d1");
    let ctx = ctx_for(&worktree, &task, &policy_with_depth(TestDepth::Full));

    let (runner, recorded) = recording_command_runner();
    let node = TestTaskNode::new().with_runner(runner);
    let out = node.process(ctx).await.expect("process should succeed");

    assert!(
        recorded.lock().unwrap().is_empty(),
        "no command should be issued when there is nothing to validate against"
    );
    assert_eq!(out.nodes["TestTaskNode"]["all_passed"], false);
    let results = out.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .unwrap();
    assert!(results
        .iter()
        .any(|r| r["name"] == json!("harness-missing")));
}
