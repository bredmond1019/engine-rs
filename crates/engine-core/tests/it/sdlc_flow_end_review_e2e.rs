//! Graph-level proof for `EN.ticket.review-mode-endonly-reviews-nothing`
//! (task 3): before this ticket, `ReviewMode::EndOnly` performed ZERO
//! acceptance-criteria review while `profiles.rs`'s `batch_reviewer` comment
//! described it as "the quality ceiling" — `TriageRouterNode::route` mapped
//! every `PASS` verdict under `EndOnly` straight to `UpdateTaskStatusNode`,
//! and the drain branch had no review node at all. `end_review.rs` (task 1)
//! and `wrap_up.rs`/`graph.rs` (task 2) already prove `EndReviewNode` and
//! its router in isolation; THIS file drives the real, fully assembled
//! `SDLC_FLOW` graph (`graph::schema()` + `graph::registry_for_policy`) end
//! to end, because a node that runs correctly in isolation but is wired
//! wrong (or never reached) would still leave the run reviewing nothing —
//! exactly the failure mode being fixed. No real `claude`, `git`, `gh`, or
//! `mev` subprocess is ever spawned.
//!
//! The central test (`end_only_full_run_makes_exactly_one_review_call_...`)
//! asserts a call that did not happen before this ticket: exactly one
//! `EndReviewNode` model invocation, on the drain branch, whose prompt
//! carries the COMPLETE Acceptance Criteria (both tasks') and a diff
//! spanning more than one task's changes — asserting prompt CONTENTS, not
//! just an invocation count, since a node that runs and reviews the wrong
//! thing would satisfy a count alone. Its companion tests protect the
//! default path: under `PerTask`/`TrivialSkip`, `EndReviewNode` must make
//! ZERO calls and the per-task review count must be exactly what it was
//! before this ticket.
//!
//! STANDING RULE 8: this file is a `mod` of `tests/it/main.rs`, never a new
//! `crates/engine-core/tests/*.rs` binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use claude_code_rs::Outcome;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::end_review::{EndReviewNode, EndReviewRouterNode};
use engine_core::workflows::sdlc_flow::final_validation::FinalValidationNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::policy::{ReviewMode, SdlcPolicy};
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, RESOLVED_POLICY_IDENTITY,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, SaveStateNode, TestTaskNode, TriageTaskNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use engine_core::workflows::sdlc_flow::ModelTransport;
use serde_json::json;

const SPEC_SLUG: &str = "fixture-end-review-e2e-spec";

/// Diff markers this suite's `git diff` stub returns, spanning two tasks'
/// worth of change — the "multi-task diff" the headline test asserts is
/// present in `EndReviewNode`'s prompt.
const MULTI_TASK_DIFF: &str = "diff --git a/one.rs b/one.rs\n+task one change\n\
     diff --git a/two.rs b/two.rs\n+task two change\n";

/// Replaces the real `SetupWorktreeNode`: writes a controlled temp-dir
/// `worktree_path` and stamps an already-resolved [`SdlcPolicy`] directly
/// under `RESOLVED_POLICY_IDENTITY`, the same shape
/// `sdlc_flow_profiles.rs`'s fixture uses — so this suite can drive each
/// `review_mode` precisely without a `harness.json` `sdlc.policy` round
/// trip.
struct FixtureSetupNode {
    worktree_path: String,
    resolved_policy: serde_json::Value,
}

#[async_trait::async_trait]
impl Node for FixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/fixture-end-review-e2e-spec",
            }),
        );
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            self.resolved_policy.clone(),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-end-review-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: PID recycling makes this removal necessary, not
    // optional (see `setup.rs`'s `temp_dir_named` doc comment).
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join(SPEC_SLUG)).unwrap();
    dir
}

/// Two PENDING tasks, each with its own distinct acceptance criterion, so
/// `EndReviewNode`'s rendered "## Acceptance Criteria" block has two
/// distinguishable pieces to prove are BOTH present — the "complete"
/// Acceptance Criteria the ticket requires, not just the last task's.
fn write_fixture_files(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join(SPEC_SLUG);
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Task One",
            "description": "d1",
            "acceptance_criteria": ["criterion one"],
            "max_attempts": max_attempts,
        },
        {
            "task_id": 2,
            "title": "Task Two",
            "description": "d2",
            "acceptance_criteria": ["criterion two"],
            "max_attempts": max_attempts,
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

fn stub_outcome(text: &str) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: std::collections::BTreeMap::new(),
        text: text.to_string(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: None,
    }
}

/// One `CommandRunner` for the whole workflow. `git status` reports
/// `src/lib.rs` modified (matching `ImplementTaskNode`'s stubbed claim, so
/// `TestTaskNode`'s write-verification guard does not trip); `git diff
/// --numstat HEAD` (`TriageTaskNode`'s trivial-classification probe)
/// returns `numstat_content`; every other `git diff ...` (both
/// `ConsolidatedReviewNode`'s `diff HEAD` and `EndReviewNode`'s `diff
/// <base>`) returns `diff_content`; everything else (add/commit/gh/mev/the
/// harness's gating check command) succeeds with empty output.
fn make_runner(diff_content: &'static str, numstat_content: &'static str) -> CommandRunner {
    Arc::new(move |program, args, _cwd| {
        if program == "git" {
            if args.first() == Some(&"status") {
                return Ok(CommandOutput {
                    status: 0,
                    stdout: " M src/lib.rs\n".to_string(),
                    stderr: String::new(),
                });
            }
            if args.first() == Some(&"diff") {
                if args.get(1) == Some(&"--numstat") {
                    return Ok(CommandOutput {
                        status: 0,
                        stdout: numstat_content.to_string(),
                        stderr: String::new(),
                    });
                }
                return Ok(CommandOutput {
                    status: 0,
                    stdout: diff_content.to_string(),
                    stderr: String::new(),
                });
            }
        }
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// A `git diff --numstat HEAD` stub small enough that
/// `task_loop::classify_trivial` reports `trivial: true` — needed only by
/// the `TrivialSkip` test.
const TRIVIAL_NUMSTAT: &str = "1\t1\tsrc/a.rs\n";
/// Deliberately large/non-trivial numstat, used everywhere `trivial`-ness
/// must not matter (`PerTask` always reviews regardless).
const NON_TRIVIAL_NUMSTAT: &str = "400\t400\tsrc/a.rs\n";

fn panicking_transport() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async { panic!("transport must not be called on this path") })
    })
}

fn implement_transport() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({
                    "summary": "implemented",
                    "modified_files": ["src/lib.rs"],
                    "tests_added": ["it_works"],
                })
                .to_string(),
            ))
        })
    })
}

fn patch_docs_transport() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "summary": "no stale docs found", "files_patched": [] }).to_string(),
            ))
        })
    })
}

/// A verdict-returning transport that counts its own invocations — the spy
/// counter both `ConsolidatedReviewNode` and `EndReviewNode` are wired
/// through in these tests.
fn counting_verdict_transport(calls: Arc<AtomicUsize>, verdict: &'static str) -> ModelTransport {
    Arc::new(move |_config, _prompt| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "verdict": verdict, "summary": "reviewed", "issues": [] }).to_string(),
            ))
        })
    })
}

/// Like [`counting_verdict_transport`] but also captures the full prompt
/// text of every call it receives, for the headline test's prompt-content
/// assertions.
fn capturing_verdict_transport(
    calls: Arc<AtomicUsize>,
    captured_prompts: Arc<Mutex<Vec<String>>>,
    verdict: &'static str,
    summary: &'static str,
    issues: Vec<&'static str>,
) -> ModelTransport {
    Arc::new(move |_config, prompt| {
        calls.fetch_add(1, Ordering::SeqCst);
        captured_prompts.lock().unwrap().push(prompt);
        let issues: Vec<String> = issues.iter().map(|s| s.to_string()).collect();
        Box::pin(async move {
            Ok(stub_outcome(
                &json!({ "verdict": verdict, "summary": summary, "issues": issues }).to_string(),
            ))
        })
    })
}

/// Builds the full assembled `SDLC_FLOW` `Workflow` on top of
/// `graph::registry_for_policy(policy)` — the real, production per-stage
/// transport wiring the ticket's node is meant to be reached through — with
/// every model/subprocess seam this suite needs to observe or control
/// stubbed on top. Mirrors `sdlc_flow_profiles.rs`'s
/// `build_task_loop_workflow`, extended with `FinalValidationNode` /
/// `PatchDocsNode` / `WrapUpNode` so a run can actually reach the drain
/// branch `EndReviewNode` sits on, not just the task loop.
#[allow(clippy::too_many_arguments)]
fn build_workflow(
    worktree: &Path,
    policy: &SdlcPolicy,
    runner: CommandRunner,
    consolidated_review_transport: ModelTransport,
    end_review_transport: ModelTransport,
) -> Workflow {
    let mut registry: NodeRegistry = graph::registry_for_policy(policy);

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
        resolved_policy: serde_json::to_value(policy).expect("SdlcPolicy should serialize"),
    }));

    registry.register(Box::new(
        ImplementTaskNode::new().with_transport(implement_transport()),
    ));
    registry.register(Box::new(TestTaskNode::new().with_runner(runner.clone())));
    // `llm_triage` is always `false` in this suite's event, so
    // `TriageTaskNode` never issues a model call — its transport must never
    // be invoked, only its runner (for the trivial-classification probe).
    registry.register(Box::new(
        TriageTaskNode::new()
            .with_transport(panicking_transport())
            .with_runner(runner.clone()),
    ));
    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(runner.clone())
            .with_transport(consolidated_review_transport),
    ));
    registry.register(Box::new(SaveStateNode::new().with_runner(runner.clone())));
    registry.register(Box::new(
        FinalValidationNode::new().with_runner(runner.clone()),
    ));
    registry.register(Box::new(
        EndReviewNode::new()
            .with_runner(runner.clone())
            .with_transport(end_review_transport),
    ));
    registry.register(Box::new(EndReviewRouterNode));
    registry.register(Box::new(
        PatchDocsNode::new().with_transport(patch_docs_transport()),
    ));
    registry.register(Box::new(WrapUpNode::new().with_runner(runner.clone())));
    // `CloseBlockNode`/`PullRequestNode`/`EmitStateNode` default construction
    // matches `sdlc_flow_terminal_paths.rs`'s pattern: their default
    // `CommandRunner` never blocks a hermetic run in this suite's shape
    // (`auto_pr: false`, no real `mev`/`gh` on PATH needed for the assertions
    // these tests make), so no override is registered for them.

    let schema = graph::schema();
    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

fn read_state(worktree: &Path) -> serde_json::Value {
    let state_path = worktree
        .join("planning")
        .join(SPEC_SLUG)
        .join("sdlc")
        .join("sdlc-flow-state.json");
    serde_json::from_str(
        &std::fs::read_to_string(&state_path)
            .unwrap_or_else(|err| panic!("state file should exist at {state_path:?}: {err}")),
    )
    .expect("committed state file should parse as JSON")
}

fn run_event() -> serde_json::Value {
    json!({ "spec_slug": SPEC_SLUG, "auto_pr": false, "llm_triage": false })
}

/// THE CENTRAL TEST: under `ReviewMode::EndOnly`, a completed run makes
/// EXACTLY ONE review call, on the drain branch, and that call's prompt
/// carries the spec's COMPLETE Acceptance Criteria (both tasks') and a
/// diff spanning more than one task's changes. Per-task review must be
/// skipped entirely (`TriageRouterNode` routes every `PASS` straight past
/// `ConsolidatedReviewNode` under `EndOnly`) — asserted here, not assumed,
/// since a leftover per-task call would mean the run reviewed the same
/// work twice rather than collapsing review into one end-of-run pass.
#[tokio::test]
async fn end_only_full_run_makes_exactly_one_review_call_with_full_ac_and_multi_task_diff() {
    let worktree = temp_worktree("end-only-headline");
    write_fixture_files(&worktree, 3);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::EndOnly,
        ..SdlcPolicy::default()
    };

    let consolidated_calls = Arc::new(AtomicUsize::new(0));
    let end_review_calls = Arc::new(AtomicUsize::new(0));
    let captured_prompts = Arc::new(Mutex::new(Vec::new()));

    let workflow = build_workflow(
        &worktree,
        &policy,
        make_runner(MULTI_TASK_DIFF, NON_TRIVIAL_NUMSTAT),
        counting_verdict_transport(consolidated_calls.clone(), "PASS"),
        capturing_verdict_transport(
            end_review_calls.clone(),
            captured_prompts.clone(),
            "PASS",
            "looks good",
            vec![],
        ),
    );

    let final_ctx = workflow
        .run(run_event(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        end_review_calls.load(Ordering::SeqCst),
        1,
        "EndOnly must make exactly one end-review call"
    );
    assert_eq!(
        consolidated_calls.load(Ordering::SeqCst),
        0,
        "EndOnly must skip per-task review entirely, for both tasks"
    );

    let prompts = captured_prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];
    assert!(
        prompt.contains("criterion one"),
        "prompt must carry task 1's acceptance criterion"
    );
    assert!(
        prompt.contains("criterion two"),
        "prompt must carry task 2's acceptance criterion"
    );
    assert!(
        prompt.contains("task one change"),
        "diff must span task 1's change"
    );
    assert!(
        prompt.contains("task two change"),
        "diff must span task 2's change"
    );
    drop(prompts);

    assert_eq!(
        final_ctx.node_runs["EndReviewNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        final_ctx.node_runs["PatchDocsNode"].status,
        NodeRunStatus::Success,
        "a PASS verdict must continue on to PatchDocsNode"
    );

    let state = read_state(&worktree);
    assert_eq!(state["status"], json!("done"));
    assert_eq!(state["review"]["verdict"], json!("PASS"));

    let _ = std::fs::remove_dir_all(&worktree);
}

/// The inverse that protects the default path: under `PerTask`,
/// `EndReviewNode` must make ZERO calls, and per-task review must run
/// exactly once per task (two tasks, two calls) — the pre-ticket count,
/// unchanged by this ticket's addition.
#[tokio::test]
async fn per_task_full_run_makes_zero_end_review_calls_and_unchanged_per_task_review_count() {
    let worktree = temp_worktree("per-task-inverse");
    write_fixture_files(&worktree, 3);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::PerTask,
        ..SdlcPolicy::default()
    };

    let consolidated_calls = Arc::new(AtomicUsize::new(0));
    let end_review_calls = Arc::new(AtomicUsize::new(0));

    let workflow = build_workflow(
        &worktree,
        &policy,
        make_runner(MULTI_TASK_DIFF, NON_TRIVIAL_NUMSTAT),
        counting_verdict_transport(consolidated_calls.clone(), "PASS"),
        counting_verdict_transport(end_review_calls.clone(), "PASS"),
    );

    let final_ctx = workflow
        .run(run_event(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        end_review_calls.load(Ordering::SeqCst),
        0,
        "PerTask must make zero end-review calls"
    );
    assert_eq!(
        consolidated_calls.load(Ordering::SeqCst),
        2,
        "PerTask must review each of the two tasks exactly once"
    );
    assert!(
        !final_ctx.nodes.contains_key("EndReviewNode"),
        "the zero-call pass-through must write no ctx.nodes entry"
    );

    let state = read_state(&worktree);
    assert_eq!(state["status"], json!("done"));

    let _ = std::fs::remove_dir_all(&worktree);
}

/// Second inverse: under `TrivialSkip` with a trivial diff, per-task review
/// is skipped too (existing pre-ticket behavior) AND `EndReviewNode` still
/// makes zero calls — the node's presence in the graph must not itself
/// trigger a call outside `EndOnly`.
#[tokio::test]
async fn trivial_skip_full_run_makes_zero_end_review_calls() {
    let worktree = temp_worktree("trivial-skip-inverse");
    write_fixture_files(&worktree, 3);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::TrivialSkip,
        ..SdlcPolicy::default()
    };

    let consolidated_calls = Arc::new(AtomicUsize::new(0));
    let end_review_calls = Arc::new(AtomicUsize::new(0));

    let workflow = build_workflow(
        &worktree,
        &policy,
        make_runner(MULTI_TASK_DIFF, TRIVIAL_NUMSTAT),
        counting_verdict_transport(consolidated_calls.clone(), "PASS"),
        counting_verdict_transport(end_review_calls.clone(), "PASS"),
    );

    let _final_ctx = workflow
        .run(run_event(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(
        end_review_calls.load(Ordering::SeqCst),
        0,
        "TrivialSkip must make zero end-review calls"
    );
    assert_eq!(
        consolidated_calls.load(Ordering::SeqCst),
        0,
        "TrivialSkip over a trivial diff must skip per-task review too"
    );

    let state = read_state(&worktree);
    assert_eq!(state["status"], json!("done"));

    let _ = std::fs::remove_dir_all(&worktree);
}

/// Verdict test: a FAIL from the end review reaches `WrapUpNode` (never
/// `PatchDocsNode`) with a blocked terminal status whose `bail_reason`
/// names the unmet criteria, driven through the real assembled graph
/// rather than `WrapUpNode::process` in isolation.
#[tokio::test]
async fn end_only_fail_verdict_routes_to_wrap_up_with_blocked_status_and_bail_reason() {
    let worktree = temp_worktree("end-only-fail");
    write_fixture_files(&worktree, 3);

    let policy = SdlcPolicy {
        review_mode: ReviewMode::EndOnly,
        ..SdlcPolicy::default()
    };

    let consolidated_calls = Arc::new(AtomicUsize::new(0));
    let end_review_calls = Arc::new(AtomicUsize::new(0));
    let captured_prompts = Arc::new(Mutex::new(Vec::new()));

    let workflow = build_workflow(
        &worktree,
        &policy,
        make_runner(MULTI_TASK_DIFF, NON_TRIVIAL_NUMSTAT),
        counting_verdict_transport(consolidated_calls.clone(), "PASS"),
        capturing_verdict_transport(
            end_review_calls.clone(),
            captured_prompts.clone(),
            "FAIL",
            "criteria unmet",
            vec!["task 2's criterion two was not satisfied"],
        ),
    );

    let final_ctx = workflow
        .run(run_event(), Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    assert_eq!(end_review_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        final_ctx.node_runs.get("PatchDocsNode").map(|r| &r.status),
        Some(&NodeRunStatus::Pending),
        "a FAIL verdict must never reach PatchDocsNode"
    );
    assert_eq!(
        final_ctx.node_runs["WrapUpNode"].status,
        NodeRunStatus::Success,
        "a FAIL verdict must still reach WrapUpNode with a terminal state"
    );

    let state = read_state(&worktree);
    assert_eq!(state["status"], json!("blocked"));
    let bail_reason = state["bail_reason"]
        .as_str()
        .expect("bail_reason should be populated");
    assert!(bail_reason.contains("criteria unmet"));
    assert!(bail_reason.contains("task 2's criterion two was not satisfied"));
    assert_eq!(state["review"]["verdict"], json!("FAIL"));
    assert_eq!(
        state["review"]["findings"],
        json!(["task 2's criterion two was not satisfied"])
    );

    let _ = std::fs::remove_dir_all(&worktree);
}
