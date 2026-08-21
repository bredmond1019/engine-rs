//! The non-termination regression test for
//! `EN.ticket.review-retry-loop-unbounded`.
//!
//! **The exact cycle this test covers, and why it was unbounded before this
//! ticket:** `Implement -> Test(pass) -> Triage(PASS) -> Review(FAIL, minor
//! issues) -> IncrementAttempt -> Implement`. `TriageTaskNode` has its own
//! `attempt_count >= max_attempts` check (`task_loop.rs:1761`), but that
//! check sits AFTER an early return: when the test run passes, `TriageTaskNode`
//! returns `PASS` immediately and never reaches the bound at all
//! (`task_loop.rs:1746`). `ReviewRouterNode`'s minor-issue `FAIL`/`PARTIAL`
//! arm then always routed back to `IncrementAttemptNode` with no bound of
//! its own — so a test stub that always passes, paired with a review stub
//! that always returns a non-structural `FAIL`, drove this exact cycle
//! forever. `Workflow::walk` has (and still has, after this ticket) no
//! step cap of its own; the run-level cap is out of scope for this fix
//! (see the ticket's `out_of_scope`) and is tracked as its own future
//! ticket. This test is therefore the ONLY thing standing between the
//! engine and an infinite implement/review spend loop on this specific
//! path — a regression here must fail loudly, not hang CI, hence the
//! wall-clock guard below.
//!
//! Driven through the **real** `graph::schema()` walk (not a hand-built
//! `TaskContext`), so it exercises the actual back-edges
//! `ReviewRouterNode -> IncrementAttemptNode -> ImplementTaskNode` and the
//! bound's exit `ReviewRouterNode -> WrapUpNode`. Hermetic: every model
//! call goes through a stub `with_transport` and every subprocess through a
//! stub `with_runner` — no real `claude`, no real `git`, no network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::Outcome;
use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::end_review::{EndReviewNode, EndReviewRouterNode};
use engine_core::workflows::sdlc_flow::final_validation::FinalValidationNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
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

/// Same fixture-setup pattern as `sdlc_flow_task_loop.rs`/
/// `sdlc_flow_retry_feedback.rs`: replaces the real `SetupWorktreeNode`
/// (which hard-codes `trees/{branch}` relative to the process cwd) with a
/// controlled temp directory, and stamps the resolved policy the same way
/// the real node does.
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
        model_usage: Default::default(),
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
        "engine-core-sdlc-flow-review-retry-loop-it-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join("fixture-spec")).unwrap();
    dir
}

/// One PENDING task. `max_attempts` on the TASK's own counter is set high
/// so it never coincides with (or is confused for) the review bound this
/// test is proving — the test's tests always pass, so `TriageTaskNode`
/// never even reaches its own bound check.
fn write_fixture_files(worktree: &Path) {
    let spec_dir = worktree.join("planning").join("fixture-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Implement the thing",
            "description": "Do the work",
            "acceptance_criteria": ["it works"],
            "max_attempts": 10,
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

/// Always reports a passing check — this is the "tests pass" half of the
/// unbounded cycle. `git status --porcelain` (the write-verification
/// guard's probe) is special-cased to report a modified file so
/// `TestTaskNode` doesn't bail for "no changes made".
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

fn noop_git_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// The real `SDLC_FLOW` graph with every model/subprocess seam stubbed.
/// `ConsolidatedReviewNode`'s transport counts its own invocations and
/// always returns `FAIL` with 2 issues — inside `STRUCTURAL_ISSUE_THRESHOLD`
/// (5), so `ReviewRouterNode` takes the minor-issue back-edge every time
/// instead of bailing straight to `WrapUpNode` on a structural verdict.
fn build_workflow(worktree: &Path, review_calls: Arc<AtomicUsize>) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        |_config, _prompt: String| {
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
        TestTaskNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(TriageTaskNode::new()));
    registry.register(Box::new(TriageRouterNode));

    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(noop_git_runner())
            .with_transport(Arc::new(move |_config, _prompt| {
                review_calls.fetch_add(1, Ordering::SeqCst);
                let outcome = stub_outcome(
                    &json!({
                        "verdict": "FAIL",
                        "summary": "two minor issues found",
                        "issues": ["nit 1", "nit 2"],
                    })
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
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(always_pass_runner()),
    ));
    registry.register(Box::new(EndReviewNode::new()));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(CloseBlockNode::new()));
    registry.register(Box::new(PullRequestNode::new()));
    registry.register(Box::new(
        EmitStateNode::new().with_runner(noop_git_runner()),
    ));

    Workflow::new_validated(registry, graph::schema())
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

#[tokio::test]
async fn review_retry_loop_terminates_after_exactly_max_review_attempts() {
    let worktree = temp_worktree();
    write_fixture_files(&worktree);

    let review_calls = Arc::new(AtomicUsize::new(0));
    let workflow = build_workflow(&worktree, review_calls.clone());

    let event = json!({ "spec_slug": "fixture-spec", "auto_pr": false });

    // Wall-clock guard: before this ticket's fix, this exact fixture
    // (always-passing tests + always-minor-FAIL review) walked the graph
    // forever. A regression must fail the suite loudly, not hang CI in a
    // repo whose full suite otherwise runs in ~2s.
    let run = tokio::time::timeout(
        Duration::from_secs(30),
        workflow.run(event, Box::new(|_ctx: &TaskContext| {})),
    )
    .await;

    let final_ctx = run
        .expect(
            "workflow.run did not terminate within the wall-clock guard — the \
             review-retry loop is unbounded again",
        )
        .expect("workflow run should not error");

    // --- The invocation count, not merely termination -----------------
    // A fix that terminates for some other reason (e.g. an unrelated
    // error, or a bail on the first pass) would satisfy a bare
    // "it finished" assertion. Assert the exact count instead: the
    // default `max_review_attempts` is 3, so `ConsolidatedReviewNode`
    // must run exactly 3 times before the router routes to `WrapUpNode`.
    let default_max_review_attempts = SdlcPolicy::default().max_review_attempts;
    assert_eq!(
        default_max_review_attempts, 3,
        "this test assumes the documented default of 3 (matching JS's \
         MAX_REVIEW_ATTEMPTS) — if the default changed, update this test's \
         expectation deliberately rather than silently drifting"
    );
    assert_eq!(
        review_calls.load(Ordering::SeqCst) as u32,
        default_max_review_attempts,
        "expected exactly max_review_attempts ConsolidatedReviewNode \
         invocations before the router bails to WrapUpNode"
    );

    // --- The run actually reached WrapUpNode, not merely stopped ------
    let wrap_up = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode must have run once the review bound is hit");
    let state = &wrap_up["state"];
    assert_eq!(
        state["telemetry"]["review_attempts"],
        json!(default_max_review_attempts),
        "the durable review_attempts counter must have reached the bound"
    );
    assert_eq!(
        state["global_status"],
        json!("blocked"),
        "an exhausted review bound must yield a blocked status, not a \
         silent clean done"
    );
    let bail_reason = state["bail_reason"]
        .as_str()
        .expect("bail_reason must be present on an exhausted-review run");
    assert!(
        bail_reason.contains("Review attempts exhausted")
            && bail_reason.contains("two minor issues found"),
        "the bail reason must name review exhaustion and the last verdict's \
         summary, got: {bail_reason:?}"
    );

    let _ = std::fs::remove_dir_all(&worktree);
}
