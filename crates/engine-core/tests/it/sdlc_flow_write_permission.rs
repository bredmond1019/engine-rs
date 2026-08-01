//! Hermetic integration tests for the write-verification guard
//! (`TestTaskNode`) and the diff-base pin (`TriageTaskNode`'s
//! trivial-diff classification, which shares the working-tree-vs-`HEAD`
//! diff basis `ConsolidatedReviewNode` uses) — EN ticket
//! `ticket-sdlc-flow-write-permission` task 7, re-pinned to `HEAD` by
//! `ticket-commit-task-work-real-diffs` task 2.
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
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
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

/// The trivial-diff classifier (which shares `ConsolidatedReviewNode`'s
/// diff basis) diffs the WORKING TREE against `HEAD`, preceded by an
/// intent-to-add pass so untracked files are counted. A `base_sha` stamped
/// by `SetupWorktreeNode` is run metadata only and must NOT become a diff
/// base — nothing in a run commits the implementer's code until
/// `SaveStateNode` runs on the pass path, so `<base_sha>..HEAD` was empty on
/// every run and every task classified as trivial.
#[tokio::test]
async fn diff_base_is_head_regardless_of_a_stamped_base_sha() {
    for stamped in [Some("abc1234"), None] {
        let worktree = temp_worktree();
        let mut ctx = ctx_with_task(&worktree, stamped);
        // Bypass a real TestTaskNode run: stamp a passing result directly so
        // TriageTaskNode's PASS branch (the one that calls the trivial-diff
        // classifier) is reached deterministically.
        ctx.nodes.insert(
            "TestTaskNode".to_string(),
            json!({ "all_passed": true, "check_results": [], "failure_summary": "" }),
        );

        let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let runner: CommandRunner = Arc::new(move |_program, args, _cwd| {
            seen_clone
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
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
            *seen.lock().unwrap(),
            vec![
                vec!["add".to_string(), "-N".to_string(), "-A".to_string()],
                vec![
                    "diff".to_string(),
                    "--numstat".to_string(),
                    "HEAD".to_string()
                ],
            ],
            "base_sha stamp = {stamped:?}"
        );
    }
}

// --- PatchDocsNode cwd scoping (ticket-policy-path-generate-docs-nodes) -----

/// `PatchDocsNode` is the OTHER node `graph.rs::registry()` hands
/// `agentic_write_config` — `dangerously_skip_permissions: true` plus a full
/// file-write grant. Until this ticket it set no `Config.cwd` at all, so
/// under `bastion serve` (whose cwd is the primary checkout, on `main`) it
/// would have written its doc patches into the wrong tree entirely. These
/// two tests pin the closure end to end through the same
/// `agentic_write_config` the registry actually uses.
#[tokio::test]
async fn patch_docs_node_with_the_write_grant_is_scoped_to_the_worktree() {
    use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
    use engine_core::workflows::sdlc_flow::graph::agentic_write_config;
    use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;

    let worktree = temp_worktree();
    let captured: Arc<Mutex<Option<claude_code_rs::Config>>> = Arc::new(Mutex::new(None));
    let captured_clone = captured.clone();

    let transport: engine_core::workflows::sdlc_flow::ModelTransport =
        Arc::new(move |config: claude_code_rs::Config, _prompt: String| {
            *captured_clone.lock().unwrap() = Some(config.clone());
            let outcome = claude_code_rs::Outcome {
                cost_usd: 0.0,
                usage: claude_code_rs::parse::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                model_usage: std::collections::BTreeMap::new(),
                text: json!({ "summary": "ok", "files_patched": [] }).to_string(),
                is_error: false,
                api_error_status: None,
                structured_output: None,
            };
            Box::pin(async move { Ok(outcome) })
        });

    let mut ctx = empty_ctx();
    ctx.nodes.insert(
        "ResolvedPolicy".to_string(),
        serde_json::to_value(SdlcPolicy::default()).expect("policy serializes"),
    );
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );

    let node = PatchDocsNode::new()
        .with_config(agentic_write_config("claude-sonnet-4-5"))
        .with_transport(transport);
    node.process(ctx).await.expect("process should succeed");

    let config = captured.lock().unwrap().clone().expect("transport called");
    // The write grant survived...
    assert!(config.dangerously_skip_permissions);
    assert_eq!(config.disallowed_tools, vec!["Bash".to_string()]);
    // ...and it is now pointed at the run's worktree, not the process cwd.
    assert_eq!(config.cwd, Some(worktree));
}

/// The fail-closed half: with no `SetupWorktreeNode` stamp the node errors
/// rather than running a skip-permissions session against an ambient cwd.
#[tokio::test]
async fn patch_docs_node_refuses_to_run_unscoped() {
    use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
    use engine_core::workflows::sdlc_flow::graph::agentic_write_config;
    use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;

    let mut ctx = empty_ctx();
    ctx.nodes.insert(
        "ResolvedPolicy".to_string(),
        serde_json::to_value(SdlcPolicy::default()).expect("policy serializes"),
    );

    let node = PatchDocsNode::new().with_config(agentic_write_config("claude-sonnet-4-5"));
    let err = node
        .process(ctx)
        .await
        .expect_err("an unscoped skip-permissions writer must not run");
    assert!(
        err.to_string().contains("worktree_path"),
        "unexpected error: {err}"
    );
}
