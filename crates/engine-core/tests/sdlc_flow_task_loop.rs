//! Integration test for the `SDLC_FLOW` top-half workflow (`EN.3.A` task 6):
//! drives the assembled graph (`engine_core::workflows::sdlc_flow::graph`)
//! through setup → generate/load tasks → the full implement/test/triage/
//! review task loop, with **at least one** triggered retry back-edge, and
//! asserts the resulting `TaskContext`/`NodeRun` shape.
//!
//! Hermetic by construction: every seam that would otherwise spawn a real
//! `claude` process or shell out to a real subprocess is stubbed —
//! `ImplementTaskNode`/`ConsolidatedReviewNode` via `with_transport`,
//! `TestTaskNode`/`ConsolidatedReviewNode`'s git-diff/`SaveStateNode` via
//! `with_runner`. `SetupWorktreeNode` is replaced outright by a tiny
//! fixture node (rather than stubbing its runner) because its real
//! implementation hard-codes `worktree_path = "trees/{branch}"` relative to
//! the process's current directory — not overridable via the injectable
//! runner alone — and this test needs a controlled temp directory instead.
//! Every other node identity (`SpecExistsRouterNode`, `LoadTaskStateNode`,
//! `TaskQueueRouterNode`, `TriageTaskNode` (deterministic branch),
//! `TriageRouterNode`, `ReviewRouterNode`, `UpdateTaskStatusNode`,
//! `PatchDocsNode`) runs for real against fixture files written to the temp
//! worktree, exercising the same code path production traffic would use.
//!
//! **Retry-bail fix (EN.3.B task 5):** `TriageRouterNode`'s `RETRYABLE`
//! back-edge no longer routes straight to `ImplementTaskNode` — it routes
//! through `IncrementAttemptNode` first, which bumps the durable state's
//! `attempt_count`/`telemetry.total_attempts` before handing off to
//! `ImplementTaskNode` for the retry. The wiring lives in the real
//! `graph::schema()` (EN.3.B task 6) now, so this test uses it unamended.
//! For this single-task, one-retry fixture, `SDLCTelemetry.total_attempts`
//! now correctly reaches `2` (one `IncrementAttemptNode` bump for the retry,
//! one `UpdateTaskStatusNode` bump at the task's eventual PASS) and
//! `task.attempt_count` reaches `1`.
//!
//! **Bottom-half tail (EN.3.B task 6):** the declared graph no longer stops
//! at `PatchDocsNode` — it continues `PatchDocsNode -> WrapUpNode ->
//! PullRequestNode -> EmitStateNode`. This test stubs `PatchDocsNode`'s
//! model transport, drives the fixture event with `auto_pr: false` so
//! `PullRequestNode` no-ops without touching git/gh, and stubs
//! `EmitStateNode`'s runner so no real `mev` subprocess spawns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_code_rs::Outcome;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::pr::PullRequestNode;
use engine_core::workflows::sdlc_flow::setup::{
    resolve_policy_for_run, CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode,
    SpecExistsRouterNode,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, IncrementAttemptNode, ReviewRouterNode,
    SaveStateNode, TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode,
    UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use serde_json::json;

/// Replaces the real `SetupWorktreeNode` for this test: writes a controlled
/// temp-dir `worktree_path` (and a fixed `branch_name`) directly, so no real
/// `git worktree add` runs and downstream nodes (`LoadTaskStateNode`,
/// `TestTaskNode`, `SaveStateNode`) resolve paths under a directory this
/// test owns and can inspect. Still resolves + stamps
/// `RESOLVED_POLICY_IDENTITY` (EN.5.D task 8: downstream task-loop/wrap-up
/// nodes now read the stamp strictly, no per-node re-resolution or silent
/// `Default` fallback), the same as the real `SetupWorktreeNode::process`
/// does.
struct FixtureSetupNode {
    worktree_path: String,
}

#[async_trait::async_trait]
impl Node for FixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/fixture-spec",
            }),
        );

        let resolved_policy = resolve_policy_for_run(&ctx, Path::new(&self.worktree_path))?;
        engine_core::policy::stamp_resolved_policy(&mut ctx, &resolved_policy)?;

        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

fn stub_outcome(text: &str) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            claude_code_rs::parse::ModelUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.01,
            },
        )]
        .into_iter()
        .collect(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        structured_output: None,
    }
}

fn temp_worktree() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-task-loop-it-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning").join("fixture-spec")).unwrap();
    dir
}

/// Writes the fixture `tasks.json` (one PENDING task, `max_attempts = 2`)
/// under `<worktree>/planning/fixture-spec/` and a `harness.json` with a
/// single gating check (its actual command text is irrelevant — the
/// `TestTaskNode` runner is stubbed).
fn write_fixture_files(worktree: &Path) {
    let spec_dir = worktree.join("planning").join("fixture-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Implement the thing",
            "description": "Do the work",
            "acceptance_criteria": ["it works"],
            "max_attempts": 2,
        }
    ]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    let harness = json!({
        "validation": {
            "checks": [
                { "name": "tests", "kind": "command", "command": "does-not-matter", "gates": true }
            ]
        }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// Injected `TestTaskNode` runner: FAILs (`status = 1`) on its first
/// harness-check invocation, PASSes (`status = 0`) on every subsequent one —
/// drives the task's first attempt to fail and its retry to pass.
/// Special-cases `git status --porcelain` (the write-verification guard's
/// probe) to report `src/lib.rs` as modified, matching `build_workflow`'s
/// stubbed `ImplementTaskNode` claim, and does NOT consume a slot in the
/// fail/pass sequence — only the actual harness-check calls do.
fn fail_then_pass_runner() -> (CommandRunner, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();
    let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        let n = calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(CommandOutput {
            status: if n == 0 { 1 } else { 0 },
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    (runner, calls)
}

/// Injected runner for `git`-shaped calls (`SaveStateNode`'s
/// add/commit, `ConsolidatedReviewNode`'s diff) that always succeeds with
/// empty output — no real subprocess spawns.
fn noop_git_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Builds the `SDLC_FLOW` `Workflow` for this test: the declared graph from
/// [`graph::schema`], paired with a registry where the model/subprocess
/// nodes are stubbed and every other identity is the real implementation.
fn build_workflow(
    worktree: &Path,
    implement_calls: Arc<AtomicUsize>,
    test_runner: CommandRunner,
) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    let implement_transport_calls = implement_calls;
    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        move |_config, _prompt| {
            implement_transport_calls.fetch_add(1, Ordering::SeqCst);
            let outcome = stub_outcome(
                &json!({
                    "summary": "implemented",
                    "modified_files": ["src/lib.rs"],
                    "tests_added": ["it_works"],
                })
                .to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        },
    ))));

    registry.register(Box::new(TestTaskNode::new().with_runner(test_runner)));
    registry.register(Box::new(TriageTaskNode::new()));
    registry.register(Box::new(TriageRouterNode));

    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(noop_git_runner())
            .with_transport(Arc::new(|_config, _prompt| {
                let outcome = stub_outcome(
                    &json!({ "verdict": "PASS", "summary": "looks good", "issues": [] })
                        .to_string(),
                );
                Box::pin(async move { Ok(outcome) })
            })),
    ));

    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new().with_runner(noop_git_runner()),
    ));
    registry.register(Box::new(PatchDocsNode::new().with_transport(Arc::new(
        |_config, _prompt| {
            let outcome = stub_outcome(
                &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        },
    ))));
    registry.register(Box::new(IncrementAttemptNode));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(PullRequestNode::new()));
    registry.register(Box::new(
        EmitStateNode::new().with_runner(noop_git_runner()),
    ));

    let schema = graph::schema();

    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

#[tokio::test]
async fn full_task_loop_triggers_retry_back_edge_and_completes() {
    let worktree = temp_worktree();
    write_fixture_files(&worktree);

    let implement_calls = Arc::new(AtomicUsize::new(0));
    let (test_runner, test_calls) = fail_then_pass_runner();

    let workflow = build_workflow(&worktree, implement_calls.clone(), test_runner);

    let event = json!({ "spec_slug": "fixture-spec", "auto_pr": false });
    let snapshots: Arc<Mutex<Vec<TaskContext>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    // `SaveStateNode` and `WrapUpNode` write to the *same* on-disk path
    // (D31-committed `sdlc/sdlc-flow-state.json`), and `WrapUpNode` runs
    // after `SaveStateNode` in every full run — so reading the file only
    // once, after `workflow.run` returns, would observe `WrapUpNode`'s
    // final (review/docs-populated) write, not `SaveStateNode`'s own
    // partial-write shape. Capture the file's content the first time
    // `SaveStateNode` reports SUCCESS instead, mid-run, before anything
    // downstream can overwrite it.
    let save_state_snapshot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let save_state_snapshot_handle = save_state_snapshot.clone();
    let on_progress: engine_core::OnProgress<'_> = Box::new(move |ctx: &TaskContext| {
        snapshots_handle.lock().unwrap().push(ctx.clone());
        let mut captured = save_state_snapshot_handle.lock().unwrap();
        if captured.is_none() {
            let is_success = ctx
                .node_runs
                .get("SaveStateNode")
                .map(|run| run.status == NodeRunStatus::Success)
                .unwrap_or(false);
            if is_success {
                if let Some(saved_to) = ctx
                    .nodes
                    .get("SaveStateNode")
                    .and_then(|result| result["saved_to"].as_str())
                {
                    if let Ok(content) = std::fs::read_to_string(saved_to) {
                        *captured = Some(content);
                    }
                }
            }
        }
    });

    let final_ctx = workflow
        .run(event, on_progress)
        .await
        .expect("workflow run should not error");

    // --- The retry back-edge actually fired -------------------------------
    assert_eq!(
        implement_calls.load(Ordering::SeqCst),
        2,
        "ImplementTaskNode should run once for the failing attempt and once \
         more for the retry"
    );
    assert!(
        test_calls.load(Ordering::SeqCst) >= 2,
        "TestTaskNode should run at least twice (fail then pass)"
    );

    let snapshots = snapshots.lock().unwrap();
    let implement_running_snapshots = snapshots
        .iter()
        .filter(|ctx| {
            ctx.node_runs
                .get("ImplementTaskNode")
                .map(|run| run.status == NodeRunStatus::Running)
                .unwrap_or(false)
        })
        .count();
    assert!(
        implement_running_snapshots >= 2,
        "expected at least two RUNNING snapshots for ImplementTaskNode (the \
         retry back-edge re-entering it), got {implement_running_snapshots}"
    );

    // --- Final task + telemetry state --------------------------------------
    let update_result = final_ctx
        .nodes
        .get("UpdateTaskStatusNode")
        .expect("UpdateTaskStatusNode should have run");
    let state: engine_core::workflows::sdlc_flow::schema::SDLCState =
        serde_json::from_value(update_result.clone()).expect("state should deserialize");

    assert_eq!(state.tasks.len(), 1);
    assert_eq!(
        state.tasks[0].status,
        engine_core::workflows::sdlc_flow::schema::SDLCTaskStatus::Done
    );
    assert_eq!(state.telemetry.tasks_passed, 1);
    assert_eq!(state.telemetry.tasks_failed, 0);
    // Retry-bail fix (EN.3.B task 5): IncrementAttemptNode bumps
    // attempt_count/total_attempts once for the retry back-edge, then
    // UpdateTaskStatusNode bumps total_attempts again at the eventual PASS —
    // see the module doc's "Retry-bail fix" note.
    assert_eq!(state.tasks[0].attempt_count, 1);
    assert_eq!(state.telemetry.total_attempts, 2);

    // --- The walk reaches the full bottom-half tail -------------------------
    let patch_docs_run = final_ctx
        .node_runs
        .get("PatchDocsNode")
        .expect("PatchDocsNode should be present in node_runs");
    assert_eq!(patch_docs_run.status, NodeRunStatus::Success);

    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode should have stamped a result");
    assert!(wrap_up_result.get("log_entry").is_some());
    assert!(wrap_up_result.get("report").is_some());
    assert!(wrap_up_result.get("status_suggestion").is_some());

    let pr_result = final_ctx
        .nodes
        .get("PullRequestNode")
        .expect("PullRequestNode should have stamped a result");
    assert_eq!(pr_result["skipped"], json!(true));
    assert_eq!(pr_result["pr_url"], json!(null));

    let emit_state_result = final_ctx
        .nodes
        .get("EmitStateNode")
        .expect("EmitStateNode should have stamped a result");
    assert_eq!(emit_state_result["emitted"], json!(true));

    // --- SaveStateNode's per-task write: D31-committed path + partial shape
    // (D10) — lands under `sdlc/`, `tasks` is an object, and `review`/
    // `docs`/`pr` are all absent since `SaveStateNode` runs before
    // `ConsolidatedReviewNode`'s per-task cycle even gets to `PatchDocsNode`/
    // `PullRequestNode` in the *final* pass (`UpdateTaskStatusNode ->
    // SaveStateNode` closes the loop, then `TaskQueueRouterNode` finds no
    // more pending tasks and moves on to `PatchDocsNode`).
    let save_state_result = final_ctx
        .nodes
        .get("SaveStateNode")
        .expect("SaveStateNode should have stamped a result");
    let saved_to = save_state_result["saved_to"].as_str().unwrap();
    assert!(saved_to.ends_with("sdlc/sdlc-flow-state.json"));

    let captured_content = save_state_snapshot
        .lock()
        .unwrap()
        .clone()
        .expect("SaveStateNode's write should have been captured mid-run");
    let state_json: serde_json::Value = serde_json::from_str(&captured_content)
        .expect("SaveStateNode's on-disk file should parse as JSON");
    assert!(state_json["tasks"].is_object());
    assert_eq!(state_json["status"], json!("done"));
    assert!(state_json["review"].is_null());
    assert!(state_json["docs"].is_null());
    assert!(state_json["pr"].is_null());
    assert!(state_json["bail_reason"].is_null());
    assert!(state_json["branch"].as_str().is_some());
    assert!(state_json["started_at"].as_str().is_some());
    assert!(state_json["updated_at"].as_str().is_some());

    // --- Parity-shape assertions (contract §6) -----------------------------
    for identity in [
        "SetupWorktreeNode",
        "SpecExistsRouterNode",
        "LoadTaskStateNode",
        "TaskQueueRouterNode",
        "ImplementTaskNode",
        "TestTaskNode",
        "TriageTaskNode",
        "TriageRouterNode",
        "IncrementAttemptNode",
        "ConsolidatedReviewNode",
        "ReviewRouterNode",
        "UpdateTaskStatusNode",
        "SaveStateNode",
        "PatchDocsNode",
        "WrapUpNode",
        "PullRequestNode",
        "EmitStateNode",
    ] {
        let run = final_ctx
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("expected a NodeRun for '{identity}'"));
        assert_eq!(
            run.status,
            NodeRunStatus::Success,
            "expected '{identity}' to have run to SUCCESS"
        );
        assert!(
            run.started_at.is_some() && run.completed_at.is_some(),
            "'{identity}' should have timing stamped"
        );
    }

    // Model nodes carry usage; deterministic nodes don't.
    for identity in [
        "ImplementTaskNode",
        "ConsolidatedReviewNode",
        "PatchDocsNode",
    ] {
        let run = final_ctx.node_runs.get(identity).unwrap();
        assert!(
            run.usage.is_some(),
            "'{identity}' is a model node and should carry usage"
        );
    }
    for identity in [
        "SetupWorktreeNode",
        "SpecExistsRouterNode",
        "LoadTaskStateNode",
        "TaskQueueRouterNode",
        "TestTaskNode",
        "TriageTaskNode",
        "TriageRouterNode",
        "IncrementAttemptNode",
        "ReviewRouterNode",
        "UpdateTaskStatusNode",
        "SaveStateNode",
        "WrapUpNode",
        "PullRequestNode",
        "EmitStateNode",
    ] {
        let run = final_ctx.node_runs.get(identity).unwrap();
        assert!(
            run.usage.is_none(),
            "'{identity}' is deterministic and should not carry usage"
        );
    }

    // GenerateTasksNode never runs on this fixture (SpecExistsRouterNode
    // finds tasks.json already present) — still declared/PENDING.
    let generate_run = final_ctx
        .node_runs
        .get("GenerateTasksNode")
        .expect("GenerateTasksNode is declared and seeded PENDING");
    assert_eq!(generate_run.status, NodeRunStatus::Pending);

    // Each executed node's own output carries the Python-parity `nodes[id]`
    // shape used throughout this module (a JSON object, not a bare scalar).
    for identity in [
        "TaskQueueRouterNode",
        "TestTaskNode",
        "TriageTaskNode",
        "ConsolidatedReviewNode",
        "UpdateTaskStatusNode",
    ] {
        let output = final_ctx
            .nodes
            .get(identity)
            .unwrap_or_else(|| panic!("expected '{identity}' output in ctx.nodes"));
        assert!(
            output.is_object(),
            "'{identity}' output should be a JSON object"
        );
    }
}

#[tokio::test]
async fn workflow_terminal_stub_reachable_when_no_pending_tasks() {
    // A degenerate fixture (task already DONE) exercises the "no pending
    // task" branch straight through PatchDocsNode -> WrapUpNode ->
    // PullRequestNode -> EmitStateNode without ever touching the task-loop
    // model nodes.
    let worktree = temp_worktree();
    let spec_dir = worktree.join("planning").join("fixture-spec");
    let sdlc_dir = spec_dir.join("sdlc");
    std::fs::create_dir_all(&sdlc_dir).unwrap();
    // D31-committed shape (D10): `tasks` is an object keyed by task id, not
    // an array, and the file lives under the `sdlc/` subdirectory.
    let state = json!({
        "spec_slug": "fixture-spec",
        "branch": "sdlc/fixture-spec",
        "worktree_path": worktree.to_string_lossy(),
        "started_at": "2026-07-25T00:00:00Z",
        "updated_at": "2026-07-25T00:00:00Z",
        "status": "done",
        "current_task": 1,
        "tasks": {
            "1": { "task_id": 1, "title": "Done already", "description": "d", "status": "done" }
        },
        "bail_reason": null,
    });
    std::fs::write(
        sdlc_dir.join("sdlc-flow-state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();

    let implement_calls = Arc::new(AtomicUsize::new(0));
    let (test_runner, _test_calls) = fail_then_pass_runner();
    let workflow = build_workflow(&worktree, implement_calls.clone(), test_runner);

    let event = json!({ "spec_slug": "fixture-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(implement_calls.load(Ordering::SeqCst), 0);
    let patch_docs_run = final_ctx.node_runs.get("PatchDocsNode").unwrap();
    assert_eq!(patch_docs_run.status, NodeRunStatus::Success);
    let task_queue_run = final_ctx.node_runs.get("TaskQueueRouterNode").unwrap();
    assert_eq!(task_queue_run.status, NodeRunStatus::Success);
    // No task dispatched -> TaskQueueRouterNode writes no output.
    assert!(!final_ctx.nodes.contains_key("TaskQueueRouterNode"));
}
