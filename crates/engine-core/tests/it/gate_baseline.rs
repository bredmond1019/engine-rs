//! `EN.17.G` task 5: integration tests for the pre-run baseline snapshot
//! (task 2), the persisted-snapshot read with its post-implementation
//! fallback (task 3), and `failureClass: escalate`'s bail-without-retry
//! behavior (task 4) — driven through engine-core's PUBLIC node API from
//! outside the crate, unlike `task_loop.rs`'s own unit tests, which have
//! private-item access. Every test name is prefixed `gate_baseline_`.
//!
//! `LoadTaskStateNode` is `EN.17.G` task 2's actual pre-run snapshot site
//! (`setup::snapshot_baselines`, called unconditionally from its
//! `process`) — these tests drive it for real rather than fabricating the
//! snapshot file by hand, so the snapshot's own resume-safety and its exact
//! on-disk path are exercised, not merely assumed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError};
use engine_core::policy::stamp_resolved_policy;
use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
use engine_core::workflows::sdlc_flow::setup::{resolve_policy_for_run, LoadTaskStateNode};
use engine_core::workflows::sdlc_flow::task_loop::{TestTaskNode, TriageTaskNode};
use engine_core::workflows::ModelTransport;
use serde_json::json;

/// Fresh empty temp dir with `planning/<spec_slug>/` created, unique per
/// call (PID + monotonic counter) so concurrent `cargo nextest` processes
/// never collide — mirrors `sdlc_flow_task_loop.rs`'s own `temp_worktree`.
fn temp_worktree(tag: &str, spec_slug: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-gate-baseline-it-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: PID recycling means a leftover dir from a prior run
    // under the same PID must be cleared before recreating it.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning").join(spec_slug)).unwrap();
    dir
}

fn write_tasks_json(worktree: &Path, spec_slug: &str, task: &SDLCTask) {
    let spec_dir = worktree.join("planning").join(spec_slug);
    let tasks = json!([{
        "task_id": task.task_id,
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "max_attempts": task.max_attempts,
        "expects_writes": task.expects_writes,
    }]);
    std::fs::write(
        spec_dir.join("tasks.json"),
        serde_json::to_string_pretty(&tasks).unwrap(),
    )
    .unwrap();
}

fn write_harness(worktree: &Path, checks: serde_json::Value) {
    let harness = json!({ "validation": { "checks": checks } });
    std::fs::write(
        worktree.join("planning").join("harness.json"),
        serde_json::to_string(&harness).unwrap(),
    )
    .unwrap();
}

/// `<worktree>/planning/<spec_slug>/sdlc/baseline-<check_name>.txt` — the
/// exact path `setup::baseline_snapshot_path` derives, reproduced here
/// (that function is crate-private) for a check `name` that is already
/// filesystem-safe on its own, so no separate slugging is needed to name
/// the path this test asserts against.
fn baseline_path(worktree: &Path, spec_slug: &str, check_name: &str) -> PathBuf {
    worktree
        .join("planning")
        .join(spec_slug)
        .join("sdlc")
        .join(format!("baseline-{check_name}.txt"))
}

/// Builds a ctx carrying `SetupWorktreeNode`'s output and a stamped default
/// resolved policy — the minimum `LoadTaskStateNode::process` needs to run
/// for real (its fresh-bootstrap path calls the strict `resolved_policy`
/// read to seed `max_attempts`).
fn ctx_for_load(worktree: &Path, spec_slug: &str) -> TaskContext {
    let mut ctx = TaskContext {
        event: json!({ "spec_slug": spec_slug }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
    let policy =
        resolve_policy_for_run(&ctx, worktree).expect("resolve_policy_for_run should succeed");
    stamp_resolved_policy(&mut ctx, &policy).expect("stamp_resolved_policy should succeed");
    ctx
}

/// Stamps `TaskQueueRouterNode`'s per-dispatch snapshot for `task` — the
/// shape `TestTaskNode`/`TriageTaskNode` both read `current_task_id` (and,
/// for `TriageTaskNode`'s deterministic branches, the rest) from.
fn stamp_task_queue_router(ctx: &mut TaskContext, task: &SDLCTask) {
    ctx.nodes.insert(
        "TaskQueueRouterNode".to_string(),
        json!({
            "current_task_id": task.task_id,
            "title": task.title,
            "description": task.description,
            "acceptance_criteria": task.acceptance_criteria,
            "attempt_count": task.attempt_count,
            "max_attempts": task.max_attempts,
        }),
    );
}

/// Stamps a fabricated `LoadTaskStateNode` result directly — used only by
/// tests that deliberately never call the real node (to prove the
/// no-pre-run-snapshot fallback path).
fn stamp_loaded_state(ctx: &mut TaskContext, spec_slug: &str, task: &SDLCTask) {
    let mut state = SDLCState::new(spec_slug);
    state.tasks = vec![task.clone()];
    ctx.nodes.insert(
        "LoadTaskStateNode".to_string(),
        serde_json::to_value(&state).unwrap(),
    );
}

fn panicking_transport() -> ModelTransport {
    Arc::new(|_config, _prompt| {
        panic!("transport should not be invoked for a deterministic triage branch")
    })
}

/// A minimal `Node` reading whether the given baseline file exists at the
/// moment it runs, stamping the boolean into its own result — a stand-in
/// for `ImplementTaskNode`, whose only relevant behavior for this test is
/// running strictly AFTER the pre-run snapshot.
struct StubImplementNode {
    baseline_file: PathBuf,
}

#[async_trait::async_trait]
impl Node for StubImplementNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            "StubImplementNode".to_string(),
            json!({ "baseline_existed": self.baseline_file.exists() }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "StubImplementNode"
    }
}

// ---------------------------------------------------------------------
// gate_baseline_net_new_entry_from_the_task_fails
// ---------------------------------------------------------------------

/// The end-to-end proof `EN.17.G` exists for: a task-introduced net-new
/// entry is caught because the baseline was captured BEFORE the task ran
/// (`LoadTaskStateNode`, driven here for real) — and, in a fixture with no
/// pre-run snapshot at all (never calling `LoadTaskStateNode`), the same
/// harness check WRONGLY passes, because `baselineCommand` then has no
/// choice but to run live, after the task's own change is already on disk.
/// This is the exact gap `task_loop.rs`'s own
/// `baseline_diff_reads_persisted_snapshot_over_a_contaminated_live_baseline`
/// unit test records as OBSERVED RED (D68) before the fix; this test proves
/// the same thing through the public API this crate exposes to callers
/// outside it.
#[tokio::test]
async fn gate_baseline_net_new_entry_from_the_task_fails() {
    let spec_slug = "gate-baseline-spec";

    // --- WITH the pre-run snapshot: the task-introduced entry is caught ---
    let worktree = temp_worktree("net-new-with-snapshot", spec_slug);
    write_harness(
        &worktree,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
            "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
        }]),
    );
    let mut task = SDLCTask::new(1, "t", "d");
    task.expects_writes = false;
    write_tasks_json(&worktree, spec_slug, &task);

    let ctx = ctx_for_load(&worktree, spec_slug);
    let ctx = LoadTaskStateNode::new()
        .process(ctx)
        .await
        .expect("LoadTaskStateNode should take the pre-run snapshot");
    assert!(
        baseline_path(&worktree, spec_slug, "net-new-lint").exists(),
        "LoadTaskStateNode must persist the pre-run snapshot before the task loop starts"
    );

    let mut ctx = ctx;
    stamp_task_queue_router(&mut ctx, &task);
    let out = TestTaskNode::new()
        .process(ctx)
        .await
        .expect("TestTaskNode should process");
    assert_eq!(
        out.nodes["TestTaskNode"]["all_passed"], false,
        "the task-introduced net-new entry must be caught against the pre-run snapshot"
    );
    let results = out.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .unwrap();
    assert_eq!(results[0]["message"], "1 net-new violation(s)");

    // --- WITHOUT a pre-run snapshot (never calling LoadTaskStateNode): the
    // check wrongly passes, because `baselineCommand` has no choice but to
    // run live, AFTER the task's change is already on disk. Contaminate
    // `baselineCommand` to match `command`'s (post-change) output — the
    // exact shape a live-run baseline would take.
    let worktree2 = temp_worktree("net-new-no-snapshot", spec_slug);
    write_harness(
        &worktree2,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
            "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
        }]),
    );
    let mut ctx2 = ctx_for_load(&worktree2, spec_slug);
    stamp_loaded_state(&mut ctx2, spec_slug, &task);
    stamp_task_queue_router(&mut ctx2, &task);
    let out2 = TestTaskNode::new()
        .process(ctx2)
        .await
        .expect("TestTaskNode should process");
    assert_eq!(
        out2.nodes["TestTaskNode"]["all_passed"], true,
        "with no pre-run snapshot, a post-implementation-equivalent baseline wrongly passes \
         the task-introduced regression — this is the exact bug EN.17.G fixes"
    );
}

// ---------------------------------------------------------------------
// gate_baseline_snapshot_precedes_first_implement
// ---------------------------------------------------------------------

/// The pre-run baseline file exists before the first task's implement stage
/// runs — asserted by a stub implement node that reads it immediately after
/// `LoadTaskStateNode::process` returns.
#[tokio::test]
async fn gate_baseline_snapshot_precedes_first_implement() {
    let spec_slug = "gate-baseline-precedes";
    let worktree = temp_worktree("precedes", spec_slug);
    write_harness(
        &worktree,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[]'",
            "command": "echo '[]'",
        }]),
    );
    let mut task = SDLCTask::new(1, "t", "d");
    task.expects_writes = false;
    write_tasks_json(&worktree, spec_slug, &task);

    let ctx = ctx_for_load(&worktree, spec_slug);
    let ctx = LoadTaskStateNode::new()
        .process(ctx)
        .await
        .expect("LoadTaskStateNode should succeed");

    let baseline_file = baseline_path(&worktree, spec_slug, "net-new-lint");
    let stub = StubImplementNode {
        baseline_file: baseline_file.clone(),
    };
    let out = stub.process(ctx).await.expect("stub node should process");
    assert_eq!(
        out.nodes["StubImplementNode"]["baseline_existed"], true,
        "the pre-run snapshot must already exist by the time the first task's implement stage \
         runs"
    );
}

// ---------------------------------------------------------------------
// gate_baseline_resume_keeps_existing_snapshot
// ---------------------------------------------------------------------

/// A resumed run (a second `LoadTaskStateNode::process` invocation over the
/// same spec) finds the existing baseline file and does not overwrite it —
/// its mtime and bytes are unchanged. A DIFFERENT `baselineCommand` on the
/// second invocation proves this: if the file were re-run, its content
/// would visibly change; the resume-safe path must never even try.
#[tokio::test]
async fn gate_baseline_resume_keeps_existing_snapshot() {
    let spec_slug = "gate-baseline-resume";
    let worktree = temp_worktree("resume", spec_slug);
    write_harness(
        &worktree,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
            "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
        }]),
    );
    let mut task = SDLCTask::new(1, "t", "d");
    task.expects_writes = false;
    write_tasks_json(&worktree, spec_slug, &task);

    let ctx1 = ctx_for_load(&worktree, spec_slug);
    LoadTaskStateNode::new()
        .process(ctx1)
        .await
        .expect("first invocation should take the snapshot");

    let baseline_file = baseline_path(&worktree, spec_slug, "net-new-lint");
    let first_bytes = std::fs::read(&baseline_file).unwrap();
    let first_mtime = std::fs::metadata(&baseline_file)
        .unwrap()
        .modified()
        .unwrap();

    // Change `baselineCommand` (and only it) before the "resumed" call.
    write_harness(
        &worktree,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"},{\"file\":\"b.py\",\"code\":\"E2\"}]'",
            "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
        }]),
    );

    let ctx2 = ctx_for_load(&worktree, spec_slug);
    LoadTaskStateNode::new()
        .process(ctx2)
        .await
        .expect("resumed invocation should not error");

    let second_bytes = std::fs::read(&baseline_file).unwrap();
    let second_mtime = std::fs::metadata(&baseline_file)
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        first_bytes, second_bytes,
        "a resumed run must not overwrite the existing snapshot's bytes"
    );
    assert_eq!(
        first_mtime, second_mtime,
        "a resumed run must not touch the existing snapshot's mtime"
    );
}

// ---------------------------------------------------------------------
// gate_baseline_missing_snapshot_falls_back_loudly
// ---------------------------------------------------------------------

/// The pre-run snapshot file is absent (deleted, or never taken — here,
/// `LoadTaskStateNode` is simply never called for this fixture): the check
/// still runs, never errors, falls back to `baselineCommand` live, and its
/// `message` states that fact explicitly even though the check passes.
#[tokio::test]
async fn gate_baseline_missing_snapshot_falls_back_loudly() {
    let spec_slug = "gate-baseline-missing";
    let worktree = temp_worktree("missing", spec_slug);
    write_harness(
        &worktree,
        json!([{
            "kind": "baseline-diff",
            "name": "net-new-lint",
            "gates": true,
            "compareKeys": ["file", "code"],
            "baselineCommand": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
            "command": "echo '[{\"file\":\"a.py\",\"code\":\"E1\"}]'",
        }]),
    );
    let mut task = SDLCTask::new(1, "t", "d");
    task.expects_writes = false;

    let mut ctx = ctx_for_load(&worktree, spec_slug);
    stamp_loaded_state(&mut ctx, spec_slug, &task);
    stamp_task_queue_router(&mut ctx, &task);

    let out = TestTaskNode::new()
        .process(ctx)
        .await
        .expect("TestTaskNode should process");
    assert_eq!(out.nodes["TestTaskNode"]["all_passed"], true);
    let results = out.nodes["TestTaskNode"]["check_results"]
        .as_array()
        .unwrap();
    assert_eq!(
        results[0]["message"],
        "baseline taken post-implementation (no pre-run snapshot found)"
    );
}

// ---------------------------------------------------------------------
// gate_baseline_escalate_class_skips_retries
// ---------------------------------------------------------------------

/// A failed check declaring `failureClass: escalate` fails its task on the
/// FIRST attempt — before `max_attempts` is anywhere near exhausted — and
/// never invokes the LLM triage transport (`panicking_transport` proves
/// that: any call panics the test).
#[tokio::test]
async fn gate_baseline_escalate_class_skips_retries() {
    let mut task = SDLCTask::new(3, "Three", "d3");
    task.max_attempts = 3;
    task.attempt_count = 0; // first attempt — nowhere near exhausted

    let mut ctx = TaskContext {
        event: json!({ "spec_slug": "gate-baseline-escalate" }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    stamp_loaded_state(&mut ctx, "gate-baseline-escalate", &task);
    stamp_task_queue_router(&mut ctx, &task);
    ctx.nodes.insert(
        "TestTaskNode".to_string(),
        json!({
            "all_passed": false,
            "failure_summary": "1 check failed",
            "check_results": [{
                "name": "cargo audit",
                "kind": "command",
                "passed": false,
                "output": "vulnerable dependency found",
                "message": "exit code 1",
                "failure_class": "escalate",
            }],
        }),
    );

    let node = TriageTaskNode::new().with_transport(panicking_transport());
    let out = node
        .process(ctx)
        .await
        .expect("process should succeed without ever reaching the LLM");

    assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "MAJOR_BAIL");
    let reason = out.nodes["TriageTaskNode"]["reason"]
        .as_str()
        .expect("reason is a string");
    assert!(
        reason.contains("cargo audit"),
        "bail reason must name the escalating check: {reason}"
    );
    assert!(
        reason.contains("escalate"),
        "bail reason should state why it bailed without retry: {reason}"
    );
}

// ---------------------------------------------------------------------
// gate_baseline_fixable_default_retries
// ---------------------------------------------------------------------

/// Companion to the above: a failed check with no `failureClass` (the
/// default `Fixable`) retries exactly as today — under budget with
/// `llm_triage` off (the default), it is `RETRYABLE`, never `MAJOR_BAIL`,
/// and (like the escalate path when it does not fire) the deterministic
/// RETRYABLE branch never reaches the LLM transport either.
#[tokio::test]
async fn gate_baseline_fixable_default_retries() {
    let mut task = SDLCTask::new(4, "Four", "d4");
    task.max_attempts = 3;
    task.attempt_count = 0;

    let mut ctx = TaskContext {
        event: json!({ "spec_slug": "gate-baseline-fixable" }),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    stamp_loaded_state(&mut ctx, "gate-baseline-fixable", &task);
    stamp_task_queue_router(&mut ctx, &task);
    stamp_resolved_policy(&mut ctx, &SdlcPolicy::default())
        .expect("stamp_resolved_policy should succeed");
    ctx.nodes.insert(
        "TestTaskNode".to_string(),
        json!({
            "all_passed": false,
            "failure_summary": "1 check failed",
            "check_results": [{
                "name": "cargo nextest run --workspace",
                "kind": "command",
                "passed": false,
                "output": "1 test failed",
                "message": "exit code 100",
                "failure_class": "fixable",
            }],
        }),
    );

    let node = TriageTaskNode::new().with_transport(panicking_transport());
    let out = node
        .process(ctx)
        .await
        .expect("process should succeed without reaching the LLM (llm_triage is off)");
    assert_eq!(out.nodes["TriageTaskNode"]["verdict"], "RETRYABLE");
}
