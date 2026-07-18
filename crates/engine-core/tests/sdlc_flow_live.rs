//! Live smoke tests for the `SDLC_FLOW` workflow's three model-calling
//! nodes (`ImplementTaskNode`, `TriageTaskNode`'s `llm_triage` branch,
//! `ConsolidatedReviewNode`) — the actual `claude` CLI, not a stubbed
//! transport, mirroring the `#[ignore]` + manual-run convention already
//! used by `crates/engine-core/tests/claude_code_step.rs` and
//! `core/claude-code-rs/src/execute.rs`'s live tests.
//!
//! **Ignored by default** so the gated `cargo test` suite (and CI) never
//! spawns a real subprocess or spends real tokens. Run manually, with a
//! logged-in `claude` CLI available on `PATH`:
//!
//! ```sh
//! cargo test -p engine-core --test sdlc_flow_live -- --ignored --test-threads=1
//! ```
//!
//! Every other model/subprocess seam (`PatchDocsNode`, `SaveStateNode`,
//! `PullRequestNode`, `EmitStateNode`, `ConsolidatedReviewNode`'s `git diff`
//! context) stays stubbed, same as `sdlc_flow_e2e.rs` — these tests exist to
//! prove the three tunable model nodes genuinely round-trip through the
//! real `claude` CLI end-to-end (real argv, real subprocess, real JSON
//! parse), not to re-validate the graph's deterministic routing (already
//! covered hermetically by `sdlc_flow_e2e.rs`/`sdlc_flow_task_loop.rs`).
//!
//! `ImplementTaskNode`'s real call is scoped with
//! `dangerously_skip_permissions: true` (required — headless `-p` mode has
//! no TTY to approve a file-edit tool interactively) and
//! `disallowed_tools: ["Bash"]`, so the session can create/edit files
//! inside its `cwd`-scoped temp worktree via the CLI's own file tools but
//! can never run an arbitrary shell command. This is an opt-in `Config`
//! built by the test itself via `ImplementTaskNode::with_config` — it does
//! not change `ImplementTaskNode::new()`'s own safe-by-default
//! construction used everywhere else (including production).
//!
//! Every real call in this file also sets `isolated: true`. This is
//! required, not optional, whenever these tests run from inside another
//! live `claude` session (e.g. driven by an in-session coding agent, as
//! opposed to a bare terminal) — without it, a nested subprocess call
//! contends with the parent session's shared `~/.claude` credentials and
//! can hang indefinitely rather than failing fast (observed live while
//! writing this test; see `claude_code_rs::Config::isolated`'s doc
//! comment for the underlying credential-refresh race it avoids).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::workflow::Workflow;
use engine_core::workflows::sdlc_flow::docs::PatchDocsNode;
use engine_core::workflows::sdlc_flow::emit_state::EmitStateNode;
use engine_core::workflows::sdlc_flow::graph;
use engine_core::workflows::sdlc_flow::pr::PullRequestNode;
use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
use engine_core::workflows::sdlc_flow::setup::{
    CommandOutput, CommandRunner, GenerateTasksNode, LoadTaskStateNode, SpecExistsRouterNode,
};
use engine_core::workflows::sdlc_flow::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, IncrementAttemptNode, ReviewRouterNode,
    SaveStateNode, TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode,
    UpdateTaskStatusNode,
};
use engine_core::workflows::sdlc_flow::wrap_up::WrapUpNode;
use serde_json::json;

/// Replaces the real `SetupWorktreeNode`, as in `sdlc_flow_e2e.rs`: writes
/// a controlled temp-dir `worktree_path` directly, so `ImplementTaskNode`'s
/// real session is scoped to it (via `Config.cwd`) rather than the test
/// binary's ambient cwd.
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
                "branch_name": "sdlc/fixture-live-spec",
            }),
        );
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
    }
}

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-live-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning").join("fixture-live-spec")).unwrap();
    dir
}

const MARKER_FILE: &str = "LIVE_TEST_MARKER.txt";
const MARKER_CONTENT: &str = "integration-test-ok";

/// Writes a fixture `tasks.json` (one task asking for a trivial, unambiguous
/// file edit a real model should reliably complete) and a `harness.json`
/// whose one gating check really `grep`s the worktree for the expected
/// file/content via the *real* `sh -c` subprocess (no stub) — the concrete,
/// independent proof that `ImplementTaskNode`'s real session actually wrote
/// the file into the scoped worktree (exercising the `Config.cwd` fix).
fn write_fixture_files(worktree: &Path, max_attempts: u32) {
    let spec_dir = worktree.join("planning").join("fixture-live-spec");
    let tasks = json!([
        {
            "task_id": 1,
            "title": "Create a marker file",
            "description": format!(
                "Create a file named exactly `{MARKER_FILE}` in the current \
                 working directory. Its entire content must be the single \
                 line `{MARKER_CONTENT}` and nothing else. Do not create or \
                 modify any other file."
            ),
            "acceptance_criteria": [
                format!("{MARKER_FILE} exists in the working directory"),
                format!("Its content is exactly \"{MARKER_CONTENT}\""),
            ],
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
                {
                    "name": "marker-file-present",
                    "kind": "command",
                    "command": format!("grep -qx {MARKER_CONTENT} {MARKER_FILE}"),
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

/// Injected runner for every non-LLM subprocess seam this test doesn't care
/// about exercising for real (`git`/`gh`/`mev`) — always succeeds with
/// empty output, same as `sdlc_flow_e2e.rs`'s `noop_git_runner`.
fn noop_runner() -> CommandRunner {
    Arc::new(|_program, _args, _cwd| {
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    })
}

/// Builds the full `SDLC_FLOW` `Workflow`: the real declared graph from
/// [`graph::schema`], with `ImplementTaskNode` and `ConsolidatedReviewNode`
/// left at their real default transport (no `.with_transport` override) —
/// every other model/subprocess seam stays stubbed.
fn build_live_workflow(worktree: &Path) -> Workflow {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));

    // Real call: dangerously_skip_permissions is required in headless `-p`
    // mode (no TTY to approve a file-edit tool), scoped down by
    // disallowed_tools so the session can only use file-editing tools, never
    // a shell — see the module doc for the full rationale. `isolated: true`
    // is required whenever this test itself runs from inside another live
    // `claude` session (as it does when driven by an in-session agent) —
    // without it, the nested subprocess contends with the parent session's
    // shared credentials file and can hang indefinitely rather than erroring
    // (observed live; see `Config::isolated`'s doc comment).
    registry.register(Box::new(ImplementTaskNode::new().with_config(Config {
        model: Some("claude-sonnet-4-5".to_string()),
        disallowed_tools: vec!["Bash".to_string()],
        dangerously_skip_permissions: true,
        isolated: true,
        ..Config::default()
    })));

    // Real subprocess: a genuine `sh -c 'grep ...'` against the file the
    // real ImplementTaskNode call (should have) just written.
    registry.register(Box::new(TestTaskNode::new()));
    registry.register(Box::new(TriageTaskNode::new().with_config(Config {
        model: Some("claude-sonnet-4-5".to_string()),
        isolated: true,
        ..Config::default()
    })));
    registry.register(Box::new(TriageRouterNode));

    // Real call: git-diff context is stubbed (a real, empty single-task
    // diff would tell the reviewer nothing useful), transport is real.
    registry.register(Box::new(
        ConsolidatedReviewNode::new()
            .with_runner(noop_runner())
            .with_config(Config {
                model: Some("claude-sonnet-4-5".to_string()),
                isolated: true,
                ..Config::default()
            }),
    ));

    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(SaveStateNode::new().with_runner(noop_runner())));
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
    registry.register(Box::new(PullRequestNode::new().with_runner(noop_runner())));
    registry.register(Box::new(EmitStateNode::new().with_runner(noop_runner())));

    let schema = graph::schema();
    Workflow::new_validated(registry, schema)
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

/// Live smoke test — actually runs the whole `SDLC_FLOW` graph with a real
/// `claude` CLI session driving `ImplementTaskNode` (real file edit, tool
/// use bypassed for headless mode, scoped to a temp worktree) and
/// `ConsolidatedReviewNode` (real judgment call over a stubbed diff
/// context). Ignored so the gated `cargo test` suite stays hermetic; run
/// manually with `cargo test -p engine-core --test sdlc_flow_live --
/// --ignored` when the `claude` CLI is available and authenticated.
#[tokio::test]
#[ignore]
async fn live_full_workflow_real_implement_and_review() {
    let worktree = temp_worktree("happy");
    // One retry of headroom: a trivial task should pass on the first real
    // attempt, but a minor format slip (e.g. a stray trailing newline the
    // exact-match check doesn't tolerate) gets one real re-attempt before
    // this test would report a hard failure.
    write_fixture_files(&worktree, 2);

    let workflow = build_live_workflow(&worktree);
    let event = json!({ "spec_slug": "fixture-live-spec", "auto_pr": false });

    let final_ctx = workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("workflow run should not error");

    // --- The real ImplementTaskNode call happened and reported real usage ---
    let implement_run = final_ctx
        .node_runs
        .get("ImplementTaskNode")
        .expect("ImplementTaskNode should have run");
    assert_eq!(implement_run.status, NodeRunStatus::Success);
    assert!(
        implement_run
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0)
            > 0,
        "a real claude call should report nonzero output tokens, not a stub's canned zero"
    );

    // --- The real session actually wrote the marker file into the scoped --
    // --- worktree (proves the Config.cwd fix works end-to-end) -----------
    let marker_path = worktree.join(MARKER_FILE);
    assert!(
        marker_path.exists(),
        "expected the real Claude Code session to create {} inside the scoped \
         worktree {} — got: {:?}",
        MARKER_FILE,
        worktree.display(),
        std::fs::read_dir(&worktree).ok().map(|entries| entries
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect::<Vec<_>>())
    );
    let content = std::fs::read_to_string(&marker_path).unwrap_or_default();
    assert_eq!(content.trim(), MARKER_CONTENT);

    // --- The real (non-stubbed) `sh -c grep` check actually passed --------
    let test_result = final_ctx
        .nodes
        .get("TestTaskNode")
        .expect("TestTaskNode should have run");
    assert_eq!(test_result["all_passed"], json!(true));

    // --- The real ConsolidatedReviewNode call happened and parsed --------
    let review_run = final_ctx
        .node_runs
        .get("ConsolidatedReviewNode")
        .expect("ConsolidatedReviewNode should have run on the PASS path");
    assert_eq!(review_run.status, NodeRunStatus::Success);
    assert!(
        review_run
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0)
            > 0,
        "a real review call should report nonzero output tokens"
    );
    let review_result = final_ctx
        .nodes
        .get("ConsolidatedReviewNode")
        .expect("ConsolidatedReviewNode should have stamped a result");
    let verdict = review_result["verdict"]
        .as_str()
        .expect("verdict should be a string");
    assert!(
        ["PASS", "FAIL", "PARTIAL"].contains(&verdict),
        "unexpected verdict from a real review call: {verdict:?}"
    );

    // --- The tail is still reached ------------------------------------------
    for identity in ["WrapUpNode", "PullRequestNode", "EmitStateNode"] {
        let run = final_ctx
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("expected a NodeRun for '{identity}'"));
        assert_eq!(run.status, NodeRunStatus::Success);
    }

    std::fs::remove_dir_all(&worktree).ok();
}

/// Live smoke test for the third tunable model node: `TriageTaskNode`'s
/// `llm_triage` branch, driven directly (not through the full graph, unlike
/// the test above) — a narrower, cheaper real call, mirroring the style of
/// `task_loop.rs`'s own inline `triage_llm_gate_invokes_model_when_enabled`
/// unit test but with no transport stub. Ignored for the same reason; run
/// manually with `cargo test -p engine-core --test sdlc_flow_live --
/// --ignored`.
#[tokio::test]
#[ignore]
async fn live_triage_task_node_llm_branch_real_call() {
    let mut task = SDLCTask::new(1, "Add a config validator", "Validate config on startup");
    task.max_attempts = 3;
    task.attempt_count = 0;
    let mut state = SDLCState::new("fixture-live-triage");
    state.tasks = vec![task.clone()];

    let ctx = TaskContext {
        event: json!({ "spec_slug": "fixture-live-triage", "llm_triage": true }),
        nodes: HashMap::from([
            (
                "LoadTaskStateNode".to_string(),
                serde_json::to_value(&state).unwrap(),
            ),
            (
                "TaskQueueRouterNode".to_string(),
                json!({
                    "current_task_id": task.task_id,
                    "title": task.title,
                    "description": task.description,
                    "acceptance_criteria": task.acceptance_criteria,
                    "attempt_count": task.attempt_count,
                    "max_attempts": task.max_attempts,
                }),
            ),
            (
                "TestTaskNode".to_string(),
                json!({
                    "all_passed": false,
                    "check_results": [],
                    "failure_summary": "Failed checks: config-validator (exit code 1)",
                }),
            ),
        ]),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };

    let node = TriageTaskNode::new().with_config(Config {
        model: Some("claude-sonnet-4-5".to_string()),
        isolated: true,
        ..Config::default()
    });
    let out = node.process(ctx).await.expect("process should succeed");

    let run = out
        .node_runs
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have run");
    // No `status == Success` assertion here: `ClaudeCodeStep` only ever
    // stamps `Running` into `node_runs` on its own — finalizing a node's
    // status to `Success`/`Failed` is `Workflow::run_with`'s job (see
    // `workflow.rs`), which this test deliberately bypasses to drive
    // `TriageTaskNode` directly. `usage` being populated is what actually
    // proves the real call completed.
    assert!(
        run.usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0)
            > 0,
        "a real triage call should report nonzero output tokens, not a stub's canned zero"
    );

    let result = out
        .nodes
        .get("TriageTaskNode")
        .expect("TriageTaskNode should have stamped a result");
    let verdict = result["verdict"]
        .as_str()
        .expect("verdict should be a string");
    assert!(
        ["RETRYABLE", "MAJOR_BAIL"].contains(&verdict),
        "unexpected verdict from a real triage call: {verdict:?}"
    );
    assert!(!result["reason"].as_str().unwrap_or_default().is_empty());
}
