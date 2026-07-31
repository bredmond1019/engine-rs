//! End-to-end integration test for the full assembled `SDLC_FLOW` workflow
//! (`EN.3.B` task 7): drives the graph from `SetupWorktreeNode` all the way
//! through the bottom-half tail — `... -> PatchDocsNode -> WrapUpNode ->
//! PullRequestNode -> EmitStateNode` — with every model/subprocess seam
//! stubbed, mirroring the seam-injection style of `sdlc_flow_task_loop.rs`
//! (`EN.3.A`/`EN.3.B` task 6).
//!
//! Hermetic by construction: no real `claude`, `git`, `gh`, or `mev`
//! subprocess is ever spawned. `SetupWorktreeNode` is replaced outright by a
//! tiny fixture node (as in `sdlc_flow_task_loop.rs`) so the run resolves
//! paths under a controlled temp directory instead of the real
//! `trees/{branch}` layout.
//!
//! Covers the spec's acceptance criteria for task 7:
//! (a) a happy-path run (all tasks pass) reaches the tail and produces the
//!     `WrapUpNode` / `PullRequestNode` / `EmitStateNode` results, in both
//!     the `auto_pr: false` (no-op) and `auto_pr: true` (stub `gh` returns a
//!     URL) shapes;
//! (b) a never-passing task bails at `max_attempts` and the run still
//!     reaches the wrap-up tail (ties task 5's retry-bail fix into the whole
//!     assembled graph, not just the task-loop unit tests);
//! (c) the run's durable `EventsRow` mapping (contract §4, D44/D45) is
//!     producible from the final `TaskContext` and round-trips through
//!     `serde_json`, exercising the same shape `engine-store`'s Postgres
//!     write path persists — see `crates/engine-store/tests/postgres_round_trip.rs`
//!     for the store-level counterpart this reuses the assertion style from.
//!
//! **Amendment (EN.3.B task 7):** this hermetic suite covers every item
//! above with stubbed transports/runners. The master-plan's "real block
//! observed by bastion live" / "byte-identical events row against a real
//! orchestrator run" acceptance items are live/manual verification beyond
//! what a hermetic test can assert (they require a real Postgres instance
//! and a real orchestrator run to diff against) and are deferred to a
//! manual verification pass — recorded in the spec's Amendment Log.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use claude_code_rs::Outcome;
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::final_validation::FinalValidationNode;
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
use uuid::Uuid;

/// Replaces the real `SetupWorktreeNode`: writes a controlled temp-dir
/// `worktree_path` (and a fixed `branch_name`) directly, so no real `git
/// worktree add` runs. Still resolves + stamps `RESOLVED_POLICY_IDENTITY`
/// (EN.5.D task 8: downstream task-loop/wrap-up nodes now read the stamp
/// strictly, no per-node re-resolution or silent `Default` fallback), the
/// same as the real `SetupWorktreeNode::process` does.
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
                "branch_name": "sdlc/fixture-e2e-spec",
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

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning").join("fixture-e2e-spec")).unwrap();
    dir
}

/// Writes the fixture `tasks.json` (`task_count` PENDING tasks) and a
/// `harness.json` under `<worktree>/planning/fixture-e2e-spec/`.
///
/// The single check declares BOTH `command` ("full-suite-check") and
/// `fastCommand` ("fast-check"), and `sdlc.policy.test_depth` is pinned to
/// `"fast"` — so `TestTaskNode`'s per-task tripwire invokes `fastCommand`
/// while `FinalValidationNode` (which always forces `TestDepth::Full`
/// internally, ignoring this policy field) invokes `command`. That gives
/// the "runs exactly once" test a command string it can count unambiguously
/// against the per-task tripwire's own (potentially many) invocations.
fn write_fixture_files(worktree: &Path, task_count: u32, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join("fixture-e2e-spec");
    let tasks: Vec<serde_json::Value> = (1..=task_count)
        .map(|task_id| {
            json!({
                "task_id": task_id,
                "title": format!("Implement thing {task_id}"),
                "description": "Do the work",
                "acceptance_criteria": ["it works"],
                "max_attempts": max_attempts,
            })
        })
        .collect();
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    let harness = json!({
        "sdlc": { "policy": { "test_depth": "fast" } },
        "validation": {
            "checks": [
                {
                    "name": "tests",
                    "kind": "command",
                    "command": "full-suite-check",
                    "fastCommand": "fast-check",
                    "gates": true,
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

/// Injected `TestTaskNode` runner that always PASSes. Special-cases
/// `git status --porcelain` to report `src/lib.rs` as modified so it stays
/// consistent with `build_workflow`'s stubbed `ImplementTaskNode` claim
/// (`modified_files: ["src/lib.rs"]`) — otherwise `TestTaskNode`'s
/// write-verification guard would trip on every "pass" run since a claimed
/// write never shows up in this runner's (otherwise empty) git output.
fn always_pass_runner() -> CommandRunner {
    Arc::new(|program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Injected `TestTaskNode` runner that always FAILs — drives the never-
/// passing-task retry-bail path.
fn always_fail_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "always fails".to_string(),
        })
    })
}

/// Injected runner shared by `TestTaskNode` and `FinalValidationNode` (the
/// "reuse `test_runner`" option `EN.3.E` task 4 calls for): always PASSes,
/// same `git status` special-case as [`always_pass_runner`], and additionally
/// records every harness-check invocation's joined args, in order, so a test
/// can count exactly how many times the per-task fast tripwire
/// (`fast-check`) vs. the run-level full gate (`full-suite-check`) actually
/// ran.
fn recording_pass_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
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
        recorded_clone.lock().unwrap().push(args.join(" "));
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    (runner, recorded)
}

/// Injected runner (also shared by `TestTaskNode` and `FinalValidationNode`)
/// that passes the per-task fast tripwire (`fast-check`) but fails the
/// full-suite gate (`full-suite-check`) — drives the failing-gate path
/// where the run must still reach `EmitStateNode` with a degraded
/// `final_validation` result rather than halting.
fn passes_fast_fails_full_runner() -> CommandRunner {
    Arc::new(|program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        let joined = args.join(" ");
        Ok(CommandOutput {
            status: if joined.contains("full-suite-check") {
                1
            } else {
                0
            },
            stdout: String::new(),
            stderr: if joined.contains("full-suite-check") {
                "full-suite-check failed".to_string()
            } else {
                String::new()
            },
        })
    })
}

/// Injected runner for `git`-shaped calls that always succeeds with empty
/// output — no real subprocess spawns.
fn noop_git_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Stub `gh pr create` / `git push` runner: records every invocation and
/// returns a canned PR URL for `gh` calls.
fn gh_stub_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        calls_clone
            .lock()
            .unwrap()
            .push(format!("{program} {}", args.join(" ")));
        if program == "gh" {
            Ok(CommandOutput {
                status: 0,
                stdout: "https://github.com/example/repo/pull/7\n".to_string(),
                stderr: String::new(),
            })
        } else {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    });
    (runner, calls)
}

/// Builds the full assembled `SDLC_FLOW` `Workflow`: the real declared graph
/// from [`graph::schema`], paired with a registry where every model/
/// subprocess node is stubbed and every other identity is the real
/// implementation.
fn build_workflow(
    worktree: &Path,
    test_runner: CommandRunner,
    pr_runner: CommandRunner,
    emit_state_runner: CommandRunner,
) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        |_config, _prompt| {
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

    registry.register(Box::new(
        TestTaskNode::new().with_runner(test_runner.clone()),
    ));
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
    // Reuses `test_runner` (the option `EN.3.E` task 4's spec calls out)
    // rather than adding a fifth `build_workflow` parameter: `TestTaskNode`
    // and `FinalValidationNode` are both pure `CommandRunner` consumers, and
    // sharing the same injected runner is what lets a single recording stub
    // (see `recording_pass_runner`) observe both the per-task fast tripwire
    // and the run-level full gate's invocations in one call log.
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(test_runner),
    ));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(PullRequestNode::new().with_runner(pr_runner)));
    registry.register(Box::new(
        EmitStateNode::new().with_runner(emit_state_runner),
    ));

    let schema = graph::schema();

    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

/// Builds an [`EventsRow`] for a completed run the way the durable store
/// layer (`engine-serve`/`engine-store`, contract §4) would: `data` is the
/// run's inbound event, `task_context` is the final `TaskContext`. Asserts
/// the parity shape by round-tripping through `serde_json`, mirroring
/// `engine-store/tests/postgres_round_trip.rs`'s assertion style without
/// requiring a live Postgres instance.
fn events_row_for(workflow_type: &str, event: serde_json::Value, ctx: &TaskContext) -> EventsRow {
    let now = Utc::now();
    EventsRow {
        id: Uuid::new_v4(),
        workflow_type: workflow_type.to_string(),
        data: event,
        task_context: ctx.clone(),
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn happy_path_reaches_tail_with_auto_pr_false() {
    let worktree = temp_worktree("happy-no-pr");
    write_fixture_files(&worktree, 1, 2);

    let workflow = build_workflow(
        &worktree,
        always_pass_runner(),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event.clone(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    // --- The walk reaches the full bottom-half tail -------------------------
    for identity in [
        "FinalValidationNode",
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
    }

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["all_passed"], json!(true));

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
    assert!(emit_state_result.get("emitted").is_some());

    // --- Durable EventsRow mapping (D44/D45 contract §4) --------------------
    let row = events_row_for("SDLC_FLOW", event, &final_ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "SDLC_FLOW");
    assert_eq!(
        round_tripped.task_context.nodes.get("WrapUpNode"),
        final_ctx.nodes.get("WrapUpNode")
    );

    // --- D31-committed on-disk state file (D10) ------------------------------
    // The real path/schema shared with base-template's JS `sdlc-flow.js`
    // engine (D31) and already assumed by `bastion`'s `flow.rs` reader and
    // `run-sdlc-flow.sh`'s `jq` queries — NOT the old flat
    // `planning/{spec}/sdlc-flow-state.json` path/array-tasks shape.
    let state_path = worktree
        .join("planning")
        .join("fixture-e2e-spec")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    assert!(
        state_path.exists(),
        "committed state file should land at the sdlc/-subdirectory path"
    );
    let state_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap())
            .expect("committed state file should parse as JSON");
    assert!(
        state_json["tasks"].is_object(),
        "tasks must be an object, not an array: {state_json:?}"
    );
    assert_eq!(state_json["status"], json!("done"));
    assert_eq!(state_json["current_task"], json!(1));
    assert!(state_json["branch"].as_str().is_some());
    assert!(state_json["worktree_path"].as_str().is_some());
    assert!(state_json["started_at"].as_str().is_some());
    assert!(state_json["updated_at"].as_str().is_some());
    assert!(state_json["bail_reason"].is_null());
    assert_eq!(state_json["final_validation"]["all_passed"], json!(true));
}

#[tokio::test]
async fn happy_path_auto_pr_true_pushes_and_opens_pr() {
    let worktree = temp_worktree("happy-with-pr");
    write_fixture_files(&worktree, 1, 2);

    let (pr_runner, pr_calls) = gh_stub_runner();
    let workflow = build_workflow(
        &worktree,
        always_pass_runner(),
        pr_runner,
        noop_git_runner(),
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": true });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let pr_result = final_ctx
        .nodes
        .get("PullRequestNode")
        .expect("PullRequestNode should have stamped a result");
    assert_eq!(pr_result["skipped"], json!(false));
    assert_eq!(
        pr_result["pr_url"],
        json!("https://github.com/example/repo/pull/7")
    );

    let recorded = pr_calls.lock().unwrap();
    assert!(recorded.iter().any(|call| call.starts_with("git push")));
    assert!(recorded.iter().any(|call| call.starts_with("gh pr create")));

    let emit_state_result = final_ctx
        .nodes
        .get("EmitStateNode")
        .expect("EmitStateNode should have stamped a result");
    assert!(emit_state_result.get("emitted").is_some());

    let final_validation_run = final_ctx
        .node_runs
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have run on the drain branch");
    assert_eq!(final_validation_run.status, NodeRunStatus::Success);
}

#[tokio::test]
async fn never_passing_task_bails_at_max_attempts_and_reaches_wrap_up_tail() {
    let worktree = temp_worktree("bail");
    let max_attempts = 2;
    write_fixture_files(&worktree, 1, max_attempts);

    let workflow = build_workflow(
        &worktree,
        always_fail_runner(),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    // --- The task bailed exactly at max_attempts, never further -------------
    // `TriageRouterNode`'s `MAJOR_BAIL` verdict routes straight to
    // `WrapUpNode` (never through `UpdateTaskStatusNode`), so the durable
    // state's last write is `IncrementAttemptNode`'s (task 5's retry-bail
    // fix): `attempt_count`/`telemetry.total_attempts` reached exactly
    // `max_attempts`, no further.
    let increment_result = final_ctx
        .nodes
        .get("IncrementAttemptNode")
        .expect("IncrementAttemptNode should have run on every retry back-edge");
    let state: engine_core::workflows::sdlc_flow::schema::SDLCState =
        serde_json::from_value(increment_result.clone()).expect("state should deserialize");

    assert_eq!(state.tasks.len(), 1);
    assert_eq!(state.tasks[0].attempt_count, max_attempts);
    assert_eq!(state.telemetry.total_attempts, max_attempts);
    assert!(
        !final_ctx.nodes.contains_key("UpdateTaskStatusNode"),
        "UpdateTaskStatusNode should not run on the MAJOR_BAIL path"
    );

    // --- The MAJOR_BAIL / structural-fail path still reaches the wrap-up tail
    for identity in [
        "TriageTaskNode",
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
            "expected '{identity}' to have run to SUCCESS on the bail path"
        );
    }

    let triage_result = final_ctx
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("MAJOR_BAIL"));

    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode should have stamped a result even on the bail path");
    assert!(wrap_up_result.get("log_entry").is_some());
    assert!(wrap_up_result.get("report").is_some());
    assert!(wrap_up_result.get("status_suggestion").is_some());
    // NOTE (recorded in the spec's Amendment Log): `WrapUpNode`'s own
    // `latest_state` helper (wrap_up.rs) only consults
    // `UpdateTaskStatusNode`/`LoadTaskStateNode`, not `IncrementAttemptNode`
    // — on this MAJOR_BAIL path (which never reaches `UpdateTaskStatusNode`)
    // it therefore falls back to the *initial* `LoadTaskStateNode` snapshot,
    // so the rendered outcome does not reflect the retries that actually
    // happened. This test only asserts the tail is reached with the three
    // artifacts present (the task 7 acceptance criterion), not their
    // specific PASS/PARTIAL content on the bail path.

    // `PatchDocsNode` never runs on the bail path: `TriageRouterNode`'s
    // `MAJOR_BAIL` verdict routes straight to `WrapUpNode`.
    assert!(
        !final_ctx.node_runs.contains_key("PatchDocsNode")
            || final_ctx.node_runs["PatchDocsNode"].status == NodeRunStatus::Pending,
        "PatchDocsNode should not have executed on the MAJOR_BAIL path"
    );

    // `FinalValidationNode` sits on `TaskQueueRouterNode`'s DRAIN branch,
    // downstream of `PatchDocsNode` — a router that never reaches the drain
    // (this MAJOR_BAIL path routes straight from `TriageRouterNode` to
    // `WrapUpNode`) must not pass through it either.
    assert!(
        !final_ctx.node_runs.contains_key("FinalValidationNode")
            || final_ctx.node_runs["FinalValidationNode"].status == NodeRunStatus::Pending,
        "FinalValidationNode should not have executed on the MAJOR_BAIL path"
    );
    assert!(
        !final_ctx.nodes.contains_key("FinalValidationNode"),
        "FinalValidationNode should not have stamped a result on the MAJOR_BAIL path"
    );

    // --- Durable EventsRow mapping still producible on the bail path --------
    let event_for_row = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let row = events_row_for("SDLC_FLOW", event_for_row, &final_ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);

    // --- D31-committed on-disk state file reflects the MAJOR_BAIL terminal --
    // signal: `status == "blocked"` and a populated `bail_reason` (D10).
    let state_path = worktree
        .join("planning")
        .join("fixture-e2e-spec")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    let state_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap())
            .expect("committed state file should parse as JSON");
    assert_eq!(state_json["status"], json!("blocked"));
    assert!(
        state_json["bail_reason"].as_str().is_some(),
        "bail_reason should be populated on the MAJOR_BAIL path: {state_json:?}"
    );
    assert!(
        state_json["final_validation"].is_null(),
        "final_validation should be null: FinalValidationNode never ran on the MAJOR_BAIL path"
    );
}

/// `FinalValidationNode` runs exactly once per run, on the task-loop drain
/// branch, even when the fixture carries multiple pending tasks that make
/// `TaskQueueRouterNode` loop more than once. The per-task fast tripwire
/// (`fast-check`, `TestDepth::Fast` via this fixture's `sdlc.policy.test_depth`)
/// fires once per task; the full-suite gate (`full-suite-check`,
/// `FinalValidationNode`'s hardcoded `TestDepth::Full`) must fire exactly
/// once, after the loop drains, not once per task.
#[tokio::test]
async fn final_validation_node_runs_exactly_once_over_multi_task_run() {
    let worktree = temp_worktree("multi-task-once");
    let task_count = 3;
    write_fixture_files(&worktree, task_count, 2);

    let (test_runner, recorded) = recording_pass_runner();
    let workflow = build_workflow(&worktree, test_runner, noop_git_runner(), noop_git_runner());

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let final_validation_run = final_ctx
        .node_runs
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have run on the drain branch");
    assert_eq!(final_validation_run.status, NodeRunStatus::Success);

    let recorded = recorded.lock().unwrap();
    let full_suite_invocations = recorded
        .iter()
        .filter(|call| call.contains("full-suite-check"))
        .count();
    assert_eq!(
        full_suite_invocations, 1,
        "the full-suite gate must run exactly once per run, regardless of task count: {recorded:?}"
    );

    let fast_invocations = recorded
        .iter()
        .filter(|call| call.contains("fast-check"))
        .count();
    assert_eq!(
        fast_invocations, task_count as usize,
        "the per-task fast tripwire should run once per task: {recorded:?}"
    );
}

/// A run whose per-task fast tripwire passes but whose full-suite gate fails
/// still reaches `EmitStateNode` (the node reports, it does not bail) and the
/// committed state records the degraded `final_validation` result.
#[tokio::test]
async fn failing_final_validation_gate_still_reaches_emit_state_with_degraded_result() {
    let worktree = temp_worktree("failing-gate");
    write_fixture_files(&worktree, 1, 2);

    let workflow = build_workflow(
        &worktree,
        passes_fast_fails_full_runner(),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    // The node reports the failure; it does NOT halt the walk.
    for identity in ["FinalValidationNode", "WrapUpNode", "EmitStateNode"] {
        let run = final_ctx
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("expected a NodeRun for '{identity}'"));
        assert_eq!(
            run.status,
            NodeRunStatus::Success,
            "'{identity}' should still run to SUCCESS even though the gate failed"
        );
    }

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["all_passed"], json!(false));
    assert!(!final_validation_result["failure_summary"]
        .as_str()
        .unwrap()
        .is_empty());

    let state_path = worktree
        .join("planning")
        .join("fixture-e2e-spec")
        .join("sdlc")
        .join("sdlc-flow-state.json");
    let state_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap())
            .expect("committed state file should parse as JSON");
    assert_eq!(state_json["final_validation"]["all_passed"], json!(false));
    assert!(!state_json["final_validation"]["failure_summary"]
        .as_str()
        .unwrap()
        .is_empty());
}
