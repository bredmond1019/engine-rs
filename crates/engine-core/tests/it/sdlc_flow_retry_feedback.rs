//! Integration test for the retry-feedback fix: **attempt 2's
//! `ImplementTaskNode` prompt must contain attempt 1's captured check
//! output.**
//!
//! This is the headline behavior of `planning/ticket-retry-failure-feedback`.
//! Before the fix, `ImplementTaskNode` built its prompt from title +
//! description + acceptance_criteria alone, so the request sent on the retry
//! back-edge (`TriageRouterNode -> IncrementAttemptNode ->
//! ImplementTaskNode`) was **byte-identical** to the first attempt's. A live
//! run (bc1a44be, 2026-08-01) burned all three attempts on two missing
//! `async` keywords for exactly that reason: the model, re-entering a
//! worktree already holding its own prior edits and told nothing about the
//! failure, reasonably concluded the task was done.
//!
//! The assertion below fails against pre-ticket code by construction — with
//! an unchanged prompt, prompts[0] == prompts[1], so `prompts[1]` cannot
//! contain the sentinel while `prompts[0]` does not.
//!
//! Driven through the **real** `graph::schema()` walk, not a hand-built
//! `TaskContext`, so it exercises the actual back-edge. Hermetic: every
//! model call goes through a stub `with_transport` and every subprocess
//! through a stub `with_runner` — no real `claude`, no real `git`, no
//! network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_code_rs::Outcome;
use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::close_block::CloseBlockNode;
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

/// A string that can only reach attempt 2's prompt by travelling
/// check stdout -> `TestTaskNode`'s `check_results[].output` ->
/// `prior_attempt_feedback` -> `ImplementTaskNode`'s prompt. Nothing else in
/// the fixture mentions it.
const FAILURE_SENTINEL: &str = "error[E0308]: expected `impl Future`, found `HttpResponse` \
                                --> src/http.rs:682:5 (RETRY_FEEDBACK_SENTINEL)";

/// Same fixture setup node as `sdlc_flow_task_loop.rs`: writes a controlled
/// temp-dir worktree path and stamps the resolved policy, so no real
/// `git worktree add` runs.
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
        "engine-core-sdlc-flow-retry-feedback-it-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join("fixture-spec")).unwrap();
    dir
}

/// One PENDING task (`max_attempts = 2`) plus a single gating harness check.
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

/// `TestTaskNode` runner that FAILs its first harness-check invocation with
/// [`FAILURE_SENTINEL`] on stdout, then passes silently. `git status
/// --porcelain` (the write-verification guard's probe) is special-cased and
/// does not consume a slot in the fail/pass sequence.
fn fail_with_sentinel_then_pass_runner() -> CommandRunner {
    let calls = Arc::new(AtomicUsize::new(0));
    Arc::new(move |program, args, _cwd| {
        if program == "git" && args.first() == Some(&"status") {
            return Ok(CommandOutput {
                status: 0,
                stdout: " M src/lib.rs\n".to_string(),
                stderr: String::new(),
            });
        }
        let n = calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(CommandOutput {
                status: 1,
                stdout: FAILURE_SENTINEL.to_string(),
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

fn noop_git_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// The real `SDLC_FLOW` graph with every model/subprocess seam stubbed. The
/// `ImplementTaskNode` transport records each prompt it is handed, in order.
fn build_workflow(worktree: &Path, prompts: Arc<Mutex<Vec<String>>>) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    registry.register(Box::new(ImplementTaskNode::new().with_transport(Arc::new(
        move |_config, prompt: String| {
            prompts.lock().unwrap().push(prompt);
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

    let test_runner = fail_with_sentinel_then_pass_runner();
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
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(test_runner),
    ));
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
async fn retry_prompt_contains_the_previous_attempts_failure_output() {
    let worktree = temp_worktree();
    write_fixture_files(&worktree);

    let prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let workflow = build_workflow(&worktree, prompts.clone());

    workflow
        .run(
            json!({ "spec_slug": "fixture-spec", "auto_pr": false }),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("workflow run should not error");

    let prompts = prompts.lock().unwrap().clone();
    assert_eq!(
        prompts.len(),
        2,
        "expected exactly two ImplementTaskNode calls (first attempt + one \
         retry via IncrementAttemptNode), got {}",
        prompts.len()
    );

    // Attempt 1 knows nothing — it is the first attempt.
    assert!(
        !prompts[0].contains("RETRY_FEEDBACK_SENTINEL"),
        "the FIRST prompt must not carry any failure output:\n{}",
        prompts[0]
    );
    assert!(
        !prompts[0].contains("PREVIOUS ATTEMPT FAILED"),
        "the FIRST prompt must be the plain first-attempt prompt:\n{}",
        prompts[0]
    );

    // Attempt 2 — the whole point of the ticket.
    assert!(
        prompts[1].contains("RETRY_FEEDBACK_SENTINEL"),
        "the RETRY prompt must carry attempt 1's captured check output:\n{}",
        prompts[1]
    );
    assert!(
        prompts[1].contains("src/http.rs:682"),
        "the RETRY prompt should carry the failing location:\n{}",
        prompts[1]
    );
    assert!(
        prompts[1].contains("FAILED CHECK: tests"),
        "the RETRY prompt should name the failing check:\n{}",
        prompts[1]
    );

    // ...and it is strictly a superset of the first attempt's request, not a
    // replacement: the task's own brief still leads.
    assert!(prompts[1].starts_with("Implement the following SDLC task."));
    assert!(prompts[1].contains("Title: Implement the thing"));
    assert_ne!(
        prompts[0], prompts[1],
        "the retry prompt must differ from the first attempt's — a \
         byte-identical retry is the defect this ticket fixes"
    );
}
