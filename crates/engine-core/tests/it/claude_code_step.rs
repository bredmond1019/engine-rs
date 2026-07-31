//! Integration tests for `ClaudeCodeStep` (`EN.2.A` task 3).
//!
//! Hermetic (gated) tests drive `ClaudeCodeStep` through `Workflow::run` with
//! a stubbed transport (the injectable seam from `EN.2.A` task 2), covering
//! the success path (RUNNING -> SUCCESS, usage stamped, output round-trips),
//! the failure path (FAILED + error, walk halted), and full-context
//! serde round-tripping.
//!
//! The `#[ignore]` live test at the bottom needs a logged-in Claude Code
//! subscription and is run manually via `cargo test -- --ignored` — it is the
//! block's real-session acceptance check, not part of the gated suite.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{
    CancellationToken, ClaudeCodeStep, Node, NodeConfig, NodeRegistry, RunOptions, Workflow,
    WorkflowSchema, CANCELLATION_METADATA_KEY,
};

const NODE_NAME: &str = "ClaudeCodeStep";

fn stub_outcome() -> Outcome {
    Outcome {
        cost_usd: 0.02,
        usage: SdkUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        // The CLI names the model only as a `modelUsage` key — there is no
        // top-level `model` field to stub.
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            SdkModelUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.02,
            },
        )]
        .into_iter()
        .collect(),
        structured_output: None,
        text: "ok".to_string(),
        is_error: false,
        api_error_status: None,
    }
}

fn single_node_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(NODE_NAME.to_string(), NodeConfig::new(NODE_NAME, vec![]));
    WorkflowSchema::new("claude-code-step-only", NODE_NAME, nodes)
}

#[tokio::test]
async fn workflow_run_stamps_success_usage_and_round_trips_context() {
    let step = ClaudeCodeStep::new(NODE_NAME, Config::default(), "do the thing")
        .with_transport(|_config, _prompt| Box::pin(async { Ok(stub_outcome()) }));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(step));

    let workflow = Workflow::new(registry, single_node_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({ "ticket_id": "T-1" }), on_progress)
        .await
        .expect("workflow should complete successfully");

    let run = result
        .node_runs
        .get(NODE_NAME)
        .expect("node_runs entry present");
    assert_eq!(run.status, NodeRunStatus::Success);
    assert!(run.started_at.is_some());
    assert!(run.completed_at.is_some());
    assert!(run.error.is_none());

    let usage = run.usage.as_ref().expect("usage should be stamped");
    assert_eq!(usage.input_tokens, Some(100));
    assert_eq!(usage.output_tokens, Some(50));
    assert_eq!(usage.model, "claude-sonnet-4-5");

    let output = result
        .nodes
        .get(NODE_NAME)
        .expect("node output should be present");
    assert_eq!(output["content"], "ok");
    assert_eq!(output["model"], "claude-sonnet-4-5");

    // The full TaskContext (including the node's output) round-trips through
    // serde_json without error — the data-contract shape holds.
    let json = serde_json::to_value(&result).expect("serialize should succeed");
    let round_tripped: TaskContext =
        serde_json::from_value(json).expect("deserialize should succeed");
    assert_eq!(round_tripped, result);
}

#[tokio::test]
async fn workflow_run_maps_sdk_error_to_failed_status_and_halts() {
    let step = ClaudeCodeStep::new(NODE_NAME, Config::default(), "do the thing")
        .with_transport(|_config, _prompt| Box::pin(async { Err(claude_code_rs::Error::Timeout) }));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(step));

    let workflow = Workflow::new(registry, single_node_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("run should return Ok with the accumulated context even on node failure");

    let run = result
        .node_runs
        .get(NODE_NAME)
        .expect("node_runs entry present");
    assert_eq!(run.status, NodeRunStatus::Failed);
    assert_eq!(
        run.error.as_deref(),
        Some(claude_code_rs::Error::Timeout.to_string().as_str())
    );
    assert!(run.usage.is_none());

    // The walk halted: no node output was ever written.
    assert!(!result.nodes.contains_key(NODE_NAME));
}

/// Sets a shared flag when dropped — lets a stub transport future prove it
/// was actually dropped (not awaited to completion) by a cancellation win.
struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Builds a `ClaudeCodeStep::with_transport` stub whose future never
/// resolves on its own (`std::future::pending`) but reports via `dropped`
/// once it is dropped — i.e. once something raced it away rather than
/// awaiting it out.
fn never_resolving_transport_step(
    dropped: Arc<AtomicBool>,
) -> impl Fn(Config, String) -> futures::future::BoxFuture<'static, claude_code_rs::Result<Outcome>>
{
    move |_config, _prompt| {
        let signal = DropSignal(dropped.clone());
        Box::pin(async move {
            let _signal = signal; // held for the future's lifetime; Drop fires on cancel
            std::future::pending::<()>().await;
            unreachable!("this transport future must be cancelled before it ever resolves")
        })
    }
}

/// `ClaudeCodeStep::process` called directly with a `CancellationToken`
/// (task 4): a cancel that lands mid-await must return promptly with `Ok`,
/// and the stub transport future must have actually been dropped rather than
/// polled to completion.
#[tokio::test]
async fn process_cancels_promptly_and_drops_in_flight_transport_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let token = CancellationToken::new();

    let step = ClaudeCodeStep::new(NODE_NAME, Config::default(), "do the thing")
        .with_cancellation_token(token.clone())
        .with_transport(never_resolving_transport_step(dropped.clone()));

    let ctx = TaskContext {
        event: serde_json::json!({}),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    };

    let process_fut = step.process(ctx);
    tokio::pin!(process_fut);

    // Race the cancel against the in-flight (never-resolving) transport
    // future: give `process` a moment to actually start awaiting before
    // triggering the cancel, so this exercises the real select rather than
    // an already-cancelled fast path.
    tokio::select! {
        _ = &mut process_fut => panic!("process resolved before cancellation was ever triggered"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {
            token.cancel();
        }
    }

    let out = tokio::time::timeout(Duration::from_secs(1), process_fut)
        .await
        .expect("a cancelled process() must return promptly, not hang")
        .expect("a cancellation win must return Ok, not a NodeError");

    assert!(
        dropped.load(Ordering::SeqCst),
        "the in-flight transport future should have been dropped on cancel"
    );
    assert!(
        !out.nodes.contains_key(NODE_NAME),
        "a cancelled node must not stamp any output"
    );
}

/// A trivial second node — its only role in the cancellation test below is
/// to give the run loop a next boundary to re-check the cancellation token
/// at, after the cancelled `ClaudeCodeStep` returns.
struct NoopNode;

#[async_trait::async_trait]
impl Node for NoopNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, engine_core::NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "NoopNode"
    }
}

/// Full `Workflow::run_with` integration (task 3 + task 4 together): a
/// cancel that lands mid-flight inside `ClaudeCodeStep` must not be recorded
/// as a FAILED `NodeRun`, and the run loop's own next-boundary check (task 3)
/// is what stamps the cancelled terminal marker and leaves the downstream
/// node PENDING.
#[tokio::test]
async fn workflow_run_with_mid_flight_cancel_does_not_mark_the_node_failed() {
    let dropped = Arc::new(AtomicBool::new(false));
    let token = CancellationToken::new();

    let step = ClaudeCodeStep::new(NODE_NAME, Config::default(), "do the thing")
        .with_cancellation_token(token.clone())
        .with_transport(never_resolving_transport_step(dropped.clone()));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(step));
    registry.register(Box::new(NoopNode));

    let mut nodes = HashMap::new();
    nodes.insert(
        NODE_NAME.to_string(),
        NodeConfig::new(NODE_NAME, vec!["NoopNode".to_string()]),
    );
    nodes.insert("NoopNode".to_string(), NodeConfig::new("NoopNode", vec![]));
    let schema = WorkflowSchema::new("claude-code-step-then-noop", NODE_NAME, nodes);

    let workflow = Workflow::new(registry, schema);

    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});
    let run_fut = workflow.run_with(
        serde_json::json!({}),
        on_progress,
        RunOptions {
            cancellation_token: Some(token.clone()),
            budget: None,
            pause_signal: None,
            run_id: None,
        },
    );
    tokio::pin!(run_fut);

    tokio::select! {
        _ = &mut run_fut => panic!("run resolved before cancellation was ever triggered"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {
            token.cancel();
        }
    }

    let result = tokio::time::timeout(Duration::from_secs(1), run_fut)
        .await
        .expect("a cancelled run_with() must return promptly, not hang")
        .expect("run_with must return Ok even when cancelled");

    assert!(
        dropped.load(Ordering::SeqCst),
        "the in-flight transport future should have been dropped on cancel"
    );

    let run = result
        .node_runs
        .get(NODE_NAME)
        .expect("node_runs entry present");
    assert_ne!(
        run.status,
        NodeRunStatus::Failed,
        "a mid-flight cancel must not be recorded as FAILED"
    );

    assert_eq!(
        result.metadata[CANCELLATION_METADATA_KEY]["cancelled"],
        serde_json::json!(true)
    );
    assert_eq!(
        result.node_runs.get("NoopNode").unwrap().status,
        NodeRunStatus::Pending,
        "the run loop must halt before dispatching the node after a cancel"
    );
}

/// Live smoke test — actually spawns a `claude` session on the logged-in
/// subscription with a trivial prompt via the default transport. Ignored so
/// the gated `cargo test` suite stays hermetic; run manually with
/// `cargo test -- --ignored` when the `claude` CLI is available and
/// authenticated.
#[tokio::test]
#[ignore]
async fn live_claude_code_step_produces_populated_usage() {
    let step = ClaudeCodeStep::new(
        NODE_NAME,
        Config::default(),
        "Reply with the single word: ok",
    );

    let ctx = TaskContext {
        event: serde_json::json!({}),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    };

    let out = step
        .process(ctx)
        .await
        .expect("live process call should succeed");

    let run = out
        .node_runs
        .get(NODE_NAME)
        .expect("node_runs entry present");
    let usage = run.usage.as_ref().expect("usage should be populated");
    assert!(usage.input_tokens.is_some());
    assert!(usage.output_tokens.is_some());
    assert!(!usage.model.is_empty());

    let output = out.nodes.get(NODE_NAME).expect("output present");
    // The output must be JSON-serializable (it already is a serde_json::Value
    // by construction) and re-serialize without error.
    serde_json::to_string(output).expect("output should serialize");
}
