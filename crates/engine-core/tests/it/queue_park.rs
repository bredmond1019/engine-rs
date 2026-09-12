//! Integration tests for `EN.17.J` task 5: `queue_park::drive` driving the
//! REAL, policy-resolved `SDLC_TASK`/`SDLC_FLOW` graphs (`registry_for_
//! policy`), not the minimal hand-built schemas `workflows::queue_park`'s own
//! unit tests use — the seam a `test_dispatch: queue_park` run actually
//! walks through in this repo. Every model/subprocess seam is stubbed
//! (`ImplementTaskNode`'s transport, `TriageTaskNode`'s git runner) exactly
//! like `sdlc_task_e2e.rs`/`sdlc_flow_e2e.rs`; the only REAL machinery left
//! running is `coord::heavy_work::HeavyWorkQueue`'s admission/store — the
//! thing under test.
//!
//! Every test name is prefixed `queue_park_` per the task's own naming
//! convention.
//!
//! **What `drive`'s injected outcome is, and is not, coupled to.** Every
//! test below drives the walk through [`queue_park::drive`] with its OWN
//! [`ScriptedHeavyJobLookup`] — a test double completely decoupled from
//! whatever the REAL `HeavyWorkQueue`-admitted job (submitted for real by
//! `TestTaskNode::process`'s `QueuePark` branch) eventually produces. This
//! mirrors `workflows::queue_park`'s own unit tests (`StubQueue`) rather than
//! `default_flow_runner`'s production `DiskHeavyJobLookup`, and for the same
//! reason spelled out on that struct: `TestTaskNode` mints the walk's own
//! correlation `job_id` before submission, which is NOT the id
//! `HeavyWorkQueue::enqueue` persists on disk, so nothing today can bridge
//! the two. What IS real and asserted directly against the shared
//! `HeavyWorkQueue`: admission bounded by a real per-class `limit` (used by
//! [`queue_park_abort_while_queued_never_runs_checks`] to prove the check
//! runner is never invoked while genuinely queued, not merely "our stub
//! never ran it").
//!
//! **`queue_park_orchestration_child_resumes_through_default_flow_runner`'s
//! own HONESTY REQUIREMENT** (the same convention `sdlc_task_e2e.rs`'s own
//! ORCHESTRATION section carries): `default_flow_runner`'s REAL Flow/Task
//! arms dispatch through `registry_for_policy`, which wires the REAL cloud
//! `ImplementTaskNode`/`TriageTaskNode` transports (`agentic_write_config`,
//! `real_cloud_transport`) — there is no injection seam for those at that
//! layer, so a hermetic test cannot drive a real Claude Code call. That test
//! therefore proves the CAUSAL MECHANISM `default_flow_runner_with_heavy_
//! work` was changed to use — a `FlowRunner` that drives a queue-parked
//! child through `queue_park::drive` before ever handing `execute_step` a
//! ctx — against a minimal, fully injectable workflow, rather than the real
//! assembled SDLC_TASK graph. `default_flow_runner_with_heavy_work`'s own
//! wiring (calling `queue_park::drive` instead of `Workflow::run_with`) is
//! reviewed directly in `execute.rs`; this test is not evidence of having
//! driven a real cloud-backed child through it.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use claude_code_rs::Outcome;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::cancellation::CancellationToken;
use engine_core::coord::heavy_work::{ClassLimit, HeavyWorkConfig, HeavyWorkQueue};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::policy::emit_state::EmitStateNode as GenericEmitStateNode;
use engine_core::policy::PolicyConfigSource;
use engine_core::repo_registry::RepoRegistry;
use engine_core::schema::{NodeConfig, WorkflowSchema};
use engine_core::suspend::{self, SuspendReason};
use engine_core::workflow::{OnProgress, Workflow, WorkflowError};
use engine_core::workflows::orchestration::execute::{
    default_flow_runner_with_heavy_work, execute_step, EngineKind, FlowInvocation,
};
use engine_core::workflows::queue_park::{self, HeavyJobLookup, RunStart};
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode as FlowEmitStateNode;
use engine_core::workflows::sdlc_flow::end_review::{EndReviewNode, EndReviewRouterNode};
use engine_core::workflows::sdlc_flow::final_validation::{FinalValidationNode, ValidationScope};
use engine_core::workflows::sdlc_flow::graph as sdlc_flow_graph;
use engine_core::workflows::sdlc_flow::pr::PullRequestNode;
use engine_core::workflows::sdlc_flow::setup::{
    self as flow_setup, CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode,
    SpecExistsRouterNode,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, IncrementAttemptNode, ReviewRouterNode,
    SaveStateNode, TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode,
    UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use engine_core::workflows::sdlc_task::graph as sdlc_task_graph;
use engine_core::workflows::sdlc_task::lean_bookkeep::LeanBookkeepNode;
use engine_core::workflows::sdlc_task::profiles::resolve_policy_for_run_from;
use engine_core::workflows::sdlc_task::task_triage_router::TaskTriageRouterNode;
use engine_core::workflows::sdlc_task::DEFAULT_STATE_FILENAME;
use serde_json::json;
use uuid::Uuid;

const TASK_SLUG: &str = "fixture-queue-park-task";
const FLOW_SLUG: &str = "fixture-queue-park-flow";

// ── shared fixture plumbing ────────────────────────────────────────────────

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-queue-park-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// A `HeavyWorkQueue` over a fresh tempdir lock dir, with `class` admitted
/// at `limit` — mirrors `task_loop.rs`'s own private
/// `enabled_heavy_work_queue` test helper (not reusable across test
/// binaries, so duplicated here).
fn enabled_heavy_work_queue(lock_dir: &Path, class: &str, limit: usize) -> HeavyWorkQueue {
    let mut classes = HashMap::new();
    classes.insert(
        class.to_string(),
        ClassLimit {
            limit,
            min_free_mb: 0,
        },
    );
    let config = HeavyWorkConfig {
        enabled: true,
        heartbeat_interval_secs: 60,
        stale_after_secs: 300,
        poll_interval_ms: 5,
        classes,
    };
    HeavyWorkQueue::new(lock_dir.to_path_buf(), config)
}

/// A `HeavyJobLookup` test double fully decoupled from any real
/// `HeavyWorkQueue` job (see the module doc's "what drive's injected outcome
/// is, and is not, coupled to"). Resolves `await_outcome` with the next
/// scripted outcome (repeating the last one once the script is exhausted,
/// so a multi-park walk — e.g. a second task's own `TestTaskNode` — never
/// panics on an empty queue), unless `hang` is set, in which case it never
/// resolves at all (the abort/cancel path).
struct ScriptedHeavyJobLookup {
    outcomes: Mutex<VecDeque<serde_json::Value>>,
    hang: bool,
    delay: Option<Duration>,
    await_calls: AtomicUsize,
    cancelled_job_ids: Mutex<Vec<Uuid>>,
}

impl ScriptedHeavyJobLookup {
    fn new(outcomes: Vec<serde_json::Value>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            hang: false,
            delay: None,
            await_calls: AtomicUsize::new(0),
            cancelled_job_ids: Mutex::new(Vec::new()),
        })
    }

    fn with_delay(outcomes: Vec<serde_json::Value>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            hang: false,
            delay: Some(delay),
            await_calls: AtomicUsize::new(0),
            cancelled_job_ids: Mutex::new(Vec::new()),
        })
    }

    fn hanging() -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(VecDeque::new()),
            hang: true,
            delay: None,
            await_calls: AtomicUsize::new(0),
            cancelled_job_ids: Mutex::new(Vec::new()),
        })
    }

    #[allow(dead_code)]
    fn await_call_count(&self) -> usize {
        self.await_calls.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn cancelled_count(&self) -> usize {
        self.cancelled_job_ids.lock().unwrap().len()
    }
}

#[async_trait]
impl HeavyJobLookup for ScriptedHeavyJobLookup {
    async fn await_outcome(&self, _job_id: Uuid) -> serde_json::Value {
        self.await_calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            // Never resolves: `drive`'s `tokio::select!` must pick the
            // cancellation branch instead.
            std::future::pending::<()>().await;
            unreachable!("hanging ScriptedHeavyJobLookup::await_outcome must never resolve");
        }
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let mut queue = self.outcomes.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().expect("checked non-empty above")
        } else {
            queue
                .front()
                .cloned()
                .expect("ScriptedHeavyJobLookup must be seeded with at least one outcome")
        }
    }

    async fn cancel_if_queued(&self, job_id: Uuid) {
        self.cancelled_job_ids.lock().unwrap().push(job_id);
    }
}

/// The exact 7-key shape `TestTaskNode::process`'s inline path stamps
/// (`workflows::sdlc_flow::task_loop`) — every scripted outcome below
/// carries the SAME key set so `queue_park_injected_output_matches_inline_
/// shape` has something meaningful to compare against.
fn pass_outcome() -> serde_json::Value {
    json!({
        "all_passed": true,
        "check_results": [
            { "name": "tests", "passed": true, "message": "", "failure_class": "fixable" }
        ],
        "failure_summary": "",
        "test_depth": "fast",
        "check_source": "task",
        "excluded_checks": [],
        "heavy_work": {
            "mode": "enabled",
            "job_id": Uuid::new_v4().to_string(),
            "class": "test",
            "waited_ms": 0,
            "degraded": false,
        },
    })
}

fn fail_outcome() -> serde_json::Value {
    json!({
        "all_passed": false,
        "check_results": [
            {
                "name": "tests",
                "passed": false,
                "message": "boom",
                "output": "assertion failed: boom",
                "failure_class": "fixable",
            }
        ],
        "failure_summary": "Failed checks: tests",
        "test_depth": "fast",
        "check_source": "task",
        "excluded_checks": [],
        "heavy_work": {
            "mode": "enabled",
            "job_id": Uuid::new_v4().to_string(),
            "class": "test",
            "waited_ms": 0,
            "degraded": false,
        },
    })
}

// ── SDLC_TASK fixture (mirrors `sdlc_task_e2e.rs::build_workflow`) ─────────

struct TaskFixtureSetupNode {
    worktree_path: String,
}

#[async_trait]
impl Node for TaskFixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": format!("task/{TASK_SLUG}"),
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

/// Writes `<worktree>/planning/<TASK_SLUG>/tasks.json` (`task_count` PENDING
/// tasks) and `<worktree>/planning/harness.json` with
/// `sdlc_task.policy.test_dispatch: "queue_park"` — the knob this whole
/// suite exercises (task 2's policy plumbing, already committed on this
/// branch).
fn write_task_fixture_files(worktree: &Path, task_count: u32, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join(TASK_SLUG);
    std::fs::create_dir_all(&spec_dir).unwrap();
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
        "sdlc_task": {
            "policy": { "test_depth": "fast", "test_dispatch": "queue_park" }
        },
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

/// Builds the full assembled `SDLC_TASK` `Workflow` — mirrors
/// `sdlc_task_e2e.rs::build_workflow` exactly, except `TestTaskNode` is
/// additionally wired to `heavy_work` (`EN.17.J` task 5) instead of the
/// disabled default, and `ImplementTaskNode`'s transport is supplied by the
/// caller so a test can capture/count invocations and inspect the prompt
/// text (retry-feedback assertions).
fn build_task_workflow(
    worktree: &Path,
    heavy_work: HeavyWorkQueue,
    implement_transport: engine_core::workflows::sdlc_flow::ModelTransport,
    git_runner: CommandRunner,
) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(TaskFixtureSetupNode {
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

    registry.register(Box::new(
        ImplementTaskNode::new().with_transport(implement_transport),
    ));

    registry.register(Box::new(
        TestTaskNode::new()
            .with_runner(always_pass_runner())
            .with_heavy_work(heavy_work),
    ));
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

    registry.register(Box::new(
        FinalValidationNode::new()
            .with_runner(always_pass_runner())
            .with_scope(ValidationScope::Reconcile),
    ));

    registry.register(Box::new(
        LeanBookkeepNode::new()
            .with_runner(git_runner.clone())
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

    Workflow::new_validated(registry, sdlc_task_graph::schema())
        .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate")
}

fn implement_transport_recording(
    calls: Arc<Mutex<Vec<String>>>,
) -> engine_core::workflows::sdlc_flow::ModelTransport {
    Arc::new(move |_config, prompt| {
        calls.lock().unwrap().push(prompt.clone());
        let outcome = stub_outcome(
            &json!({
                "summary": "implemented",
                "modified_files": ["src/lib.rs"],
                "tests_added": ["it_works"],
            })
            .to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    })
}

/// An `on_progress` callback recording the dispatch order of every node
/// identity (first `Running` observation only) — mirrors
/// `sdlc_task_e2e.rs::OrderRecorder`.
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

fn read_task_state_json(worktree: &Path) -> serde_json::Value {
    let state_path = worktree
        .join("planning")
        .join(TASK_SLUG)
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

/// Drives `workflow` fresh through [`queue_park::drive`] with `heavy_work`,
/// mirroring the exact call shape `default_flow_runner_with_heavy_work`
/// (`execute.rs`) uses.
async fn drive_fresh(
    workflow: &Workflow,
    event: serde_json::Value,
    on_progress: OnProgress<'_>,
    heavy_work: Arc<dyn HeavyJobLookup>,
) -> Result<TaskContext, WorkflowError> {
    let cancel = CancellationToken::new();
    queue_park::drive(
        workflow,
        RunStart::Fresh {
            event,
            on_progress,
            options: engine_core::RunOptions::default(),
            heavy_work,
        },
        &cancel,
    )
    .await
}

// ── SDLC_TASK tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn queue_park_sdlc_task_suspends_at_test_stage() {
    let worktree = temp_dir("suspends");
    write_task_fixture_files(&worktree, 1, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    // Class limit 1, occupied by a holding job before the run starts — the
    // real submitted job (from `TestTaskNode::process`) must stay `Queued`,
    // never admitted, for the duration of this test.
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 1);
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holding = queue
        .submit(
            engine_core::coord::heavy_work::HeavyWorkSpec {
                class: "test".to_string(),
                repo: "fixture".to_string(),
                cwd: worktree.clone(),
                commands: vec![],
                run_id: None,
            },
            move || {
                release_rx.recv().ok();
            },
        )
        .await;

    let (test_runner_calls_dummy, _unused) = (Arc::new(Mutex::new(Vec::<String>::new())), ());
    let _ = test_runner_calls_dummy;

    let implement_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let workflow = build_task_workflow(
        &worktree,
        queue,
        implement_transport_recording(implement_calls.clone()),
        Arc::new(|_p, _a, _c| {
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }),
    );

    let (recorder, order) = OrderRecorder::new();
    let event = json!({ "spec_slug": TASK_SLUG });
    let cancel = CancellationToken::new();
    let heavy_work_lookup = ScriptedHeavyJobLookup::hanging();

    // Run just far enough to observe the suspension: race the (never-
    // resolving) drive loop against a short deadline, since this test wants
    // the SUSPENDED intermediate state, not a resumed/terminal one.
    let run = queue_park::drive(
        &workflow,
        RunStart::Fresh {
            event,
            on_progress: recorder.callback(),
            options: engine_core::RunOptions::default(),
            heavy_work: heavy_work_lookup.clone(),
        },
        &cancel,
    );
    let timed_out = tokio::time::timeout(Duration::from_millis(500), run)
        .await
        .is_err();
    assert!(
        timed_out,
        "drive should still be awaiting the queued job's outcome 500ms in"
    );

    assert!(
        order.lock().unwrap().contains(&"TestTaskNode".to_string()),
        "TestTaskNode must have dispatched before the park"
    );
    // Zero real check invocations while genuinely queued behind the holding
    // job — the acceptance criterion this test exists to pin.
    assert_eq!(
        implement_calls.lock().unwrap().len(),
        1,
        "ImplementTaskNode dispatches exactly once before TestTaskNode parks"
    );

    // Release the holding job and drop the queue cleanly.
    release_tx.send(()).ok();
    let _ = holding.await;
}

#[tokio::test]
async fn queue_park_pass_resumes_to_next_task_sdlc_task() {
    let worktree = temp_dir("pass-task");
    write_task_fixture_files(&worktree, 2, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 4);

    let implement_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let workflow = build_task_workflow(
        &worktree,
        queue,
        implement_transport_recording(implement_calls.clone()),
        always_pass_runner(),
    );

    let (recorder, order) = OrderRecorder::new();
    let heavy_work: Arc<dyn HeavyJobLookup> = ScriptedHeavyJobLookup::new(vec![pass_outcome()]);
    let event = json!({ "spec_slug": TASK_SLUG });

    let final_ctx = drive_fresh(&workflow, event, recorder.callback(), heavy_work)
        .await
        .expect("a two-task all-passing queue-park run should not error");

    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "succeeded"
    );
    assert!(
        !suspend::is_suspended(&final_ctx.metadata),
        "the final ctx must not still be suspended"
    );

    // `OrderRecorder` dedupes by identity (first `Running` observation
    // only, mirroring `sdlc_task_e2e.rs::OrderRecorder`) so a SECOND
    // dispatch of the same identity (task 2's own `ImplementTaskNode`/
    // `TaskQueueRouterNode`) is invisible to it — the transport's own call
    // log is the observable that actually counts repeats.
    let order = order.lock().unwrap();
    assert!(
        order.contains(&"TriageTaskNode".to_string()),
        "TriageTaskNode must have dispatched: {order:?}"
    );
    drop(order);

    // Task 2's prompt names the second task, proving the walk actually
    // advanced `current_task_id` rather than re-running task 1.
    let calls = implement_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "ImplementTaskNode must have dispatched once per task"
    );
    assert!(calls[1].contains("thing 2"), "prompt was: {}", calls[1]);
    drop(calls);

    let state_json = read_task_state_json(&worktree);
    assert_eq!(state_json["status"], json!("done"));
}

#[tokio::test]
async fn queue_park_fail_resumes_to_increment_attempt_sdlc_task() {
    let worktree = temp_dir("fail-task");
    write_task_fixture_files(&worktree, 1, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 4);

    let implement_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let workflow = build_task_workflow(
        &worktree,
        queue,
        implement_transport_recording(implement_calls.clone()),
        always_pass_runner(),
    );

    let (recorder, order) = OrderRecorder::new();
    // First park fails, second (the retry) passes.
    let heavy_work: Arc<dyn HeavyJobLookup> =
        ScriptedHeavyJobLookup::new(vec![fail_outcome(), pass_outcome()]);
    let event = json!({ "spec_slug": TASK_SLUG });

    let final_ctx = drive_fresh(&workflow, event, recorder.callback(), heavy_work)
        .await
        .expect("a fail-then-pass queue-park run should not error");
    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "succeeded"
    );

    let order = order.lock().unwrap();
    let first_triage_pos = order
        .iter()
        .position(|n| n == "TriageTaskNode")
        .expect("TriageTaskNode dispatched at least once");
    let increment_pos = order
        .iter()
        .position(|n| n == "IncrementAttemptNode")
        .expect("IncrementAttemptNode dispatched after the failing park");
    assert!(
        increment_pos > first_triage_pos,
        "IncrementAttemptNode must follow the first TriageTaskNode: {order:?}"
    );

    let calls = implement_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "task 1 implemented twice: the original attempt and the retry"
    );
    assert!(
        calls[1].contains("tests"),
        "the retry prompt must name the failing check ('tests'): {}",
        calls[1]
    );
}

#[tokio::test]
async fn queue_park_injected_output_matches_inline_shape() {
    // Inline run (test_dispatch left at the built-in Inline default).
    let inline_worktree = temp_dir("inline-shape");
    let spec_dir = inline_worktree.join("planning").join(TASK_SLUG);
    std::fs::create_dir_all(&spec_dir).unwrap();
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&json!([{
            "task_id": 1,
            "title": "Implement thing 1",
            "description": "Do the work",
            "acceptance_criteria": ["it works"],
            "max_attempts": 2,
        }]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        inline_worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&json!({
            "sdlc_task": { "policy": { "test_depth": "fast" } },
            "validation": { "checks": [
                { "name": "tests", "kind": "command", "command": "full-suite-check",
                  "fastCommand": "fast-check", "gates": true }
            ] }
        }))
        .unwrap(),
    )
    .unwrap();

    let disabled_queue = HeavyWorkQueue::new(PathBuf::new(), HeavyWorkConfig::disabled());
    let implement_calls_inline: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let inline_workflow = build_task_workflow(
        &inline_worktree,
        disabled_queue,
        implement_transport_recording(implement_calls_inline),
        always_pass_runner(),
    );
    let inline_ctx = inline_workflow
        .run(
            json!({ "spec_slug": TASK_SLUG }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("inline run should not error");
    let inline_output = inline_ctx.nodes.get("TestTaskNode").cloned().unwrap();
    let mut inline_keys: Vec<&String> = inline_output.as_object().unwrap().keys().collect();
    inline_keys.sort();

    // queue_park run — the SAME scripted pass_outcome() this suite's other
    // tests use.
    let queued_keys: Vec<String> = {
        let mut keys: Vec<String> = pass_outcome()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };

    let inline_keys: Vec<String> = inline_keys.into_iter().cloned().collect();
    assert_eq!(
        inline_keys, queued_keys,
        "an inline TestTaskNode output and a queue-park-injected one must carry the same key set"
    );
}

#[tokio::test]
async fn queue_park_wait_does_not_hit_implement_timeout() {
    let worktree = temp_dir("timeout");
    let spec_dir = worktree.join("planning").join(TASK_SLUG);
    std::fs::create_dir_all(&spec_dir).unwrap();
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&json!([{
            "task_id": 1,
            "title": "Implement thing 1",
            "description": "Do the work",
            "acceptance_criteria": ["it works"],
            "max_attempts": 2,
        }]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&json!({
            "sdlc_task": {
                "policy": {
                    "test_depth": "fast",
                    "test_dispatch": "queue_park",
                    "timeouts": { "implement": 1 }
                }
            },
            "validation": { "checks": [
                { "name": "tests", "kind": "command", "command": "full-suite-check",
                  "fastCommand": "fast-check", "gates": true }
            ] }
        }))
        .unwrap(),
    )
    .unwrap();

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 4);
    let implement_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let workflow = build_task_workflow(
        &worktree,
        queue,
        implement_transport_recording(implement_calls),
        always_pass_runner(),
    );

    // The parked job is held for 3s — well past `timeouts.implement = 1` —
    // proving the queue-park wait is never charged against ImplementTaskNode's
    // own call budget (a totally different node/stage).
    let heavy_work: Arc<dyn HeavyJobLookup> =
        ScriptedHeavyJobLookup::with_delay(vec![pass_outcome()], Duration::from_secs(3));
    let event = json!({ "spec_slug": TASK_SLUG });

    let final_ctx = drive_fresh(
        &workflow,
        event,
        Box::new(|_ctx: &TaskContext| {}),
        heavy_work,
    )
    .await
    .expect("a long queue-park wait must not surface as a timeout error");
    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "succeeded",
        "no failed node despite the 3s park, well past timeouts.implement=1s"
    );
}

#[tokio::test]
async fn queue_park_abort_while_queued_never_runs_checks() {
    let worktree = temp_dir("abort");
    write_task_fixture_files(&worktree, 1, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 1);
    // Occupy the one slot so the real submitted job stays Queued for the
    // life of this test.
    let (_release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let _holding = queue
        .submit(
            engine_core::coord::heavy_work::HeavyWorkSpec {
                class: "test".to_string(),
                repo: "fixture".to_string(),
                cwd: worktree.clone(),
                commands: vec![],
                run_id: None,
            },
            move || {
                // Never released within this test's lifetime — dropped with
                // the runtime at test end.
                release_rx.recv().ok();
            },
        )
        .await;

    let check_invocations = Arc::new(AtomicUsize::new(0));
    let counted = check_invocations.clone();
    let test_runner: CommandRunner = Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    });

    // Build a workflow whose TestTaskNode uses `test_runner` (to count real
    // check invocations) instead of the shared `always_pass_runner`.
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TaskFixtureSetupNode {
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
    registry.register(Box::new(ImplementTaskNode::new().with_transport(
        implement_transport_recording(Arc::new(Mutex::new(Vec::new()))),
    )));
    registry.register(Box::new(
        TestTaskNode::new()
            .with_runner(test_runner)
            .with_heavy_work(queue),
    ));
    registry.register(Box::new(
        TriageTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(TaskTriageRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new()
            .with_runner(always_pass_runner())
            .with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(IncrementAttemptNode));
    registry.register(Box::new(
        FinalValidationNode::new()
            .with_runner(always_pass_runner())
            .with_scope(ValidationScope::Reconcile),
    ));
    registry.register(Box::new(
        LeanBookkeepNode::new()
            .with_runner(always_pass_runner())
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
    let workflow = Workflow::new_validated(registry, sdlc_task_graph::schema())
        .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate");

    let cancel = CancellationToken::new();
    let heavy_work = ScriptedHeavyJobLookup::hanging();
    let event = json!({ "spec_slug": TASK_SLUG });

    // A sibling task fires the cancellation shortly after the walk starts —
    // `drive` itself runs directly on this test's own task (not spawned),
    // since its `on_progress` closure is not `Send`.
    let canceller = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        })
    };

    let drive_future = queue_park::drive(
        &workflow,
        RunStart::Fresh {
            event,
            on_progress: Box::new(|_ctx: &TaskContext| {}),
            options: engine_core::RunOptions {
                cancellation_token: Some(cancel.clone()),
                ..engine_core::RunOptions::default()
            },
            heavy_work: heavy_work.clone(),
        },
        &cancel,
    );
    let final_ctx = tokio::time::timeout(Duration::from_secs(5), drive_future)
        .await
        .expect("drive should return promptly once cancelled")
        .expect("a cancellation must not surface as a WorkflowError");
    canceller.await.ok();

    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "cancelled"
    );
    assert_eq!(
        check_invocations.load(Ordering::SeqCst),
        0,
        "the queued job's check runner must never have been invoked"
    );
}

// ── SDLC_FLOW fixture (mirrors `sdlc_flow_e2e.rs::build_workflow`) ─────────

struct FlowFixtureSetupNode {
    worktree_path: String,
}

#[async_trait]
impl Node for FlowFixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": format!("sdlc/{FLOW_SLUG}"),
            }),
        );
        let resolved_policy =
            flow_setup::resolve_policy_for_run(&ctx, Path::new(&self.worktree_path))?;
        engine_core::policy::stamp_resolved_policy(&mut ctx, &resolved_policy)?;
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

fn write_flow_fixture_files(worktree: &Path, task_count: u32, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join(FLOW_SLUG);
    std::fs::create_dir_all(&spec_dir).unwrap();
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
        "sdlc": { "policy": { "test_depth": "fast", "test_dispatch": "queue_park" } },
        "validation": { "checks": [
            { "name": "tests", "kind": "command", "command": "full-suite-check",
              "fastCommand": "fast-check", "gates": true }
        ] }
    });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string_pretty(&harness).unwrap(),
    )
    .unwrap();
}

/// Builds the full assembled `SDLC_FLOW` `Workflow` — mirrors
/// `sdlc_flow_e2e.rs::build_workflow_with_docs`, except `TestTaskNode` is
/// additionally wired to `heavy_work`. Every downstream drain node
/// (`ConsolidatedReviewNode` onward) is stubbed exactly as that file does,
/// even though this suite's own fixtures never dispatch past the task loop
/// (a 2-task fixture whose task 1 either passes-and-advances or fails-and-
/// retries) — `Workflow::new_validated` requires every declared node to
/// have a registered implementation regardless of what a given run reaches.
fn build_flow_workflow(
    worktree: &Path,
    heavy_work: HeavyWorkQueue,
) -> (Workflow, Arc<Mutex<Vec<String>>>) {
    let mut registry = NodeRegistry::new();
    let implement_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    registry.register(Box::new(FlowFixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode::new()));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode::new()));
    registry.register(Box::new(TaskQueueRouterNode));

    registry
        .register(Box::new(ImplementTaskNode::new().with_transport(
            implement_transport_recording(implement_calls.clone()),
        )));

    registry.register(Box::new(
        TestTaskNode::new()
            .with_runner(always_pass_runner())
            .with_heavy_work(heavy_work),
    ));
    registry.register(Box::new(
        TriageTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(TriageRouterNode));

    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(always_pass_runner())
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
        SaveStateNode::new().with_runner(always_pass_runner()),
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
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(EndReviewNode::new()));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(
        WrapUpNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(CloseBlockNode::new()));
    registry.register(Box::new(
        PullRequestNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(
        FlowEmitStateNode::new().with_runner(always_pass_runner()),
    ));

    let workflow = Workflow::new_validated(registry, sdlc_flow_graph::schema())
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate");
    (workflow, implement_calls)
}

#[tokio::test]
async fn queue_park_pass_resumes_to_next_task_sdlc_flow() {
    let worktree = temp_dir("pass-flow");
    write_flow_fixture_files(&worktree, 2, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 4);
    let (workflow, implement_calls) = build_flow_workflow(&worktree, queue);

    let (recorder, order) = OrderRecorder::new();
    let heavy_work: Arc<dyn HeavyJobLookup> = ScriptedHeavyJobLookup::new(vec![pass_outcome()]);
    let event = json!({ "spec_slug": FLOW_SLUG });

    let final_ctx = drive_fresh(&workflow, event, recorder.callback(), heavy_work)
        .await
        .expect("a two-task all-passing SDLC_FLOW queue-park run should not error");
    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "succeeded"
    );

    let order = order.lock().unwrap();
    assert!(
        order.contains(&"TriageTaskNode".to_string()),
        "TriageTaskNode must have dispatched: {order:?}"
    );
    drop(order);

    let calls = implement_calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "ImplementTaskNode must have dispatched once per task"
    );
    assert!(calls[1].contains("thing 2"), "prompt was: {}", calls[1]);
}

#[tokio::test]
async fn queue_park_fail_resumes_to_increment_attempt_sdlc_flow() {
    let worktree = temp_dir("fail-flow");
    write_flow_fixture_files(&worktree, 1, 2);

    let lock_dir = tempfile::tempdir().expect("tempdir");
    let queue = enabled_heavy_work_queue(lock_dir.path(), "test", 4);
    let (workflow, implement_calls) = build_flow_workflow(&worktree, queue);

    let (recorder, order) = OrderRecorder::new();
    let heavy_work: Arc<dyn HeavyJobLookup> =
        ScriptedHeavyJobLookup::new(vec![fail_outcome(), pass_outcome()]);
    let event = json!({ "spec_slug": FLOW_SLUG });

    let final_ctx = drive_fresh(&workflow, event, recorder.callback(), heavy_work)
        .await
        .expect("a fail-then-pass SDLC_FLOW queue-park run should not error");
    assert_eq!(
        engine_core::completion::derive_terminal_status(&final_ctx),
        "succeeded"
    );

    let order = order.lock().unwrap();
    let first_triage_pos = order
        .iter()
        .position(|n| n == "TriageTaskNode")
        .expect("TriageTaskNode dispatched");
    let increment_pos = order
        .iter()
        .position(|n| n == "IncrementAttemptNode")
        .expect("IncrementAttemptNode dispatched");
    assert!(increment_pos > first_triage_pos, "order was: {order:?}");

    let calls = implement_calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].contains("tests"), "prompt was: {}", calls[1]);
}

// ── ORCHESTRATION-child mechanism, through `execute_step` ──────────────────
//
// HONESTY REQUIREMENT — see the module doc's own note: this does not drive a
// real cloud-backed SDLC_TASK/SDLC_FLOW child (`registry_for_policy` wires
// the REAL Claude Code transport for `ImplementTaskNode`/`TriageTaskNode`
// with no injection seam at that layer). It proves the causal mechanism
// `default_flow_runner_with_heavy_work` was changed to use: a `FlowRunner`
// that drives a queue-parked child fully to completion via
// `queue_park::drive` before `execute_step` ever reads the returned ctx —
// against a minimal, fully injectable workflow.

struct RequestParkThenSucceedNode {
    job_id: Uuid,
}

#[async_trait]
impl Node for RequestParkThenSucceedNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.metadata["heavy_work"] = json!({
            "job_id": self.job_id.to_string(),
            "class": "test",
            "state": "queued",
        });
        suspend::request_suspension_with_reason(&mut ctx.metadata, SuspendReason::HeavyWorkQueue);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TestTaskNode"
    }
}

struct SucceedNode;

#[async_trait]
impl Node for SucceedNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "TriageTaskNode"
    }
}

fn minimal_queue_park_workflow(job_id: Uuid) -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(RequestParkThenSucceedNode { job_id }));
    registry.register(Box::new(SucceedNode));

    let mut nodes = HashMap::new();
    nodes.insert(
        "TestTaskNode".to_string(),
        NodeConfig::new("TestTaskNode", vec!["TriageTaskNode".to_string()]),
    );
    nodes.insert(
        "TriageTaskNode".to_string(),
        NodeConfig::new("TriageTaskNode", vec![]),
    );
    let schema = WorkflowSchema::new("MINIMAL_QUEUE_PARK", "TestTaskNode", nodes);
    Workflow::new(registry, schema)
}

fn two_repo_registry_for_orchestration() -> (tempfile::TempDir, RepoRegistry) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
    )
    .unwrap();
    let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
    (dir, registry)
}

#[tokio::test]
async fn queue_park_orchestration_child_resumes_through_default_flow_runner() {
    // Sanity: `default_flow_runner_with_heavy_work` builds and returns a
    // `FlowRunner` without panicking, over a real `RepoRegistry` — pins that
    // this task's wiring (`execute.rs`) compiles and constructs cleanly
    // against the exact seam `execute_step` calls.
    let (_dir, registry) = two_repo_registry_for_orchestration();
    let heavy_work_for_default: Arc<dyn HeavyJobLookup> = ScriptedHeavyJobLookup::hanging();
    let _sanity_runner =
        default_flow_runner_with_heavy_work(Arc::new(registry), heavy_work_for_default);

    // The mechanism itself: a `FlowRunner` that drives a queue-parked child
    // through `queue_park::drive` — precisely what `default_flow_runner_
    // with_heavy_work`'s Flow/Task arms now do (`execute.rs`) — must never
    // hand `execute_step` a still-suspended ctx.
    let job_id = Uuid::new_v4();
    let workflow = Arc::new(minimal_queue_park_workflow(job_id));
    let heavy_work: Arc<dyn HeavyJobLookup> = ScriptedHeavyJobLookup::new(vec![json!({})]);

    let drive_runner: engine_core::workflows::orchestration::execute::FlowRunner =
        Arc::new(move |_invocation: FlowInvocation| {
            let workflow = workflow.clone();
            let heavy_work = heavy_work.clone();
            Box::pin(async move {
                let cancel = CancellationToken::new();
                queue_park::drive(
                    &workflow,
                    RunStart::Fresh {
                        event: json!({}),
                        on_progress: Box::new(|_ctx: &TaskContext| {}),
                        options: engine_core::RunOptions::default(),
                        heavy_work,
                    },
                    &cancel,
                )
                .await
            })
        });

    let (_dir2, registry2) = two_repo_registry_for_orchestration();
    let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
    let step = engine_core::workflows::orchestration::chain::ChainStep {
        repo: "repo-a".to_string(),
        block_id: "A.1".to_string(),
        directives: None,
        ..Default::default()
    };

    let outcome = execute_step(
        &step,
        &resolve_engine,
        &registry2,
        &drive_runner,
        false,
        true,
        Uuid::new_v4(),
        None,
        None,
        engine_core::policy::permission::PermissionProfile::Standard,
        None,
        None,
        None,
    )
    .await
    .expect("a parked-then-passed child must integrate as a success");

    assert!(
        !suspend::is_suspended(&outcome.ctx.metadata),
        "execute_step must never see a still-suspended ctx once the FlowRunner drives \
         queue-park suspensions to completion itself"
    );
}
