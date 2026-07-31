//! Hermetic integration tests for the write-verification guard
//! (`TestTaskNode`) and the diff-base pin (`TriageTaskNode`'s
//! trivial-diff classification, which shares the same `<base_sha>..HEAD`
//! / `main..HEAD` range builder `ConsolidatedReviewNode` uses) — EN ticket
//! `ticket-sdlc-flow-write-permission` task 7.
//!
//! Every seam that would otherwise spawn a real subprocess or `claude`
//! session is stubbed: `CommandRunner` closures assert their own `argv` and
//! return canned output, and no `ModelTransport` is exercised at all (the
//! scenarios here only touch the deterministic `TestTaskNode`/
//! `TriageTaskNode`/`TriageRouterNode` path). Nodes are driven directly
//! against a hand-built `TaskContext` rather than through the full assembled
//! graph, mirroring the Testing Strategy in
//! `planning/ticket-sdlc-flow-write-permission/tasks.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use engine_contract::TaskContext;
use engine_core::node::Node;
use engine_core::routing::Router;
use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
use engine_core::workflows::sdlc_flow::setup::{CommandOutput, CommandRunner};
use engine_core::workflows::sdlc_flow::task_loop::{
    TestTaskNode, TriageRouterNode, TriageTaskNode,
};
use serde_json::json;

// --- fixtures --------------------------------------------------------------

/// A fresh temp directory, unique per call (no real `git worktree`
/// involved — `TestTaskNode`/`TriageTaskNode` only need a path that exists
/// so `std::fs`/the stubbed `CommandRunner` don't choke on a missing cwd).
///
/// **EN.3.D task 5:** also writes a `planning/harness.json` declaring an
/// EMPTY `validation.checks` array. Before this block, `TestTaskNode`
/// silently auto-passed a worktree with no harness *file* at all; task 4
/// made a genuinely missing harness a gating `harness-missing` failure. An
/// empty-but-present harness is different: the file exists (so the
/// gating branch never fires) and it declares zero checks, so
/// `check_results` stays empty and `all_passed` stays true when nothing
/// else fails — exactly what these write-verification-guard tests need to
/// keep isolating the guard's behavior rather than the (now-correct)
/// no-harness failure path.
fn temp_worktree() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-write-permission-test-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning")).expect("create temp worktree dir");

    let harness = json!({ "validation": { "checks": [] } });
    std::fs::write(
        dir.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();

    dir
}

fn empty_ctx() -> TaskContext {
    TaskContext {
        event: json!({}),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    }
}

/// Build a ctx carrying `SetupWorktreeNode` (worktree path + optional
/// `base_sha`), `LoadTaskStateNode` (one PENDING task, default
/// `attempt_count = 0` / `max_attempts = 3`), and `TaskQueueRouterNode`
/// (dispatching that task) — the minimum a real `SetupWorktreeNode` ->
/// `GenerateTasksNode`/`LoadTaskStateNode` -> `TaskQueueRouterNode` walk
/// would have produced by the time `ImplementTaskNode`/`TestTaskNode` run.
fn ctx_with_task(worktree: &std::path::Path, base_sha: Option<&str>) -> TaskContext {
    let mut ctx = empty_ctx();

    let mut setup = json!({ "worktree_path": worktree.to_string_lossy() });
    if let Some(sha) = base_sha {
        setup["base_sha"] = json!(sha);
    }
    ctx.nodes.insert("SetupWorktreeNode".to_string(), setup);
    // Required since task 8's strict `resolved_policy_strict` read (no more
    // per-node re-resolution or silent `Default` fallback) — mirrors what a
    // real run's dispatch/`SetupWorktreeNode` stamp would have seeded.
    ctx.nodes.insert(
        engine_core::policy::RESOLVED_POLICY_IDENTITY.to_string(),
        serde_json::to_value(engine_core::workflows::sdlc_flow::policy::SdlcPolicy::default())
            .expect("SdlcPolicy serializes"),
    );

    let mut state = SDLCState::new("write-permission-fixture");
    state
        .tasks
        .push(SDLCTask::new(1, "Do the thing", "fixture task"));
    ctx.nodes.insert(
        "LoadTaskStateNode".to_string(),
        serde_json::to_value(&state).expect("SDLCState serializes"),
    );

    ctx.nodes.insert(
        "TaskQueueRouterNode".to_string(),
        json!({
            "current_task_id": 1,
            "title": "Do the thing",
            "description": "fixture task",
        }),
    );

    ctx
}

/// Stamp an `ImplementTaskNode` output claiming `modified_files`, mirroring
/// what the (stubbed, in production write-permitted) implement step writes.
fn with_implement_claim(mut ctx: TaskContext, modified_files: &[&str]) -> TaskContext {
    ctx.nodes.insert(
        "ImplementTaskNode".to_string(),
        json!({
            "summary": "did the thing",
            "modified_files": modified_files,
            "tests_added": [],
        }),
    );
    ctx
}

/// A `CommandRunner` stub that answers `git status --porcelain` with
/// `status_lines` and every other command with empty, successful output.
fn porcelain_runner(status_lines: &'static str) -> CommandRunner {
    Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            Ok(CommandOutput {
                status: 0,
                stdout: status_lines.to_string(),
                stderr: String::new(),
            })
        } else {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    })
}

// --- write-verification guard: mismatch routes into the retry path --------

/// `ImplementTaskNode` claims `modified_files` that never show up in `git
/// status --porcelain`: `TestTaskNode` synthesizes a failed
/// `write-verification` check, and that failure is not swallowed — it
/// drives `TriageTaskNode` to `RETRYABLE` (attempt 0 of 3, still under
/// budget) and `TriageRouterNode` routes to the real retry back-edge target
/// `IncrementAttemptNode`, exactly like a harness-check failure would.
#[tokio::test]
async fn guard_mismatch_fails_check_and_routes_to_retry() {
    let worktree = temp_worktree();
    let ctx = with_implement_claim(ctx_with_task(&worktree, None), &["src/lib.rs"]);

    let test_node = TestTaskNode::new().with_runner(porcelain_runner(""));
    let ctx = test_node
        .process(ctx)
        .await
        .expect("TestTaskNode should not error");

    assert_eq!(ctx.nodes["TestTaskNode"]["all_passed"], false);
    let results = ctx.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .expect("check_results is an array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], "write-verification");
    assert_eq!(results[0]["passed"], false);
    assert!(ctx.nodes["TestTaskNode"]["failure_summary"]
        .as_str()
        .unwrap()
        .contains("write-verification"));

    let triage_node = TriageTaskNode::new();
    let ctx = triage_node
        .process(ctx)
        .await
        .expect("TriageTaskNode should not error");
    assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");

    let router = TriageRouterNode;
    assert_eq!(
        router.route(&ctx),
        Some("IncrementAttemptNode".to_string()),
        "a write-verification mismatch must flow through the normal retry back-edge"
    );
}

// --- write-verification guard: match passes through ------------------------

/// A claimed file that DOES show up in `git status --porcelain` never trips
/// the guard: `TestTaskNode` passes through with no synthesized check, and
/// (with no `harness.json` present) the task overall passes and
/// `TriageTaskNode` reaches `PASS`.
#[tokio::test]
async fn guard_match_passes_through_to_triage_pass() {
    let worktree = temp_worktree();
    let ctx = with_implement_claim(ctx_with_task(&worktree, None), &["src/lib.rs"]);

    let test_node = TestTaskNode::new().with_runner(porcelain_runner(" M src/lib.rs\n"));
    let ctx = test_node
        .process(ctx)
        .await
        .expect("TestTaskNode should not error");

    assert_eq!(ctx.nodes["TestTaskNode"]["all_passed"], true);
    assert!(ctx.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .unwrap()
        .is_empty());

    let triage_node = TriageTaskNode::new().with_runner(porcelain_runner(""));
    let ctx = triage_node
        .process(ctx)
        .await
        .expect("TriageTaskNode should not error");
    assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "PASS");
}

// --- write-verification guard: empty claim never trips ---------------------

/// A genuinely no-op task (`modified_files: []`) never trips the guard, even
/// against a completely clean worktree with nothing in `git status`.
#[tokio::test]
async fn empty_claim_never_trips_the_guard() {
    let worktree = temp_worktree();
    let ctx = with_implement_claim(ctx_with_task(&worktree, None), &[]);

    let test_node = TestTaskNode::new().with_runner(porcelain_runner(""));
    let ctx = test_node
        .process(ctx)
        .await
        .expect("TestTaskNode should not error");

    assert_eq!(ctx.nodes["TestTaskNode"]["all_passed"], true);
    assert!(ctx.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .unwrap()
        .is_empty());
}

// --- diff-base pin -----------------------------------------------------

/// When `SetupWorktreeNode` stamped a `base_sha` (the SHA captured at
/// worktree-setup time), `TriageTaskNode`'s trivial-diff classification
/// (which shares the diff-range builder `ConsolidatedReviewNode` also uses)
/// diffs `<base_sha>..HEAD`, not `main..HEAD` — so a `main` that advances
/// mid-run can't misreport unrelated commits as reversions.
#[tokio::test]
async fn diff_base_pins_to_captured_sha_when_present() {
    let worktree = temp_worktree();
    let mut ctx = ctx_with_task(&worktree, Some("abc1234"));
    // Bypass a real TestTaskNode run: stamp a passing result directly so
    // TriageTaskNode's PASS branch (the one that calls the trivial-diff
    // classifier) is reached deterministically.
    ctx.nodes.insert(
        "TestTaskNode".to_string(),
        json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
    );

    let seen_args: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let seen_args_clone = seen_args.clone();
    let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
        *seen_args_clone.lock().unwrap() = Some(args.iter().map(|s| s.to_string()).collect());
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });

    let triage_node = TriageTaskNode::new().with_runner(runner);
    let ctx = triage_node
        .process(ctx)
        .await
        .expect("TriageTaskNode should not error");

    assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "PASS");
    assert_eq!(
        seen_args.lock().unwrap().as_deref(),
        Some(vec![
            "diff".to_string(),
            "--numstat".to_string(),
            "abc1234..HEAD".to_string()
        ])
        .as_deref()
    );
}

/// With no `base_sha` captured (e.g. `SetupWorktreeNode`'s `git rev-parse`
/// failed, or a ctx driven directly in a unit test), the diff range falls
/// back to `main..HEAD`.
#[tokio::test]
async fn diff_base_falls_back_to_main_when_absent() {
    let worktree = temp_worktree();
    let mut ctx = ctx_with_task(&worktree, None);
    ctx.nodes.insert(
        "TestTaskNode".to_string(),
        json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
    );

    let seen_args: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(None));
    let seen_args_clone = seen_args.clone();
    let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
        *seen_args_clone.lock().unwrap() = Some(args.iter().map(|s| s.to_string()).collect());
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });

    let triage_node = TriageTaskNode::new().with_runner(runner);
    let ctx = triage_node
        .process(ctx)
        .await
        .expect("TriageTaskNode should not error");

    assert_eq!(ctx.nodes["TriageTaskNode"]["verdict"], "PASS");
    assert_eq!(
        seen_args.lock().unwrap().as_deref(),
        Some(vec![
            "diff".to_string(),
            "--numstat".to_string(),
            "main..HEAD".to_string()
        ])
        .as_deref()
    );
}
