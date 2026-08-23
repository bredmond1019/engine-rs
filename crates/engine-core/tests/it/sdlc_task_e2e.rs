//! End-to-end integration test for the full assembled `SDLC_TASK` workflow
//! (`EN.11.N` task 6, port design T9): drives the graph from
//! `SetupWorktreeNode` through the lean tail —
//! `... -> FinalValidationNode(Reconcile) -> LeanBookkeepNode ->
//! CloseBlockNode -> EmitStateNode` — with every model/subprocess seam
//! stubbed, mirroring the seam-injection style of `sdlc_flow_e2e.rs`.
//!
//! Hermetic by construction: no real `claude`, `git`, or `mev` subprocess is
//! ever spawned by an INJECTED seam. `SetupWorktreeNode` is replaced outright
//! by a tiny fixture node (as in `sdlc_flow_e2e.rs`) so the run resolves
//! paths under a controlled temp directory instead of the real
//! `trees/{branch}` layout. `CloseBlockNode` is the one node this suite does
//! NOT inject a stub for — like `sdlc_flow_e2e.rs`, it is driven against a
//! fixture worktree with no `planning/state.json`, so `evaluate` resolves to
//! a `Skipped` outcome (or, on the D56 reconcile-failed / blocked paths, the
//! block-status-aware skip) before ever touching a real `mev`/git call.
//!
//! Per-source-unit coverage table (D68 #2 — never a single aggregate total),
//! measured against `base-template/.claude/workflows/sdlc-task.js` at
//! authoring time (D68 #3):
//! - graph assembly/validation: [`graph_workflow_builds_and_validates_end_to_end`]
//! - happy path (`status: "done"`): [`happy_path_all_tasks_pass_writes_done`]
//! - `MAJOR_BAIL` (`status: "blocked"`): [`major_bail_at_max_attempts_writes_blocked`]
//! - D56 reconcile (`status: "reconcile_failed"`, terminal, skips bookkeep
//!   flip, leaves per-task commits standing):
//!   [`failing_reconcile_writes_reconcile_failed_and_is_terminal`]
//! - `--resume` on an all-passed set (zero `ImplementTaskNode` calls, exactly
//!   one reconcile): [`resume_on_all_passed_set_reruns_only_the_reconcile`]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_code_rs::Outcome;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::policy::emit_state::EmitStateNode as GenericEmitStateNode;
use engine_core::policy::PolicyConfigSource;
use engine_core::workflow::{OnProgress, Workflow};
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
use engine_core::workflows::sdlc_flow::final_validation::{FinalValidationNode, ValidationScope};
use engine_core::workflows::sdlc_flow::graph as sdlc_flow_graph;
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode, SpecExistsRouterNode,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ImplementTaskNode, IncrementAttemptNode, SaveStateNode, TaskQueueRouterNode, TestTaskNode,
    TriageTaskNode, UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_task::graph as sdlc_task_graph;
use engine_core::workflows::sdlc_task::lean_bookkeep::LeanBookkeepNode;
use engine_core::workflows::sdlc_task::profiles::resolve_policy_for_run_from;
use engine_core::workflows::sdlc_task::task_triage_router::TaskTriageRouterNode;
use engine_core::workflows::sdlc_task::DEFAULT_STATE_FILENAME;
// `EN.11.P` task 5: the ORCHESTRATION side of the mixed `task` + `flow`
// chain — a different subsystem from the rest of this file (which drives
// the assembled SDLC_TASK graph in isolation), pulled in only for the
// handful of tests at the bottom of this file per the task's file scope.
use engine_core::workflows::orchestration::execute::{EngineKind, FlowInvocation, FlowRunner};
use engine_core::workflows::orchestration::graph::{
    OrchestrationRunNode, NODE_NAME as ORCH_NODE_NAME,
};
use serde_json::json;

const SPEC_SLUG: &str = "fixture-task-e2e-spec";

/// Replaces the real `SetupWorktreeNode`: writes a controlled temp-dir
/// `worktree_path` (and a `"task/"`-prefixed branch, matching
/// `SetupWorktreeNode::with_branch_prefix("task/")`'s real behavior)
/// directly, so no real `git worktree add` runs. Still resolves + stamps
/// `RESOLVED_POLICY_IDENTITY`, exactly as the real node does — via
/// SDLC_TASK's OWN seam (`resolve_policy_for_run_from` against
/// `sdlc_task.{policy,profiles}`, projected through `to_sdlc_policy()`),
/// mirroring `sdlc_task_graph::sdlc_task_policy_resolver` (`EN.11.O` task
/// 3), rather than `sdlc_flow`'s plain `resolve_policy_for_run` — that
/// distinction is exactly what makes `event.profile` observable through
/// this suite's real registry (task 6).
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
                "branch_name": format!("task/{SPEC_SLUG}"),
            }),
        );

        let source = PolicyConfigSource::Worktree(PathBuf::from(&self.worktree_path));
        let resolved_task_policy = resolve_policy_for_run_from(&ctx, &source)?;
        let resolved_policy = resolved_task_policy.to_sdlc_policy();
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
        "engine-core-sdlc-task-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty (see `sdlc_flow_e2e.rs`'s identical helper for why
    // PID-recycling makes this removal necessary, not optional).
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join(SPEC_SLUG)).unwrap();
    dir
}

/// Writes the fixture `tasks.json` (`task_count` PENDING tasks) and a
/// `harness.json` under `<worktree>/planning/<SPEC_SLUG>/`.
///
/// `sdlc_task.policy.test_depth` is pinned `"fast"` so `TestTaskNode`'s per-task
/// tripwire invokes `fastCommand` while `FinalValidationNode` under
/// `ValidationScope::Reconcile` (this workflow's drain-branch scope) is free
/// to actually run `select_reconcile_checks` rather than hitting skip
/// condition 1 (`test_depth == Full`).
fn write_fixture_files(worktree: &Path, task_count: u32, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join(SPEC_SLUG);
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
        "sdlc_task": { "policy": { "test_depth": "fast" } },
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

/// Like [`write_fixture_files`], but the tasks OMIT their own
/// `max_attempts` field, so `LoadTaskStateNode::seed_max_attempts` seeds
/// each task's retry ceiling from the resolved policy instead of a literal
/// in the fixture — the seam task 6's profile-driven retry-ceiling
/// assertions need (`SdlcTaskPolicy::default().max_attempts` is 3;
/// `cheap-fast`'s is 2; `thorough`'s is 5). The `harness.json`'s
/// `validation.checks` shape is unchanged (still one `gates: true` check
/// with a `fastCommand` that differs from `command`, so
/// `select_reconcile_checks` still includes it for any `test_depth: Fast`
/// profile).
fn write_fixture_files_policy_driven_max_attempts(worktree: &Path, task_count: u32) {
    let spec_dir = worktree.join("planning").join(SPEC_SLUG);
    let tasks: Vec<serde_json::Value> = (1..=task_count)
        .map(|task_id| {
            json!({
                "task_id": task_id,
                "title": format!("Implement thing {task_id}"),
                "description": "Do the work",
                "acceptance_criteria": ["it works"],
            })
        })
        .collect();
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();

    let harness = json!({
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

/// Injected runner that always PASSes every check — special-cases
/// `git status --porcelain` to report a modified file (kept consistent with
/// `build_workflow`'s stubbed `ImplementTaskNode` claim) so `TestTaskNode`'s
/// write-verification guard never trips.
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

/// Injected runner that always FAILs — drives the never-passing-task
/// retry-bail (`MAJOR_BAIL`) path.
fn always_fail_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "always fails".to_string(),
        })
    })
}

/// Injected runner shared by `TestTaskNode` and `FinalValidationNode`:
/// passes the per-task fast tripwire (`fast-check`) but fails the
/// authoritative reconcile check (`full-suite-check`) — drives the D56
/// terminal-reconcile path.
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

/// Stub `git`-shaped runner that records every invocation's joined
/// arguments, in order, and always succeeds — used to observe both the
/// per-task `SaveStateNode` commits and (via `TriageTaskNode`'s unconditional
/// `classify_trivial` git calls) that no real subprocess is ever spawned.
fn recording_git_runner() -> (CommandRunner, Arc<Mutex<Vec<String>>>) {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();
    let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        recorded_clone
            .lock()
            .unwrap()
            .push(format!("{program} {}", args.join(" ")));
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
    });
    (runner, recorded)
}

/// Injected runner, shared by `TestTaskNode` and `FinalValidationNode`,
/// that always PASSes every check (special-cased `git status` for
/// `TestTaskNode`'s write-verification guard, as in [`always_pass_runner`])
/// while RECORDING every non-`git status` invocation's joined arguments —
/// the observable task 6's profile tests read to count reconcile-specific
/// (`full-suite-check`) calls versus per-task tripwire (`fast-check`)
/// calls, without asserting on any resolved policy value.
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

/// An `on_progress` callback that records the DISPATCH order of every node
/// identity — the first time each identity's `NodeRun` is observed
/// transitioning to `Running` — so a test can assert what ran, and in what
/// order, without depending on `HashMap` iteration order (`ctx.node_runs`
/// is scanned fully on every callback; only a not-yet-seen `Running` entry
/// is pushed, so only the ONE newly-dispatched node per callback lands).
struct OrderRecorder {
    order: Arc<Mutex<Vec<String>>>,
    seen: std::collections::HashSet<String>,
}

impl OrderRecorder {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let order = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                order: order.clone(),
                seen: std::collections::HashSet::new(),
            },
            order,
        )
    }

    fn callback(mut self) -> OnProgress<'static> {
        Box::new(move |ctx: &TaskContext| {
            for (identity, run) in &ctx.node_runs {
                if run.status == NodeRunStatus::Running && self.seen.insert(identity.clone()) {
                    self.order.lock().unwrap().push(identity.clone());
                }
            }
        })
    }
}

/// Builds the full assembled `SDLC_TASK` `Workflow`: the real declared
/// graph from [`sdlc_task_graph::schema`], paired with a registry where
/// every model/subprocess node is stubbed and every other identity is the
/// real implementation. Mirrors `sdlc_flow_e2e.rs::build_workflow`.
fn build_workflow(
    worktree: &Path,
    test_runner: CommandRunner,
    git_runner: CommandRunner,
) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(
        SpecExistsRouterNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(
        LoadTaskStateNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
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
    // branch, which shells out to git — inject the same recording/no-op
    // runner used elsewhere so this suite stays hermetic.
    registry.register(Box::new(
        TriageTaskNode::new().with_runner(git_runner.clone()),
    ));
    registry.register(Box::new(TaskTriageRouterNode));

    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new()
            .with_runner(git_runner.clone())
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(IncrementAttemptNode));

    // The drain-branch identity, at Reconcile scope — reuses `test_runner`
    // (same rationale `sdlc_flow_e2e.rs` documents: `TestTaskNode` and
    // `FinalValidationNode` are both pure `CommandRunner` consumers).
    registry.register(Box::new(
        FinalValidationNode::new()
            .with_runner(test_runner)
            .with_scope(ValidationScope::Reconcile),
    ));

    registry.register(Box::new(
        LeanBookkeepNode::new()
            .with_runner(git_runner)
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(
        CloseBlockNode::new().with_state_source("LeanBookkeepNode"),
    ));
    registry.register(Box::new(GenericEmitStateNode::new(Arc::new(
        |_program: &str, _args: &[&str], _cwd: &Path| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        },
    ))));

    let schema = sdlc_task_graph::schema();

    Workflow::new_validated(registry, schema)
        .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate")
}

/// Like [`build_workflow`], but `ImplementTaskNode`'s transport is wrapped
/// in a shared counter — the observable this suite's retry-ceiling
/// assertions need (task 6: "counting `ImplementTaskNode` invocations",
/// never a resolved-struct assertion). Returns the counter alongside the
/// `Workflow` so a test can drive a failing run and read off exactly how
/// many attempts `IncrementAttemptNode`'s loop actually dispatched before
/// `TaskTriageRouterNode`'s budget-exhausted `MAJOR_BAIL` arm fired.
fn build_workflow_counting_implement(
    worktree: &Path,
    test_runner: CommandRunner,
    git_runner: CommandRunner,
) -> (Workflow, Arc<AtomicUsize>) {
    let mut registry = NodeRegistry::new();
    let implement_calls = Arc::new(AtomicUsize::new(0));

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(
        SpecExistsRouterNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(
        LoadTaskStateNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(TaskQueueRouterNode));

    {
        let implement_calls = implement_calls.clone();
        registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
            move |_config, _prompt| {
                implement_calls.fetch_add(1, Ordering::SeqCst);
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
    }

    registry.register(Box::new(
        TestTaskNode::new().with_runner(test_runner.clone()),
    ));
    // `thorough`'s `llm_triage: true` (task 2) means `TriageTaskNode` calls
    // a model transport to classify a failure as RETRYABLE/MAJOR_BAIL
    // instead of the deterministic heuristic `llm_triage: false` uses — so
    // this builder MUST stub that transport too, or an under-budget
    // attempt would reach for the real `claude_code_rs::execute` transport
    // and hang/fail against a real network call. The bail gate itself
    // (`attempt_count >= max_attempts`) is checked BEFORE this branch, so
    // it fires deterministically regardless of what this stub returns.
    registry.register(Box::new(
        TriageTaskNode::new()
            .with_runner(git_runner.clone())
            .with_transport(Arc::new(|_config, _prompt| {
                let outcome = stub_outcome(
                    &json!({ "verdict": "RETRYABLE", "reason": "stubbed retry" }).to_string(),
                );
                Box::pin(async move { Ok(outcome) })
            })),
    ));
    registry.register(Box::new(TaskTriageRouterNode));

    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new()
            .with_runner(git_runner.clone())
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(IncrementAttemptNode));

    registry.register(Box::new(
        FinalValidationNode::new()
            .with_runner(test_runner)
            .with_scope(ValidationScope::Reconcile),
    ));

    registry.register(Box::new(
        LeanBookkeepNode::new()
            .with_runner(git_runner)
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(
        CloseBlockNode::new().with_state_source("LeanBookkeepNode"),
    ));
    registry.register(Box::new(GenericEmitStateNode::new(Arc::new(
        |_program: &str, _args: &[&str], _cwd: &Path| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        },
    ))));

    let schema = sdlc_task_graph::schema();

    (
        Workflow::new_validated(registry, schema)
            .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate"),
        implement_calls,
    )
}

fn read_state_json(worktree: &Path) -> serde_json::Value {
    let state_path = worktree
        .join("planning")
        .join(SPEC_SLUG)
        .join("sdlc")
        .join(DEFAULT_STATE_FILENAME);
    serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap_or_else(|err| {
        panic!(
            "committed state file should exist at {}: {err}",
            state_path.display()
        )
    }))
    .expect("committed state file should parse as JSON")
}

// --- graph assembly / validation ------------------------------------------

#[tokio::test]
async fn graph_workflow_builds_and_validates_end_to_end() {
    // `sdlc_task_graph::workflow()` already runs `Workflow::new_validated`
    // internally and panics on a structurally unsound graph — calling it at
    // all IS the assertion. Paired with `graph::schema`'s own
    // `WorkflowValidator::validate` unit test, this is the "first point the
    // graph validates end to end" this task's title names.
    let _workflow = sdlc_task_graph::workflow();

    // Cross-checked against the real `sdlc_flow` drain target this graph
    // reuses unmodified: `TaskQueueRouterNode::route`'s hardcoded string.
    let schema = sdlc_task_graph::schema();
    assert!(schema.nodes.contains_key("FinalValidationNode"));
    let _ = sdlc_flow_graph::WORKFLOW_TYPE; // sanity: the two graphs are distinct modules
}

// --- happy path: status "done" ---------------------------------------------

#[tokio::test]
async fn happy_path_all_tasks_pass_writes_done() {
    let worktree = temp_worktree("happy");
    write_fixture_files(&worktree, 2, 2);

    let (git_runner, git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, always_pass_runner(), git_runner);

    let (recorder, order) = OrderRecorder::new();
    let event = json!({ "spec_slug": SPEC_SLUG });
    let final_ctx = workflow
        .run(event, recorder.callback())
        .await
        .expect("workflow run should not error");

    for identity in [
        "TaskQueueRouterNode",
        "ImplementTaskNode",
        "TestTaskNode",
        "TriageTaskNode",
        "TaskTriageRouterNode",
        "UpdateTaskStatusNode",
        "SaveStateNode",
        "FinalValidationNode",
        "LeanBookkeepNode",
        "CloseBlockNode",
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

    // No review/docs/PR machinery exists in this graph at all.
    for absent in [
        "ConsolidatedReviewNode",
        "ReviewRouterNode",
        "EndReviewNode",
        "PatchDocsNode",
        "PullRequestNode",
        "WrapUpNode",
    ] {
        assert!(
            !final_ctx.node_runs.contains_key(absent),
            "'{absent}' must never appear in an SDLC_TASK run"
        );
    }

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["all_passed"], json!(true));

    let bookkeep_result = final_ctx
        .nodes
        .get("LeanBookkeepNode")
        .expect("LeanBookkeepNode should have stamped a result");
    assert_eq!(bookkeep_result["full_run"], json!(true));

    let state_json = read_state_json(&worktree);
    assert_eq!(state_json["status"], json!("done"));
    assert!(state_json["bail_reason"].is_null());

    // The per-task commits landed — two `SaveStateNode` commit invocations,
    // one per task, none reverted.
    let calls = git_calls.lock().unwrap();
    let commit_calls = calls.iter().filter(|c| c.contains("commit")).count();
    assert!(
        commit_calls >= 2,
        "expected at least 2 commit invocations (one per task), got: {calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("revert")));

    // Dispatch order sanity: the task loop ran before the drain tail.
    let order = order.lock().unwrap();
    let final_validation_pos = order
        .iter()
        .position(|n| n == "FinalValidationNode")
        .expect("FinalValidationNode dispatched");
    let lean_bookkeep_pos = order
        .iter()
        .position(|n| n == "LeanBookkeepNode")
        .expect("LeanBookkeepNode dispatched");
    assert!(final_validation_pos < lean_bookkeep_pos);
}

// --- MAJOR_BAIL: status "blocked" ------------------------------------------

#[tokio::test]
async fn major_bail_at_max_attempts_writes_blocked() {
    let worktree = temp_worktree("bail");
    let max_attempts = 2;
    write_fixture_files(&worktree, 1, max_attempts);

    let (git_runner, _git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, always_fail_runner(), git_runner);

    let event = json!({ "spec_slug": SPEC_SLUG });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let triage_result = final_ctx
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("MAJOR_BAIL"));

    // MAJOR_BAIL routes straight to LeanBookkeepNode — never through
    // UpdateTaskStatusNode, and FinalValidationNode never runs.
    assert!(!final_ctx.nodes.contains_key("UpdateTaskStatusNode"));
    assert!(
        !final_ctx.nodes.contains_key("FinalValidationNode"),
        "the reconcile must never run on a bailed run"
    );

    for identity in ["LeanBookkeepNode", "CloseBlockNode", "EmitStateNode"] {
        let run = final_ctx
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("expected a NodeRun for '{identity}'"));
        assert_eq!(run.status, NodeRunStatus::Success);
    }

    let state_json = read_state_json(&worktree);
    assert_eq!(state_json["status"], json!("blocked"));
    assert!(state_json["bail_reason"].as_str().is_some());

    let close_result = final_ctx
        .nodes
        .get("CloseBlockNode")
        .expect("CloseBlockNode should have stamped a result");
    assert!(
        close_result["outcome"]
            .as_str()
            .unwrap()
            .starts_with("SKIPPED"),
        "a blocked run must not close its block: {close_result:?}"
    );
}

// --- D56 reconcile: status "reconcile_failed", terminal --------------------

#[tokio::test]
async fn failing_reconcile_writes_reconcile_failed_and_is_terminal() {
    let worktree = temp_worktree("reconcile-failed");
    write_fixture_files(&worktree, 1, 2);

    let (git_runner, git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, passes_fast_fails_full_runner(), git_runner);

    let (recorder, order) = OrderRecorder::new();
    let event = json!({ "spec_slug": SPEC_SLUG });
    let final_ctx = workflow
        .run(event, recorder.callback())
        .await
        .expect("workflow run should not error");

    // The per-task fast tripwire passed (fast-check), so the task loop
    // completed cleanly and reached the drain branch.
    let triage_result = final_ctx
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("PASS"));

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["all_passed"], json!(false));
    // `run_and_stamp`'s `failure_summary` names the failed CHECK, not its
    // command string — the harness fixture's single check is named "tests".
    assert!(final_validation_result["failure_summary"]
        .as_str()
        .unwrap()
        .contains("tests"));

    // --- status: reconcile_failed, bookkeep flip skipped, commits stand ---
    let state_json = read_state_json(&worktree);
    assert_eq!(state_json["status"], json!("reconcile_failed"));
    assert!(state_json["bail_reason"]
        .as_str()
        .unwrap()
        .contains("tests"));

    let close_result = final_ctx
        .nodes
        .get("CloseBlockNode")
        .expect("CloseBlockNode should have stamped a result");
    let outcome = close_result["outcome"].as_str().unwrap();
    assert!(
        outcome.starts_with("SKIPPED"),
        "a failed reconcile must not close its block: {close_result:?}"
    );
    let detail = close_result["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("reconcile_failed") || detail.contains("D56"),
        "the skip reason should name the D56 reconcile-failed path: {detail}"
    );

    // The per-task commit (from SaveStateNode, task 1's PASS) still landed —
    // "leaves the per-task commits standing" is not merely the absence of a
    // revert, it is a positive commit call having happened.
    let calls = git_calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| c.contains("commit")),
        "expected the per-task commit to have landed: {calls:?}"
    );
    assert!(!calls
        .iter()
        .any(|c| c.contains("revert") || c.contains("reset")));

    // --- reconcile_failed is TERMINAL: nothing in the task loop re-enters --
    // Interpreted at the graph-walk level (D56 CALL 2): once
    // `LeanBookkeepNode` has run, only the fixed terminal tail
    // (`CloseBlockNode` -> `EmitStateNode`, both of which self-skip/no-op
    // rather than perform their real effect) may still dispatch — no
    // task-loop node (which would imply the chain kept going) ever runs
    // again.
    let order = order.lock().unwrap();
    let lean_bookkeep_pos = order
        .iter()
        .position(|n| n == "LeanBookkeepNode")
        .expect("LeanBookkeepNode dispatched");
    let allowed_after: [&str; 2] = ["CloseBlockNode", "EmitStateNode"];
    for identity in order.iter().skip(lean_bookkeep_pos + 1) {
        assert!(
            allowed_after.contains(&identity.as_str()),
            "'{identity}' dispatched after LeanBookkeepNode on a reconcile_failed run — the \
             chain must stop there per D56 CALL 2. Full order: {order:?}"
        );
    }
    // LeanBookkeepNode itself only ever dispatches once.
    assert_eq!(order.iter().filter(|n| *n == "LeanBookkeepNode").count(), 1);
}

// --- --resume on an all-passed set: only the reconcile re-runs -------------

#[tokio::test]
async fn resume_on_all_passed_set_reruns_only_the_reconcile() {
    let worktree = temp_worktree("resume");
    write_fixture_files(&worktree, 1, 2);

    // First run: a clean full pass, committing the state file with every
    // task Done.
    let (git_runner_1, _calls_1) = recording_git_runner();
    let workflow_1 = build_workflow(&worktree, always_pass_runner(), git_runner_1);
    let first_ctx = workflow_1
        .run(
            json!({ "spec_slug": SPEC_SLUG }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("first run should not error");
    assert_eq!(
        first_ctx
            .nodes
            .get("LeanBookkeepNode")
            .and_then(|r| r.get("state"))
            .and_then(|s| s.get("global_status"))
            .and_then(|v| v.as_str()),
        Some("done")
    );

    // Second run: `resume: true` against the SAME worktree. `LoadTaskStateNode`
    // loads the existing (all-Done) state file rather than bootstrapping
    // from `tasks.json`, so `TaskQueueRouterNode` finds no PENDING task and
    // routes straight to the drain branch — `ImplementTaskNode` is never
    // dispatched at all.
    let (git_runner_2, _calls_2) = recording_git_runner();
    let (test_runner_2, reconcile_calls) = {
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
    };
    let workflow_2 = build_workflow(&worktree, test_runner_2, git_runner_2);

    let second_ctx = workflow_2
        .run(
            json!({ "spec_slug": SPEC_SLUG, "resume": true }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("resume run should not error");

    // Zero ImplementTaskNode invocations: it stayed Pending, never Running.
    let implement_run = second_ctx
        .node_runs
        .get("ImplementTaskNode")
        .expect("ImplementTaskNode is a schema node, seeded Pending up front");
    assert_eq!(
        implement_run.status,
        NodeRunStatus::Pending,
        "ImplementTaskNode must never have dispatched on an all-passed --resume run"
    );
    assert!(!second_ctx.nodes.contains_key("ImplementTaskNode"));
    assert!(!second_ctx.nodes.contains_key("TestTaskNode"));
    assert!(!second_ctx.nodes.contains_key("TriageTaskNode"));

    // Exactly one reconcile: FinalValidationNode ran to success once, and
    // the recording runner it shares with TestTaskNode saw exactly the one
    // authoritative `full-suite-check` invocation the reconcile issues (no
    // per-task `fast-check` calls at all, since the task loop never ran).
    let final_validation_run = second_ctx
        .node_runs
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have run on the resumed drain branch");
    assert_eq!(final_validation_run.status, NodeRunStatus::Success);

    let calls = reconcile_calls.lock().unwrap();
    let full_suite_calls = calls
        .iter()
        .filter(|c| c.contains("full-suite-check"))
        .count();
    assert_eq!(
        full_suite_calls, 1,
        "expected exactly one reconcile invocation, got: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("fast-check")),
        "no per-task tripwire should have run on a resume with no pending tasks: {calls:?}"
    );

    let state_json = read_state_json(&worktree);
    assert_eq!(state_json["status"], json!("done"));
}

// --- EN.11.O task 6: each profile changes a DIFFERENT observable bound ----
//
// Per the block record and this task's spec: a resolved-VALUE assertion
// alone is a check whose inputs both come from the artifact under test
// (`gate-scope-must-be-shown-capable-of-failing`). Every assertion below
// counts something an INJECTED seam observed — `CommandRunner` calls at
// the reconcile, or `ImplementTaskNode` transport invocations under a
// never-passing task — never a resolved `SdlcTaskPolicy`/`SdlcPolicy`
// field.
//
// Retry-ceiling arithmetic: `retry_bail_fires_at_exactly_max_attempts_via_
// triage_back_edge` (`sdlc_flow/task_loop.rs`) pins that `TriageTaskNode`'s
// bail gate reads `attempt_count >= max_attempts` BEFORE this attempt's
// increment, so a never-passing task dispatches `ImplementTaskNode` at
// `attempt_count` 0, 1, ..., `max_attempts` inclusive — `max_attempts + 1`
// calls in total — before `MAJOR_BAIL` fires on the `max_attempts`-th
// triage with no further increment. `baseline` (3) -> 4 calls,
// `cheap-fast` (2) -> 3 calls, `thorough` (5) -> 6 calls: three different
// counts, one per profile.

#[tokio::test]
async fn baseline_profile_skips_reconcile_and_uses_max_attempts_three() {
    // Half 1: an all-passed run under `baseline` skips the reconcile
    // entirely (zero check results stamped by `FinalValidationNode`) —
    // `baseline` restates the built-in `test_depth: Full`, which is
    // `FinalValidationNode`'s Reconcile-scope skip condition 1. NOTE:
    // `test_depth: Full` also means the shared per-task tripwire
    // (`TestTaskNode`) itself invokes the authoritative `full-suite-check`
    // command per task (no `fastCommand` substitution at `Full`), so the
    // raw `CommandRunner` call log is NOT the right observable here — it
    // would conflate per-task calls with a reconcile call. The stamped
    // `FinalValidationNode` result (`skipped` / `check_results`) is the
    // one place that isolates the reconcile's own zero-call behaviour.
    let worktree = temp_worktree("baseline-reconcile");
    write_fixture_files(&worktree, 2, 2);

    let (test_runner, _calls) = recording_pass_runner();
    let (git_runner, _git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, test_runner, git_runner);

    let event = json!({ "spec_slug": SPEC_SLUG, "profile": "baseline" });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["skipped"], json!(true));
    assert_eq!(
        final_validation_result["check_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "zero CommandRunner calls at the reconcile: check_results must be empty on the skip path"
    );

    // Half 2: a never-passing task under `baseline`, with `max_attempts`
    // seeded from the resolved policy (task's own JSON omits it), bails
    // after exactly 4 `ImplementTaskNode` invocations (3 + 1).
    let worktree2 = temp_worktree("baseline-retry");
    write_fixture_files_policy_driven_max_attempts(&worktree2, 1);
    let (git_runner2, _git_calls2) = recording_git_runner();
    let (workflow2, implement_calls) =
        build_workflow_counting_implement(&worktree2, always_fail_runner(), git_runner2);

    let event2 = json!({ "spec_slug": SPEC_SLUG, "profile": "baseline" });
    let final_ctx2 = workflow2
        .run(event2, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let triage_result = final_ctx2
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("MAJOR_BAIL"));
    assert_eq!(
        implement_calls.load(Ordering::SeqCst),
        4,
        "baseline's max_attempts (3) should yield 4 ImplementTaskNode calls before MAJOR_BAIL"
    );
}

#[tokio::test]
async fn cheap_fast_profile_runs_reconcile_and_uses_max_attempts_two() {
    // Half 1: an all-passed run under `cheap-fast` (`test_depth: Fast`)
    // actually SELECTS AND RUNS the reconcile — a non-zero `CommandRunner`
    // call count at `FinalValidationNode`, the opposite of `baseline`'s
    // half above.
    let worktree = temp_worktree("cheap-fast-reconcile");
    write_fixture_files(&worktree, 2, 2);

    let (test_runner, reconcile_calls) = recording_pass_runner();
    let (git_runner, _git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, test_runner, git_runner);

    let event = json!({ "spec_slug": SPEC_SLUG, "profile": "cheap-fast" });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["skipped"], json!(false));
    {
        let calls = reconcile_calls.lock().unwrap();
        let full_suite_calls = calls
            .iter()
            .filter(|c| c.contains("full-suite-check"))
            .count();
        assert!(
            full_suite_calls > 0,
            "cheap-fast (test_depth: Fast) must actually run the reconcile: {calls:?}"
        );
    }

    // Half 2: a never-passing task under `cheap-fast` bails after exactly
    // 3 `ImplementTaskNode` invocations (2 + 1) — fewer than `baseline`'s
    // 4 and `thorough`'s 6, proving the resolved `max_attempts: 2` is
    // actually read by the retry loop.
    let worktree2 = temp_worktree("cheap-fast-retry");
    write_fixture_files_policy_driven_max_attempts(&worktree2, 1);
    let (git_runner2, _git_calls2) = recording_git_runner();
    let (workflow2, implement_calls) =
        build_workflow_counting_implement(&worktree2, always_fail_runner(), git_runner2);

    let event2 = json!({ "spec_slug": SPEC_SLUG, "profile": "cheap-fast" });
    let final_ctx2 = workflow2
        .run(event2, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let triage_result = final_ctx2
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("MAJOR_BAIL"));
    assert_eq!(
        implement_calls.load(Ordering::SeqCst),
        3,
        "cheap-fast's max_attempts (2) should yield 3 ImplementTaskNode calls before MAJOR_BAIL"
    );
}

#[tokio::test]
async fn thorough_profile_skips_reconcile_like_baseline_but_uses_max_attempts_five() {
    // Half 1: `thorough` keeps the built-in `test_depth: Full`, so its
    // reconcile is skipped exactly like `baseline`'s — zero check results
    // stamped by `FinalValidationNode` — even though every other knob
    // (model tier, timeouts, retry ceiling) differs. See `baseline`'s
    // Half-1 comment for why the stamped result, not the raw
    // `CommandRunner` call log, is the right observable at `Full` depth
    // (the per-task tripwire itself calls `full-suite-check` there too).
    let worktree = temp_worktree("thorough-reconcile");
    write_fixture_files(&worktree, 2, 2);

    let (test_runner, _calls) = recording_pass_runner();
    let (git_runner, _git_calls) = recording_git_runner();
    let workflow = build_workflow(&worktree, test_runner, git_runner);

    let event = json!({ "spec_slug": SPEC_SLUG, "profile": "thorough" });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["skipped"], json!(true));
    assert_eq!(
        final_validation_result["check_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "zero CommandRunner calls at the reconcile: check_results must be empty on the skip path"
    );

    // Half 2: a never-passing task under `thorough` bails after exactly 6
    // `ImplementTaskNode` invocations (5 + 1) — the highest of the three
    // profiles, distinct from both `baseline` (4) and `cheap-fast` (3).
    let worktree2 = temp_worktree("thorough-retry");
    write_fixture_files_policy_driven_max_attempts(&worktree2, 1);
    let (git_runner2, _git_calls2) = recording_git_runner();
    let (workflow2, implement_calls) =
        build_workflow_counting_implement(&worktree2, always_fail_runner(), git_runner2);

    let event2 = json!({ "spec_slug": SPEC_SLUG, "profile": "thorough" });
    let final_ctx2 = workflow2
        .run(event2, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let triage_result = final_ctx2
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["verdict"], json!("MAJOR_BAIL"));
    assert_eq!(
        implement_calls.load(Ordering::SeqCst),
        6,
        "thorough's max_attempts (5) should yield 6 ImplementTaskNode calls before MAJOR_BAIL"
    );
}

/// Pins the trap the block record names twice over (`policy.rs`,
/// `profiles.rs`): `test_depth: Full` — the built-in default, restated by
/// both `baseline` and `thorough` — SILENTLY DISABLES the terminal
/// reconcile. Reached through SDLC_TASK's OWN registry/schema (this
/// suite's `build_workflow`, not a direct unit call into
/// `final_validation.rs`), so this side cannot drift unnoticed from the
/// mirrored `final_validation.rs` unit test
/// (`reconcile_scope_skips_with_zero_calls_when_test_depth_is_full`).
#[tokio::test]
async fn test_depth_full_reconcile_skip_pins_zero_calls_and_skip_reason_through_sdlc_task_registry()
{
    let worktree = temp_worktree("test-depth-full-pin");
    // `write_fixture_files_policy_driven_max_attempts` (not
    // `write_fixture_files`) deliberately carries NO `sdlc_task.policy`
    // section — `write_fixture_files`'s harness pins `test_depth: "fast"`
    // as a layer-3 default (for the pre-existing, non-profile tests in
    // this file), which would mask the built-in default this pin exists
    // to exercise.
    write_fixture_files_policy_driven_max_attempts(&worktree, 1);

    let (git_runner, _git_calls) = recording_git_runner();
    // No `profile` in the event at all — the built-in default IS
    // `test_depth: Full`, so this exercises the no-profile-selected path
    // the behaviour-stability guarantee (task 1) anchors on.
    let workflow = build_workflow(&worktree, always_pass_runner(), git_runner);

    let event = json!({ "spec_slug": SPEC_SLUG });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let final_validation_result = final_ctx
        .nodes
        .get("FinalValidationNode")
        .expect("FinalValidationNode should have stamped a result");
    assert_eq!(final_validation_result["skipped"], json!(true));
    assert_eq!(
        final_validation_result["check_results"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "zero CommandRunner calls: check_results must be empty on the skip path"
    );
    assert!(
        final_validation_result["skip_reason"]
            .as_str()
            .unwrap()
            .contains("test_depth=full"),
        "skip_reason must name test_depth=full: {final_validation_result:?}"
    );
}

// ── EN.11.P task 5: ORCHESTRATION driving a mixed `task` + `flow` chain ────
//
// Everything above this point drives the assembled SDLC_TASK graph in
// isolation, one block at a time. These three tests exercise a different
// subsystem entirely — `orchestration::graph::OrchestrationRunNode`, which
// drives a CHAIN of blocks, each dispatched to whichever `EngineKind` it
// resolves to (`task` or `flow`) — and are placed in this file only
// because that is this task's declared file scope, not because they share
// fixtures with the tests above.
//
// HONESTY REQUIREMENT (carried from `EN.11.P`'s `carryover_context`,
// `orchestration-workflow-never-driven-a-real-chain`): every test below
// runs against a tempdir fixture repo with a stubbed `FlowRunner` — no
// real `/sdlc-task` or `/sdlc-flow` process is ever spawned, and no real
// engine-serve HTTP endpoint is hit. A green result here pins the
// DISPATCH and INTEGRATION contract (which engine a block resolves to,
// whether the chain stops on a failed reconcile, `steps_integrated`'s
// count) — it is NOT evidence of having driven a real mixed chain, and
// the carryover must not be reported as cleared on the strength of these
// tests. The block's acceptance criterion is phrased as a curl against a
// live engine (`GET /campaigns/$ID` -> `steps_integrated == 2`); that curl
// form was NOT run against a live engine as part of this task — only the
// in-repo integration-test equivalent below was.

/// Mirrors `orchestration::graph`'s own `two_repo_brain_root` test helper
/// (duplicated rather than shared, like `orchestration_chain.rs`'s
/// `cancel_after_block` comment explains — this file is a different crate
/// target). One fixture repo is enough for these tests: every chain step
/// below targets the same repo slug with different block ids.
fn orchestration_brain_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::create_dir_all(
        dir.path()
            .join("planning")
            .join("roadmaps")
            .join("my-roadmap"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    dir
}

/// Writes the state file `verify_state_write` reads for `block_id`, at the
/// path shape `integrate.rs`'s own (private) `state_path_for` mirrors:
/// `planning/{block_id}/sdlc/{filename}`, where `filename` is
/// `sdlc-flow-state.json` for [`EngineKind::Flow`] and
/// `sdlc-task-state.json` (this file's own `DEFAULT_STATE_FILENAME`) for
/// [`EngineKind::Task`]. `extra` is merged onto the base `{"status": ...}`
/// object, e.g. for `"bail_reason"` on a reconcile-failed fixture.
fn write_orchestration_state(
    repo_path: &Path,
    block_id: &str,
    engine: EngineKind,
    status: &str,
    extra: serde_json::Value,
) {
    let filename = match engine {
        EngineKind::Flow => "sdlc-flow-state.json",
        EngineKind::Task => DEFAULT_STATE_FILENAME,
    };
    let dir = repo_path.join("planning").join(block_id).join("sdlc");
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = json!({ "status": status });
    if let serde_json::Value::Object(extra_map) = extra {
        if let serde_json::Value::Object(body_map) = &mut body {
            body_map.extend(extra_map);
        }
    }
    std::fs::write(dir.join(filename), body.to_string()).unwrap();
}

/// `(repo, block_id, engine)` for one recorded [`FlowRunner`] invocation.
type OrchestrationCall = (String, String, EngineKind);

/// A [`FlowRunner`] stub that records every invocation (repo, block_id,
/// engine) without spawning anything real — the actual per-step outcome
/// this suite cares about comes from the state file
/// `write_orchestration_state` pre-seeds on disk, exactly like
/// `orchestration::graph`'s own fixture tests (`write_done_state`).
fn recording_orchestration_runner() -> (FlowRunner, Arc<Mutex<Vec<OrchestrationCall>>>) {
    let calls: Arc<Mutex<Vec<OrchestrationCall>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let runner: FlowRunner = Arc::new(move |invocation: FlowInvocation| {
        recorded.lock().unwrap().push((
            invocation.repo.clone(),
            invocation.block_id.clone(),
            invocation.engine,
        ));
        Box::pin(async move {
            Ok(TaskContext {
                event: json!({}),
                nodes: HashMap::new(),
                metadata: json!({ "ran": invocation.block_id }),
                node_runs: HashMap::new(),
            })
        })
    });
    (runner, calls)
}

/// THE PAYOFF CASE: a two-block chain of one `task` block and one `flow`
/// block completes, `steps_integrated == 2`. This is the in-repo pin for
/// the block's `curl -s $ENGINE/campaigns/$ID | jq -e '.steps_integrated
/// == 2'` acceptance criterion — see the HONESTY REQUIREMENT comment above
/// this section: the curl form itself was NOT exercised against a live
/// engine.
#[tokio::test]
async fn mixed_task_and_flow_chain_completes_with_steps_integrated_two() {
    let dir = orchestration_brain_root();
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "T.1",
        EngineKind::Task,
        "done",
        json!({}),
    );
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "F.1",
        EngineKind::Flow,
        "done",
        json!({}),
    );

    let (run_flow, calls) = recording_orchestration_runner();
    let resolve_engine = |_repo: &str, block_id: &str| {
        if block_id == "T.1" {
            EngineKind::Task
        } else {
            EngineKind::Flow
        }
    };

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_resolve_engine(Arc::new(resolve_engine));
    let ctx = TaskContext {
        event: json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "T.1" },
                { "repo": "repo-a", "block_id": "F.1" }
            ],
            "roadmap_slug": "my-roadmap",
        }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    let out = node
        .process(ctx)
        .await
        .expect("a mixed task+flow chain should complete");
    let recorded = &out.nodes[ORCH_NODE_NAME];
    assert_eq!(recorded["steps_integrated"], 2);
    assert_eq!(recorded["cancellation"]["cancelled"], false);

    // Both blocks actually dispatched to their resolved engine, not merely
    // "some engine" — the mix is what this test pins.
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        ("repo-a".to_string(), "T.1".to_string(), EngineKind::Task)
    );
    assert_eq!(
        calls[1],
        ("repo-a".to_string(), "F.1".to_string(), EngineKind::Flow)
    );
}

/// A `task` block whose reconcile failed surfaces
/// `IntegrateError::ReconcileFailed` and STOPS the chain. Asserted on the
/// chain actually stopping (the second, `flow`, step never dispatches at
/// all), not merely on the error variant/message — a chain that dispatched
/// every step and only afterward reported an error would NOT satisfy this.
#[tokio::test]
async fn task_block_reconcile_failure_stops_the_chain() {
    let dir = orchestration_brain_root();
    // T.1 (task engine) reconciled and failed.
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "T.1",
        EngineKind::Task,
        "reconcile_failed",
        json!({ "bail_reason": "full-suite-check failed: tests" }),
    );
    // F.2 (flow engine) is a perfectly fine "would have passed" block — its
    // state file is seeded exactly like the payoff case's, so if the chain
    // wrongly kept going after T.1, this step would complete and mask the
    // stop-on-failure behavior this test exists to pin.
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "F.2",
        EngineKind::Flow,
        "done",
        json!({}),
    );

    let (run_flow, calls) = recording_orchestration_runner();
    let resolve_engine = |_repo: &str, block_id: &str| {
        if block_id == "T.1" {
            EngineKind::Task
        } else {
            EngineKind::Flow
        }
    };

    let node = OrchestrationRunNode::new()
        .with_run_flow(run_flow)
        .with_resolve_engine(Arc::new(resolve_engine));
    let ctx = TaskContext {
        event: json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "T.1" },
                { "repo": "repo-a", "block_id": "F.2" }
            ],
            "roadmap_slug": "my-roadmap",
        }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    let err = node
        .process(ctx)
        .await
        .expect_err("a failed reconcile must stop the chain with an Err, not a partial success");
    assert!(
        err.message.contains("reconcile_failed") || err.message.contains("reconcile failed"),
        "error should name the reconcile failure: {}",
        err.message
    );
    assert!(
        err.message.contains("T.1"),
        "error should name the failing block: {}",
        err.message
    );

    // The chain actually stopped: F.2 was never dispatched.
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "the flow block after the failed reconcile must never have dispatched: {calls:?}"
    );
    assert_eq!(calls[0].1, "T.1");
}

/// Regression guard: a `flow`-only chain behaves exactly as before this
/// block's engine-mixing dispatch existed — every block resolves to
/// [`EngineKind::Flow`] (the [`OrchestrationRunNode::new`] default, not
/// overridden here), and the chain completes with `steps_integrated == 2`
/// like any pre-`EN.11.P` two-block flow chain.
#[tokio::test]
async fn flow_only_chain_is_unaffected_by_task_engine_dispatch() {
    let dir = orchestration_brain_root();
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "F.1",
        EngineKind::Flow,
        "done",
        json!({}),
    );
    write_orchestration_state(
        &dir.path().join("repo-a"),
        "F.2",
        EngineKind::Flow,
        "done",
        json!({}),
    );

    let (run_flow, calls) = recording_orchestration_runner();

    // No `with_resolve_engine` override: the built-in default resolves
    // every block to `EngineKind::Flow`, exactly as it did before this
    // block introduced `EngineKind::Task` dispatch.
    let node = OrchestrationRunNode::new().with_run_flow(run_flow);
    let ctx = TaskContext {
        event: json!({
            "brain_root": dir.path(),
            "blocks": [
                { "repo": "repo-a", "block_id": "F.1" },
                { "repo": "repo-a", "block_id": "F.2" }
            ],
            "roadmap_slug": "my-roadmap",
        }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    let out = node
        .process(ctx)
        .await
        .expect("an unmodified flow-only chain should still complete");
    let recorded = &out.nodes[ORCH_NODE_NAME];
    assert_eq!(recorded["steps_integrated"], 2);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|(_, _, engine)| *engine == EngineKind::Flow));
}
