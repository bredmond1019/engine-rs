//! Integration suite for `EN.3.G` ("Terminal-path robustness"): proves that
//! a garbage verdict from either model-judgment stage (triage / review)
//! still reaches `WrapUpNode` and leaves a terminal state file on disk,
//! rather than silently halting the walk mid-graph the way `TriageRouterNode`
//! / `ReviewRouterNode`'s pre-EN.3.G `_ => None` catch-all arms did.
//!
//! Reuses the same seam-injection scaffolding style as `sdlc_flow_e2e.rs`
//! (fixture `SetupWorktreeNode` replacement over a real temp dir, stubbed
//! `ModelTransport` per model node, stubbed `CommandRunner` for
//! `git`/`gh`/`mev`) — kept as a self-contained module rather than importing
//! `sdlc_flow_e2e`'s private helpers, since those are not `pub(crate)` and
//! this suite needs slightly different stubs (garbage-verdict transports,
//! an `llm_triage: true` event flag). No real `claude`, `git`, `gh`, or
//! `mev` subprocess is ever spawned.
//!
//! STANDING RULE 8: this file is a `mod` of `tests/it/main.rs`, never a new
//! `crates/engine-core/tests/*.rs` binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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

/// Replaces the real `SetupWorktreeNode` — identical in spirit to
/// `sdlc_flow_e2e.rs`'s fixture of the same name.
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
                "branch_name": "sdlc/fixture-terminal-paths-spec",
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
        "engine-core-sdlc-flow-terminal-paths-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the subdirs.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join("fixture-terminal-paths-spec")).unwrap();
    dir
}

/// Same fixture-file writer as `sdlc_flow_e2e.rs`'s `write_fixture_files`
/// (single pending task, `test_depth: "fast"`), duplicated here to keep this
/// module self-contained.
fn write_fixture_files(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree
        .join("planning")
        .join("fixture-terminal-paths-spec");
    let tasks = json!([{
        "task_id": 1,
        "title": "Implement thing 1",
        "description": "Do the work",
        "acceptance_criteria": ["it works"],
        "max_attempts": max_attempts,
    }]);
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

/// `TestTaskNode`/`FinalValidationNode` runner that always PASSes, same
/// `git status` special-case as `sdlc_flow_e2e.rs`'s `always_pass_runner`.
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

/// `TestTaskNode` runner that always FAILs — drives the triage-transport
/// path (Test 1) where the LLM classifies a real test failure.
fn always_fail_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "always fails".to_string(),
        })
    })
}

/// `git`-shaped runner that always succeeds with empty output — used for
/// `ConsolidatedReviewNode`'s `git diff` call and `SaveStateNode`'s commit.
fn noop_git_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Stub `gh pr create` / `git push` runner: returns a canned PR URL for
/// `gh` calls, identical in spirit to `sdlc_flow_e2e.rs`'s `gh_stub_runner`.
fn gh_stub_runner() -> CommandRunner {
    Arc::new(|program, args, _cwd| {
        if program == "gh" {
            Ok(CommandOutput {
                status: 0,
                stdout: "https://github.com/example/repo/pull/42\n".to_string(),
                stderr: String::new(),
            })
        } else {
            let _ = args;
            Ok(CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    })
}

/// A model transport that always returns the given raw `verdict` string
/// (used to synthesize a garbage/unrecognized reply from either
/// `TriageTaskNode` or `ConsolidatedReviewNode`).
fn garbage_verdict_transport(
    verdict: &'static str,
) -> engine_core::workflows::sdlc_flow::setup::ModelTransport {
    Arc::new(move |_config, _prompt| {
        let outcome = stub_outcome(
            &json!({ "verdict": verdict, "reason": "garbage", "summary": "garbage", "issues": [] })
                .to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    })
}

fn passing_review_transport() -> engine_core::workflows::sdlc_flow::setup::ModelTransport {
    Arc::new(|_config, _prompt| {
        let outcome = stub_outcome(
            &json!({ "verdict": "PASS", "summary": "looks good", "issues": [] }).to_string(),
        );
        Box::pin(async move { Ok(outcome) })
    })
}

/// Builds the full assembled `SDLC_FLOW` `Workflow` — the real declared
/// graph from [`graph::schema`] paired with a registry where every model/
/// subprocess node is stubbed. `triage_transport`/`review_transport` let
/// each test inject a garbage-verdict reply from either stage;
/// `pr_runner`/`emit_state_runner` mirror `sdlc_flow_e2e.rs`'s knobs.
#[allow(clippy::too_many_arguments)]
fn build_workflow(
    worktree: &Path,
    test_runner: CommandRunner,
    triage_transport: engine_core::workflows::sdlc_flow::setup::ModelTransport,
    review_transport: engine_core::workflows::sdlc_flow::setup::ModelTransport,
    pr_runner: CommandRunner,
    emit_state_runner: CommandRunner,
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
    registry.register(Box::new(
        TriageTaskNode::new().with_transport(triage_transport),
    ));
    registry.register(Box::new(TriageRouterNode));

    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(noop_git_runner())
            .with_transport(review_transport),
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
    registry.register(Box::new(EndReviewNode::new()));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(CloseBlockNode::new()));
    registry.register(Box::new(PullRequestNode::new().with_runner(pr_runner)));
    registry.register(Box::new(
        EmitStateNode::new().with_runner(emit_state_runner),
    ));

    let schema = graph::schema();

    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

fn read_state(worktree: &Path, spec_slug: &str) -> serde_json::Value {
    let state_path = worktree
        .join("planning")
        .join(spec_slug)
        .join("sdlc")
        .join("sdlc-flow-state.json");
    serde_json::from_str(
        &std::fs::read_to_string(&state_path)
            .unwrap_or_else(|err| panic!("state file should exist at {state_path:?}: {err}")),
    )
    .expect("committed state file should parse as JSON")
}

/// Test 1 (the block's headline): a garbage verdict from `TriageTaskNode`
/// still reaches `WrapUpNode`, and the on-disk terminal state is `blocked`
/// with a `bail_reason` naming the offending string. Fails before task 1
/// (the walk halted at `TriageRouterNode` with no wrap-up).
#[tokio::test]
async fn garbage_triage_verdict_still_reaches_terminal_state() {
    let worktree = temp_worktree("triage-garbage");
    write_fixture_files(&worktree, 2);

    let workflow = build_workflow(
        &worktree,
        always_fail_runner(),
        garbage_verdict_transport("WAT"),
        passing_review_transport(),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({
        "spec_slug": "fixture-terminal-paths-spec",
        "auto_pr": false,
        "llm_triage": true,
    });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode must have produced a result on the garbage-triage-verdict path");
    assert!(wrap_up_result.get("log_entry").is_some());

    let triage_result = final_ctx
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert_eq!(triage_result["unrecognized_verdict"], json!("WAT"));

    let state_json = read_state(&worktree, "fixture-terminal-paths-spec");
    assert_eq!(state_json["status"], json!("blocked"));
    assert_ne!(state_json["status"], json!("running"));
    let bail_reason = state_json["bail_reason"]
        .as_str()
        .expect("bail_reason should be populated");
    assert!(
        bail_reason.contains("WAT"),
        "bail_reason should name the offending verdict: {bail_reason}"
    );
}

/// Test 2: same guarantee for a garbage `ConsolidatedReviewNode` verdict —
/// triage PASSes, review returns garbage, the run still reaches `WrapUpNode`
/// with a terminal state on disk.
#[tokio::test]
async fn garbage_review_verdict_still_reaches_terminal_state() {
    let worktree = temp_worktree("review-garbage");
    write_fixture_files(&worktree, 2);

    let workflow = build_workflow(
        &worktree,
        always_pass_runner(),
        garbage_verdict_transport("PASS"),
        garbage_verdict_transport("HUH"),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({
        "spec_slug": "fixture-terminal-paths-spec",
        "auto_pr": false,
        "llm_triage": false,
    });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode must have produced a result on the garbage-review-verdict path");
    assert!(wrap_up_result.get("log_entry").is_some());

    let review_result = final_ctx
        .nodes
        .get("ConsolidatedReviewNode")
        .expect("ConsolidatedReviewNode should have stamped a result");
    assert_eq!(review_result["unrecognized_verdict"], json!("HUH"));

    let state_json = read_state(&worktree, "fixture-terminal-paths-spec");
    assert_eq!(state_json["status"], json!("blocked"));
    assert_ne!(state_json["status"], json!("running"));
    let bail_reason = state_json["bail_reason"]
        .as_str()
        .expect("bail_reason should be populated");
    assert!(
        bail_reason.contains("HUH"),
        "bail_reason should name the offending verdict: {bail_reason}"
    );
}

/// Test 3: `auto_pr: true` with a stub `gh` populates the emitted state
/// file's `pr` block (task 6); a companion `auto_pr: false` walk leaves
/// `pr` null.
#[tokio::test]
async fn emitted_state_carries_pr_block_only_when_auto_pr_true() {
    let worktree_with_pr = temp_worktree("with-pr");
    write_fixture_files(&worktree_with_pr, 2);
    let workflow_with_pr = build_workflow(
        &worktree_with_pr,
        always_pass_runner(),
        garbage_verdict_transport("PASS"),
        passing_review_transport(),
        gh_stub_runner(),
        noop_git_runner(),
    );
    let event_with_pr = json!({
        "spec_slug": "fixture-terminal-paths-spec",
        "auto_pr": true,
        "llm_triage": false,
    });
    workflow_with_pr
        .run(event_with_pr, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let state_with_pr = read_state(&worktree_with_pr, "fixture-terminal-paths-spec");
    assert_eq!(
        state_with_pr["pr"]["url"],
        json!("https://github.com/example/repo/pull/42")
    );
    assert_eq!(state_with_pr["pr"]["number"], json!(42));

    let worktree_no_pr = temp_worktree("without-pr");
    write_fixture_files(&worktree_no_pr, 2);
    let workflow_no_pr = build_workflow(
        &worktree_no_pr,
        always_pass_runner(),
        garbage_verdict_transport("PASS"),
        passing_review_transport(),
        noop_git_runner(),
        noop_git_runner(),
    );
    let event_no_pr = json!({
        "spec_slug": "fixture-terminal-paths-spec",
        "auto_pr": false,
        "llm_triage": false,
    });
    workflow_no_pr
        .run(event_no_pr, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let state_no_pr = read_state(&worktree_no_pr, "fixture-terminal-paths-spec");
    assert!(state_no_pr["pr"].is_null());
}

/// Test 4 (regression guard): a happy-path walk (all verdicts recognized,
/// the single task passes) still reaches `WrapUpNode` with `status: "done"`
/// and a null `bail_reason` — proving tasks 1 and 6 changed nothing on the
/// known-good path.
#[tokio::test]
async fn happy_path_still_reaches_done_with_no_bail_reason() {
    let worktree = temp_worktree("happy");
    write_fixture_files(&worktree, 2);

    let workflow = build_workflow(
        &worktree,
        always_pass_runner(),
        garbage_verdict_transport("PASS"),
        passing_review_transport(),
        noop_git_runner(),
        noop_git_runner(),
    );

    let event = json!({
        "spec_slug": "fixture-terminal-paths-spec",
        "auto_pr": false,
        "llm_triage": false,
    });
    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    let wrap_up_result = final_ctx
        .nodes
        .get("WrapUpNode")
        .expect("WrapUpNode must have produced a result on the happy path");
    assert!(wrap_up_result.get("log_entry").is_some());

    let triage_result = final_ctx
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    assert!(triage_result.get("unrecognized_verdict").is_none());

    let state_json = read_state(&worktree, "fixture-terminal-paths-spec");
    assert_eq!(state_json["status"], json!("done"));
    assert!(state_json["bail_reason"].is_null());
}
