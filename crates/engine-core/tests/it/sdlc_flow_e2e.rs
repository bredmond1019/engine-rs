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
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::end_review::{EndReviewNode, EndReviewRouterNode};
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
///
/// Note it has never stamped `base_sha`. Since
/// `ticket-commit-task-work-real-diffs` that is not a gap at all: review and
/// trivial-skip diffs are taken as working-tree-vs-`HEAD`, so no diff base is
/// derived from the stamp and its absence cannot change what a run reviews.
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
        session_id: None,
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
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the subdirs.
    std::fs::remove_dir_all(&dir).ok();
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
/// `git_runner` is shared by EVERY node in the run that shells out to git on
/// the flow's own behalf — `SaveStateNode`, `WrapUpNode`, and
/// `PullRequestNode`. Sharing one injected runner is what lets a single
/// recording stub observe the run's whole git call SEQUENCE in order, which
/// is how the commit-topology regressions below assert that per-task commits
/// land before the wrap-up commit and that the wrap-up commit lands before
/// the push. It also closes a hermeticity hole: `WrapUpNode::new()` used to
/// be registered un-injected, so it carried `default_command_runner` and
/// really did spawn `git` from the temp worktree.
fn build_workflow(
    worktree: &Path,
    test_runner: CommandRunner,
    git_runner: CommandRunner,
    emit_state_runner: CommandRunner,
) -> Workflow {
    build_workflow_with_docs(
        worktree,
        test_runner,
        git_runner,
        emit_state_runner,
        default_docs_node(),
    )
}

/// The `PatchDocsNode` stub [`build_workflow`] installs: reports a clean
/// sweep and writes nothing.
fn default_docs_node() -> PatchDocsNode {
    PatchDocsNode::new().with_transport(Arc::new(|_config, _prompt| {
        let outcome = stub_outcome(
            &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    }))
}

/// [`build_workflow`] with the `PatchDocsNode` stub supplied by the caller —
/// used by the docs-committed regression, which needs a docs stage that
/// really writes a file into the worktree.
fn build_workflow_with_docs(
    worktree: &Path,
    test_runner: CommandRunner,
    git_runner: CommandRunner,
    emit_state_runner: CommandRunner,
    docs_node: PatchDocsNode,
) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode::new()));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode::new()));
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
    // `TriageTaskNode` calls `classify_trivial` unconditionally on its PASS
    // branch, which shells out to git (`add -N -A`, then
    // `diff --numstat HEAD`). Registered un-injected it would carry
    // `default_command_runner` and really spawn those in the temp worktree —
    // breaking this suite's hermeticity claim, and now with a MUTATING call.
    registry.register(Box::new(
        TriageTaskNode::new().with_runner(git_runner.clone()),
    ));
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
        SaveStateNode::new().with_runner(git_runner.clone()),
    ));
    registry.register(Box::new(docs_node));
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
    registry.register(Box::new(EndReviewNode::new()));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(WrapUpNode::new().with_runner(git_runner.clone())));
    registry.register(Box::new(CloseBlockNode::new()));
    registry.register(Box::new(PullRequestNode::new().with_runner(git_runner)));
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
    // `WrapUpNode`, never through `UpdateTaskStatusNode` — so the terminal
    // state `WrapUpNode` reports is the one this asserts on.
    //
    // The two counters are different quantities and must not be reconciled:
    // `attempt_count` counts RETRIES and stops at exactly `max_attempts`
    // (task 5's retry-bail fix), while `telemetry.total_attempts` counts
    // implement -> test attempts MADE — the initial dispatch plus each retry,
    // i.e. `max_attempts + 1`. Charging the attempt at `ImplementTaskNode`
    // is what makes the last one — the one that produced the bail — appear
    // at all; charged at the outcome it was invisible, because no
    // outcome-charging site sits on this path (run R4 reported 0).
    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode should have run on the bail path");
    let state: engine_core::workflows::sdlc_flow::schema::SDLCState =
        serde_json::from_value(wrap_up_result["state"].clone()).expect("state should deserialize");

    assert_eq!(state.tasks.len(), 1);
    assert_eq!(state.tasks[0].attempt_count, max_attempts);
    assert_eq!(state.telemetry.total_attempts, max_attempts + 1);
    assert!(
        final_ctx.nodes.contains_key("IncrementAttemptNode"),
        "the retry back-edge should still have run on every retry"
    );
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

    // ticket-wrapup-outcome-truth: end-to-end proof that a bailed run is
    // never recorded as a success. `log_entry` is what gets pasted into a
    // work log, and `UpdateTaskStatusNode` (asserted above as never having
    // run) is the only thing that increments `telemetry.tasks_failed` — so
    // the outcome word MUST come from the terminal signal, not the counter.
    let log_entry = wrap_up_result["log_entry"]
        .as_str()
        .expect("log_entry should be a string");
    assert!(
        log_entry.contains("Outcome: BLOCKED"),
        "a MAJOR_BAIL run must render BLOCKED, got: {log_entry}"
    );
    assert!(
        !log_entry.contains("Outcome: PASS"),
        "a MAJOR_BAIL run must never render PASS, got: {log_entry}"
    );
    assert!(
        wrap_up_result["report"]
            .as_str()
            .expect("report should be a string")
            .contains("- Outcome: BLOCKED"),
        "the report must agree with log_entry on the bail path"
    );
    assert!(
        !wrap_up_result["status_suggestion"]
            .as_str()
            .expect("status_suggestion should be a string")
            .contains("completed successfully"),
        "a blocked run must not suggest a done status"
    );

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

// --- commit topology (ticket-commit-task-work-real-diffs task 6) -----------

/// Shared handle to a recorded `(program, argv)` call log.
type RecordedCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// A `git`-shaped recording runner for the flow's own git calls. Records
/// `(program, argv)` for every invocation in order and returns empty success,
/// so the whole run's commit sequence can be asserted. `git status` is
/// special-cased the same way [`always_pass_runner`] does — but note this
/// runner is injected into `SaveStateNode`/`WrapUpNode`/`PullRequestNode`,
/// not `TestTaskNode`, so it never sees the harness checks.
fn recording_git_runner() -> (CommandRunner, RecordedCalls) {
    let calls: RecordedCalls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        calls_clone.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| (*s).to_string()).collect(),
        ));
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });
    (runner, calls)
}

/// Index of every `git commit` in a recorded call log.
fn commit_indices(calls: &[(String, Vec<String>)]) -> Vec<usize> {
    calls
        .iter()
        .enumerate()
        .filter(|(_, (program, args))| {
            program == "git" && args.first().map(String::as_str) == Some("commit")
        })
        .map(|(i, _)| i)
        .collect()
}

/// The commit message of the `git commit` at `index`.
fn commit_message(calls: &[(String, Vec<String>)], index: usize) -> &str {
    calls[index].1[2].as_str()
}

/// **The commit topology this ticket establishes**, asserted end to end over
/// a real two-task run of the assembled graph:
///
/// - each passed task produces exactly one `git add -A` + `git commit` pair,
///   with the task id in the message (so `HEAD` carries that task's code and
///   state, and the next task's working-tree diff is only its own work);
/// - the wrap-up produces exactly one further commit;
/// - the wrap-up commit strictly PRECEDES `git push` — this is what puts
///   `PatchDocsNode`'s doc edits (`docs.rs` makes no git calls at all) on the
///   branch that gets pushed, and what makes an auto-PR contain real code
///   instead of state files only.
#[tokio::test]
async fn per_task_and_wrap_up_commits_land_in_order_before_the_push() {
    let worktree = temp_worktree("commit-topology");
    let task_count = 2;
    write_fixture_files(&worktree, task_count, 2);

    let (git_runner, calls) = recording_git_runner();
    let workflow = build_workflow(
        &worktree,
        always_pass_runner(),
        git_runner,
        noop_git_runner(),
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": true });
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let recorded = calls.lock().unwrap();
    let commits = commit_indices(&recorded);
    assert_eq!(
        commits.len(),
        task_count as usize + 1,
        "expected one commit per passed task plus one wrap-up commit: {recorded:?}"
    );

    // Every commit is immediately preceded by `git add -A` — the widened
    // staging that carries CODE, not just the state file.
    for &i in &commits {
        assert!(i > 0, "a commit must be preceded by its add: {recorded:?}");
        assert_eq!(recorded[i - 1].0, "git");
        assert_eq!(
            recorded[i - 1].1,
            vec!["add".to_string(), "-A".to_string()],
            "commit at {i} was not preceded by `git add -A`: {recorded:?}"
        );
    }

    // The per-task commits carry their task ids, in task order.
    assert!(commit_message(&recorded, commits[0]).starts_with("feat(sdlc): 1 —"));
    assert!(commit_message(&recorded, commits[1]).starts_with("feat(sdlc): 2 —"));
    // ...and the last commit is the wrap-up's.
    let wrap_up_commit = commits[2];
    assert_eq!(
        commit_message(&recorded, wrap_up_commit),
        "chore(sdlc): wrap-up — docs patch + terminal state"
    );

    // The wrap-up commit precedes the push, so the pushed branch contains it.
    let push_index = recorded
        .iter()
        .position(|(program, args)| {
            program == "git" && args.first().map(String::as_str) == Some("push")
        })
        .expect("the auto_pr run should have pushed");
    assert!(
        wrap_up_commit < push_index,
        "wrap-up commit (at {wrap_up_commit}) must precede the push (at {push_index}): {recorded:?}"
    );
}

/// `PatchDocsNode` runs BEFORE `WrapUpNode` on the drain branch and makes no
/// git calls of its own, so the wrap-up's `git add -A` is the only thing that
/// can land a doc patch on the branch. Pins that ordering: the wrap-up's
/// staging is recorded strictly after `PatchDocsNode` completed, and still
/// before the push.
#[tokio::test]
async fn wrap_up_stages_after_patch_docs_ran_and_before_the_push() {
    let worktree = temp_worktree("docs-committed");
    write_fixture_files(&worktree, 1, 2);

    // Records, for each `git add -A`, whether the docs stage's file was
    // already on disk when that staging ran. That is what actually proves the
    // wrap-up's staging can see the doc patch — an ordering assertion over
    // argv alone would pass even if PatchDocsNode wrote nothing.
    let doc_file = worktree.join("docs").join("patched.md");
    let doc_present_at_add: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let doc_present_clone = doc_present_at_add.clone();
    let doc_file_probe = doc_file.clone();
    let (recorder, calls) = recording_git_runner();
    let git_runner: CommandRunner = Arc::new(move |program, args, cwd| {
        if program == "git" && args == ["add", "-A"] {
            doc_present_clone
                .lock()
                .unwrap()
                .push(doc_file_probe.exists());
        }
        recorder(program, args, cwd)
    });

    // A PatchDocsNode whose stubbed transport actually WRITES a doc file into
    // the worktree, exactly as a real docs patch would — so there is genuine
    // uncommitted content for the wrap-up commit to sweep up.
    let docs_worktree = worktree.clone();
    let docs_node = PatchDocsNode::new().with_transport(Arc::new(move |_config, _prompt| {
        let docs_dir = docs_worktree.join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("patched.md"), "# patched by the docs stage\n").unwrap();
        let outcome = stub_outcome(
            &json!({ "summary": "patched one doc", "files_patched": ["docs/patched.md"] })
                .to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    }));

    let workflow = build_workflow_with_docs(
        &worktree,
        always_pass_runner(),
        git_runner,
        noop_git_runner(),
        docs_node,
    );

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": true });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        final_ctx.node_runs["PatchDocsNode"].status,
        NodeRunStatus::Success
    );

    let recorded = calls.lock().unwrap();
    let commits = commit_indices(&recorded);
    let wrap_up_commit = *commits.last().expect("a wrap-up commit must exist");
    assert_eq!(
        commit_message(&recorded, wrap_up_commit),
        "chore(sdlc): wrap-up — docs patch + terminal state",
        "the LAST commit must be the wrap-up's, i.e. after PatchDocsNode ran"
    );
    let push_index = recorded
        .iter()
        .position(|(program, args)| {
            program == "git" && args.first().map(String::as_str) == Some("push")
        })
        .expect("the auto_pr run should have pushed");
    assert!(wrap_up_commit < push_index);

    // The doc file really was written, and really was on disk by the time the
    // LAST `git add -A` (the wrap-up's) ran — so that staging would pick it
    // up. The per-task staging ran before PatchDocsNode and did not see it.
    assert!(
        doc_file.exists(),
        "the docs stage should have written its file"
    );
    let presence = doc_present_at_add.lock().unwrap();
    assert_eq!(
        presence.first(),
        Some(&false),
        "the per-task staging runs before PatchDocsNode, so the doc must be absent then"
    );
    assert_eq!(
        presence.last(),
        Some(&true),
        "the wrap-up staging runs after PatchDocsNode, so the doc must be present then: \
         {presence:?}"
    );
}

/// **Retry-leak check.** The retry path (`TriageRouterNode`/`ReviewRouterNode`
/// → `IncrementAttemptNode` → `ImplementTaskNode`) never touches
/// `SaveStateNode`, so a failed attempt must produce NO commit — its work
/// stays in the working tree, where the next attempt's `git diff HEAD` sees
/// it cumulatively. Only the eventual pass commits, exactly once.
#[tokio::test]
async fn a_failed_attempt_commits_nothing_and_the_pass_commits_once() {
    let worktree = temp_worktree("retry-leak");
    write_fixture_files(&worktree, 1, 3);

    // Fails the harness check on the first attempt, passes on every attempt
    // after — the fail-then-pass shape from the retry-feedback suite.
    let attempts = Arc::new(AtomicUsize::new(0));
    let test_runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        let n = attempts.fetch_add(1, Ordering::SeqCst);
        Ok(CommandOutput {
            status: i32::from(n == 0),
            stdout: String::new(),
            stderr: if n == 0 {
                "first attempt fails".to_string()
            } else {
                String::new()
            },
        })
    });

    let (git_runner, calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, test_runner, git_runner, noop_git_runner());

    let event = json!({ "spec_slug": "fixture-e2e-spec", "auto_pr": false });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    // The run really did retry.
    assert!(
        final_ctx.node_runs.contains_key("IncrementAttemptNode"),
        "the fixture should have driven at least one retry"
    );

    let recorded = calls.lock().unwrap();
    let commits = commit_indices(&recorded);
    assert_eq!(
        commits.len(),
        2,
        "exactly one per-task commit (after the pass) plus the wrap-up commit — \
         a retry must not commit: {recorded:?}"
    );
    assert!(commit_message(&recorded, commits[0]).starts_with("feat(sdlc): 1 —"));
    assert_eq!(
        commit_message(&recorded, commits[1]),
        "chore(sdlc): wrap-up — docs patch + terminal state"
    );
}

/// **The one test in this suite that runs REAL git**, deliberately.
///
/// Every other assertion about the review diff stubs `CommandRunner` and can
/// therefore only pin argv *shape*. But the load-bearing claim of
/// `ticket-commit-task-work-real-diffs` is a claim about git SEMANTICS: that
/// `git add -N -A` followed by `git diff HEAD` surfaces a brand-new untracked
/// file's content to the reviewer. Plausible-argv-with-empty-semantics is
/// exactly the failure mode that produced the original defect (the old
/// `<base_sha>..HEAD` range was perfectly well-formed and always empty), so
/// asserting argv alone would leave the real bug class unpinned.
///
/// Drives the real `ConsolidatedReviewNode` with its DEFAULT command runner
/// against a throwaway repository, and asserts both a modified tracked file
/// and a brand-new untracked file reach the reviewer's prompt.
#[tokio::test]
async fn real_git_intent_add_surfaces_untracked_content_in_the_review_prompt() {
    let repo = temp_worktree("real-git");

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("git should be available on PATH")
    };

    git(&["init", "--quiet"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test"]);
    std::fs::write(repo.join("tracked.rs"), "fn original() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "base"]);

    // The current "task's" work: one tracked edit + one brand-new file.
    std::fs::write(repo.join("tracked.rs"), "fn edited() {}\n").unwrap();
    std::fs::write(repo.join("brand_new.rs"), "fn newly_created() {}\n").unwrap();

    let mut ctx = TaskContext {
        event: json!({ "spec_slug": "fixture-e2e-spec" }),
        nodes: std::collections::HashMap::new(),
        metadata: json!({}),
        node_runs: std::collections::HashMap::new(),
    };
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": repo.to_string_lossy() }),
    );
    ctx.nodes.insert(
        "TaskQueueRouterNode".to_string(),
        json!({ "current_task_id": 1, "title": "One", "acceptance_criteria": ["it works"] }),
    );
    // `ConsolidatedReviewNode` now bumps the durable `review_attempts`
    // counter (EN.ticket.review-retry-loop-unbounded task 2) via
    // `latest_state`, which requires a loaded `SDLCState` on `ctx` — same
    // as every other `ConsolidatedReviewNode` unit test's
    // `ctx_with_current_task` helper stamps.
    let mut state = engine_core::workflows::sdlc_flow::schema::SDLCState::new("fixture-e2e-spec");
    state.tasks = vec![engine_core::workflows::sdlc_flow::schema::SDLCTask::new(
        1, "One", "d1",
    )];
    ctx.nodes.insert(
        "LoadTaskStateNode".to_string(),
        serde_json::to_value(&state).unwrap(),
    );
    let policy = engine_core::workflows::sdlc_flow::policy::SdlcPolicy::default();
    engine_core::policy::stamp_resolved_policy(&mut ctx, &policy).unwrap();

    let seen_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_prompt_clone = seen_prompt.clone();
    let node = ConsolidatedReviewNode::new().with_transport(Arc::new(move |_config, prompt| {
        *seen_prompt_clone.lock().unwrap() = Some(prompt);
        let outcome =
            stub_outcome(&json!({ "verdict": "PASS", "summary": "ok", "issues": [] }).to_string());
        Box::pin(async move { Ok(outcome) })
    }));

    node.process(ctx).await.expect("review should succeed");

    let prompt = seen_prompt
        .lock()
        .unwrap()
        .clone()
        .expect("the transport should have been called");

    assert!(
        prompt.contains("fn edited()"),
        "the modified tracked file must reach the reviewer:\n{prompt}"
    );
    assert!(
        prompt.contains("fn newly_created()"),
        "the BRAND-NEW untracked file's content must reach the reviewer — this is what \
         `git add -N -A` buys, and what a plain `git diff HEAD` would omit:\n{prompt}"
    );
    // The pre-existing content appears only as a REMOVED line, never as
    // context the reviewer might mistake for new work.
    assert!(
        prompt.contains("-fn original()"),
        "the replaced line should show as a deletion:\n{prompt}"
    );
    assert!(
        !prompt.contains("+fn original()"),
        "already-committed content must not be presented as this task's work:\n{prompt}"
    );

    std::fs::remove_dir_all(&repo).ok();
}
