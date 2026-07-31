//! End-to-end integration test for the `EN.6.J` run_id stamp + terminal
//! status on failure: drives a minimal hand-built `Workflow` through the
//! real `Workflow::run_with` walk (not a hand-built `TaskContext`), so the
//! `RunOptions::run_id` stamp, the failure path's halted-walk return, and
//! `wrap_up::write_terminal_blocked_state`/`WrapUpNode`'s disk writes are
//! all exercised as production traffic would hit them.
//!
//! Hermetic by construction: every node here is either a tiny fixture
//! (`FixtureSetupNode`/`FixtureLoadStateNode`, mirroring
//! `sdlc_flow_task_loop.rs`'s `FixtureSetupNode` pattern) or a
//! deterministic, no-model, no-subprocess real node (`WrapUpNode`) — no
//! `claude` transport, no `CommandRunner` subprocess, nothing to stub.
//!
//! Test 1 is the block's named acceptance test: a node that deterministically
//! returns `Err` halts the walk (per `Workflow::run_with`'s documented
//! behavior — the accumulated `TaskContext` is still returned as `Ok`), and
//! the failure-path writer produces a `"blocked"` status + `bail_reason` +
//! this run's `run_id` on disk. Test 2 is the happy-path counterpart: a walk
//! that reaches `WrapUpNode` writes `"done"` with the same run_id and a null
//! `bail_reason`. Test 3 is the JS-engine compatibility case: a committed
//! state fixture written with no `run_id` key at all (the shape
//! base-template's `sdlc-flow.js` engine produces) parses cleanly and
//! reports `None`, and rewriting it loses no other D31 field.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::policy::stamp_resolved_policy;
use engine_core::schema::{NodeConfig, WorkflowSchema};
use engine_core::workflow::{RunOptions, Workflow};
use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
use engine_core::workflows::sdlc_flow::schema::{RunMeta, SDLCState, SDLCTask, SDLCTaskStatus};
use engine_core::workflows::sdlc_flow::wrap_up::{write_terminal_blocked_state, WrapUpNode};
use serde_json::json;
use uuid::Uuid;

// --- fixtures ---------------------------------------------------------

/// A fresh, unique temp directory standing in for a worktree — no real
/// `git worktree` involved, matching `sdlc_flow_task_loop.rs`'s
/// `temp_worktree` helper.
fn temp_worktree() -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-sdlc-flow-run-id-terminal-it-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes an initial D31-committed `sdlc-flow-state.json` under
/// `<worktree>/planning/<spec_slug>/sdlc/`, exactly as a prior
/// `SaveStateNode`/`WrapUpNode` write would have left it — required so
/// `write_terminal_blocked_state`'s "parent dir already exists" guard finds
/// something to terminate.
fn seed_state_file(worktree: &Path, spec_slug: &str, state: &SDLCState) {
    let state_dir = worktree.join("planning").join(spec_slug).join("sdlc");
    std::fs::create_dir_all(&state_dir).unwrap();
    let run_meta = RunMeta {
        branch: "sdlc/fixture".to_string(),
        worktree_path: worktree.to_string_lossy().to_string(),
        started_at: "2026-07-01T00:00:00Z".to_string(),
        updated_at: "2026-07-01T00:00:00Z".to_string(),
        run_id: None,
    };
    let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
    let json_str = serde_json::to_string_pretty(&committed).unwrap();
    std::fs::write(state_dir.join("sdlc-flow-state.json"), json_str).unwrap();
}

/// Stands in for the real `SetupWorktreeNode`: stamps a controlled
/// `worktree_path` plus a built-in-default `SdlcPolicy` under
/// `RESOLVED_POLICY_IDENTITY` (the strict stamp `WrapUpNode::process` reads
/// via `resolved_policy_strict`) — mirrors `sdlc_flow_task_loop.rs`'s
/// `FixtureSetupNode`, minus the real policy-file resolution this test
/// doesn't need.
struct FixtureSetupNode {
    worktree_path: String,
}

#[async_trait]
impl Node for FixtureSetupNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({
                "worktree_path": self.worktree_path,
                "branch_name": "sdlc/fixture",
            }),
        );
        stamp_resolved_policy(&mut ctx, &SdlcPolicy::default())?;
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SetupWorktreeNode"
    }
}

/// Stands in for the real `LoadTaskStateNode`: stamps a fixed `SDLCState`
/// verbatim, exactly the shape `latest_state`/`build_run_meta` read.
struct FixtureLoadStateNode {
    state: SDLCState,
}

#[async_trait]
impl Node for FixtureLoadStateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&self.state).expect("SDLCState serializes"),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "LoadTaskStateNode"
    }
}

/// A node that deterministically fails every run — the forced-error node
/// the failure path (`engine-serve`'s post-walk cleanup, stood in here by a
/// direct call to `write_terminal_blocked_state`) reacts to.
struct FailingNode {
    message: String,
}

#[async_trait]
impl Node for FailingNode {
    async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new(self.message.clone()))
    }

    fn name(&self) -> &str {
        "FailingNode"
    }
}

/// Builds a 3-node linear schema: `SetupWorktreeNode -> LoadTaskStateNode ->
/// <tail>`, `<tail>` being whichever terminal node identity the caller
/// wants exercised (`"FailingNode"` or `"WrapUpNode"`).
fn linear_schema(tail_identity: &str) -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        "SetupWorktreeNode".to_string(),
        NodeConfig::new("SetupWorktreeNode", vec!["LoadTaskStateNode".to_string()]),
    );
    nodes.insert(
        "LoadTaskStateNode".to_string(),
        NodeConfig::new("LoadTaskStateNode", vec![tail_identity.to_string()]),
    );
    nodes.insert(
        tail_identity.to_string(),
        NodeConfig::new(tail_identity, vec![]),
    );
    WorkflowSchema::new("EN_6_J_RUN_ID_TERMINAL_FIXTURE", "SetupWorktreeNode", nodes)
}

// --- Test 1: forced node error leaves a terminal "blocked" status -----

#[tokio::test]
async fn forced_node_error_leaves_terminal_blocked_status_with_run_id() {
    let worktree = temp_worktree();

    let mut initial_state = SDLCState::new("EN.6.J-terminal-fixture");
    let mut task = SDLCTask::new(1, "One", "d1");
    task.status = SDLCTaskStatus::InProgress;
    initial_state.tasks.push(task);
    seed_state_file(&worktree, &initial_state.spec_slug, &initial_state);

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(FixtureLoadStateNode {
        state: initial_state,
    }));
    registry.register(Box::new(FailingNode {
        message: "node ImplementTaskNode failed: boom".to_string(),
    }));

    let workflow = Workflow::new(registry, linear_schema("FailingNode"));

    let run_id = Uuid::new_v4();
    let final_ctx = workflow
        .run_with(
            json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            RunOptions {
                run_id: Some(run_id),
                ..Default::default()
            },
        )
        .await
        .expect("a failed node halts the walk but run_with still returns Ok(ctx)");

    let failing_run = final_ctx
        .node_runs
        .get("FailingNode")
        .expect("FailingNode should have a recorded NodeRun");
    assert_eq!(failing_run.status, NodeRunStatus::Failed);
    let reason = failing_run
        .error
        .clone()
        .expect("a FAILED NodeRun should carry an error message");

    let saved_to = write_terminal_blocked_state(&final_ctx, &reason)
        .expect("worktree + loaded state present and the sdlc/ dir already exists");

    let on_disk = std::fs::read_to_string(&saved_to).unwrap();
    let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(value["status"], json!("blocked"));
    assert_eq!(value["bail_reason"], json!(reason));
    assert_eq!(value["run_id"], json!(run_id.to_string()));

    let _ = std::fs::remove_dir_all(&worktree);
}

// --- Test 2: the happy path reaches WrapUpNode and writes "done" -------

#[tokio::test]
async fn clean_run_reaches_wrap_up_and_writes_done_status_with_run_id() {
    let worktree = temp_worktree();

    let mut state = SDLCState::new("EN.6.J-terminal-fixture-happy");
    let mut task = SDLCTask::new(1, "One", "d1");
    task.status = SDLCTaskStatus::Done;
    state.tasks.push(task);
    state.telemetry.tasks_passed = 1;

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(FixtureSetupNode {
        worktree_path: worktree.to_string_lossy().to_string(),
    }));
    registry.register(Box::new(FixtureLoadStateNode { state }));
    registry.register(Box::new(WrapUpNode::new()));

    let workflow = Workflow::new(registry, linear_schema("WrapUpNode"));

    let run_id = Uuid::new_v4();
    let final_ctx = workflow
        .run_with(
            json!({}),
            Box::new(|_ctx: &TaskContext| {}),
            RunOptions {
                run_id: Some(run_id),
                ..Default::default()
            },
        )
        .await
        .expect("a clean run should complete without a WorkflowError");

    let wrap_up_run = final_ctx
        .node_runs
        .get("WrapUpNode")
        .expect("WrapUpNode should have a recorded NodeRun");
    assert_eq!(wrap_up_run.status, NodeRunStatus::Success);

    let saved_to = final_ctx.nodes["WrapUpNode"]["saved_to"]
        .as_str()
        .expect("WrapUpNode should have persisted to disk (worktree present)");

    let on_disk = std::fs::read_to_string(saved_to).unwrap();
    let value: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(value["status"], json!("done"));
    assert_eq!(value["run_id"], json!(run_id.to_string()));
    assert!(value["bail_reason"].is_null());

    let _ = std::fs::remove_dir_all(&worktree);
}

// --- Test 3: JS-engine compatibility (no `run_id` key at all) ----------

#[test]
fn committed_json_without_run_id_key_parses_as_none_and_preserves_other_d31_fields() {
    let js_engine_shape = json!({
        "spec_slug": "js-engine-fixture",
        "branch": "sdlc/js-engine-fixture",
        "worktree_path": "/trees/js-engine-fixture",
        "started_at": "2026-07-01T00:00:00Z",
        "updated_at": "2026-07-02T00:00:00Z",
        "status": "done",
        "current_task": null,
        "tasks": {},
        "bail_reason": null,
    });

    let parsed = SDLCState::from_committed_state_json(&js_engine_shape)
        .expect("a committed JSON with no run_id key at all must still parse cleanly");
    assert_eq!(parsed.run_id, None);
    assert_eq!(parsed.spec_slug, "js-engine-fixture");
    assert_eq!(
        parsed.branch.as_deref(),
        Some("sdlc/js-engine-fixture"),
        "other D31 round-trip fields must survive an absent run_id key"
    );
    assert_eq!(
        parsed.worktree_path.as_deref(),
        Some("/trees/js-engine-fixture")
    );
    assert_eq!(parsed.started_at.as_deref(), Some("2026-07-01T00:00:00Z"));

    // A subsequent write over this JS-engine-authored state must not lose
    // any other D31 field, and must still emit an explicit `run_id: null`
    // (never a dropped key) now that an engine-rs writer owns the file.
    let run_meta = RunMeta {
        branch: parsed.branch.clone().unwrap_or_default(),
        worktree_path: parsed.worktree_path.clone().unwrap_or_default(),
        started_at: parsed.started_at.clone().unwrap_or_default(),
        updated_at: "2026-07-03T00:00:00Z".to_string(),
        run_id: None,
    };
    let rewritten = parsed.to_committed_state_json(&run_meta, None, None, None, None, None);
    assert_eq!(rewritten["run_id"], serde_json::Value::Null);
    assert_eq!(rewritten["branch"], json!("sdlc/js-engine-fixture"));
    assert_eq!(
        rewritten["worktree_path"],
        json!("/trees/js-engine-fixture")
    );
    assert_eq!(rewritten["started_at"], json!("2026-07-01T00:00:00Z"));
}
