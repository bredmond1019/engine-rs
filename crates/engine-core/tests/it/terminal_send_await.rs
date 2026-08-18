//! Integration suite over the `EN.9.E` send/await pair, driven through a real
//! `Workflow::run_with` walk rather than a direct `Node::process()` call —
//! the seams task 2/task 3's own unit tests cannot exercise on their own:
//! stale-marker rejection end-to-end through `TerminalAwaitNode`, `send_id`
//! back-edge idempotency across two separate workflow runs, and a
//! `CancellationToken`-driven abort returning within the 5s bound.
//!
//! `StubTerminalDriver` throughout — this block adds no real-environment
//! requirement of its own; `EN.9.D` already owns the real-Mini exercise.
//!
//! Every workflow here is `SeedSessionNode -> {TerminalSendNode |
//! TerminalAwaitNode}`: `SeedSessionNode` stands in for a prior
//! `TerminalSessionNode` run by stamping the same `ctx.nodes` shape
//! (`session_name`/`lease_nonce`) the send/await nodes read, following the
//! precedent `send.rs`'s/`await_node.rs`'s own unit tests already
//! establish for a fixture upstream session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use engine_contract::TaskContext;
use engine_core::nodes::terminal::{predicate, session, TerminalAwaitNode, TerminalSendNode};
use engine_core::{
    CancellationToken, Node, NodeConfig, NodeError, NodeRegistry, RunOptions, Workflow,
    WorkflowSchema,
};
use term_core::driver::{StubOutcome, StubTerminalDriver};
use uuid::Uuid;

/// Stands in for a prior `TerminalSessionNode` run: stamps the
/// `session_name`/`lease_nonce` shape `TerminalSendNode`/`TerminalAwaitNode`
/// read off `ctx.nodes[session::NODE_NAME]`.
struct SeedSessionNode {
    session_name: String,
    lease_nonce: String,
}

#[async_trait::async_trait]
impl Node for SeedSessionNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            session::NODE_NAME.to_string(),
            serde_json::json!({
                "session_name": self.session_name,
                "lease_nonce": self.lease_nonce,
                "created": true,
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        session::NODE_NAME
    }
}

/// A two-node linear schema: `session::NODE_NAME -> second`, wired via
/// `connections[0]` only — mirrors `cancellation.rs`'s `linear_schema`
/// fixture shape.
fn seed_then_schema(second: &str) -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        session::NODE_NAME.to_string(),
        NodeConfig::new(session::NODE_NAME, vec![second.to_string()]),
    );
    nodes.insert(second.to_string(), NodeConfig::new(second, vec![]));
    WorkflowSchema::new("seed-then", session::NODE_NAME, nodes)
}

fn seed_live_lease(driver: &StubTerminalDriver, session_name: &str, nonce: &str) {
    let far_future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + Duration::from_secs(3600).as_millis() as u64;
    driver.set_show_option_result_for(
        format!("@engine_lease@{session_name}"),
        StubOutcome::Ok(format!("some-run:{nonce}:TerminalSessionNode:{far_future}")),
    );
}

fn send_keys_calls(driver: &StubTerminalDriver) -> usize {
    driver
        .calls()
        .iter()
        .filter(|c| c.get(1).map(String::as_str) == Some("send-keys"))
        .count()
}

// ── Stale-marker rejection, end to end through TerminalAwaitNode ─────────

#[tokio::test]
async fn stale_marker_rejection_end_to_end_through_a_real_workflow_walk() {
    let dir = std::env::temp_dir().join(format!(
        "engine-rs-terminal-send-await-it-stale-{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.log");
    let nonce = "nonce-stale";
    let marker_path = predicate::marker_path(out.to_str().unwrap(), nonce);
    // Written BEFORE the `sent_at` used below — a marker surviving from a
    // previous run, exactly the case the four-part marker contract must
    // reject.
    std::fs::write(&marker_path, nonce).unwrap();

    let driver = Arc::new(StubTerminalDriver::new());
    driver.set_capture_pane_result(StubOutcome::Ok("some output".to_string()));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SeedSessionNode {
        session_name: "eng-run1_TerminalSessionNode".to_string(),
        lease_nonce: "nonce-1".to_string(),
    }));
    registry.register(Box::new(TerminalAwaitNode::new(driver.clone())));

    let workflow = Workflow::new(registry, seed_then_schema("TerminalAwaitNode"));

    // sent_at in the FUTURE relative to the marker's mtime, guaranteeing
    // staleness regardless of filesystem timestamp resolution.
    let sent_at = chrono::Utc::now() + chrono::Duration::seconds(60);
    let event = serde_json::json!({
        "predicate": {
            "type": "marker",
            "out": out.to_str().unwrap(),
            "nonce": nonce,
        },
        "sent_at": sent_at.to_rfc3339(),
        "policy": { "poll_interval_ms": 10, "timeout_ms": 50 },
    });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        workflow.run_with(
            event,
            Box::new(|_c: &TaskContext| {}),
            RunOptions::default(),
        ),
    )
    .await
    .expect("expected the await node to time out well within 5 seconds")
    .expect("a stale marker times out, it does not error the run");

    let stored = result
        .nodes
        .get("TerminalAwaitNode")
        .expect("TerminalAwaitNode stamped a result");
    assert_eq!(
        stored["satisfied"],
        serde_json::json!(false),
        "a stale marker must not satisfy a fresh await"
    );
    assert_eq!(stored["timed_out"], serde_json::json!(true));

    std::fs::remove_dir_all(&dir).ok();
}

// ── send_id back-edge idempotency, driven through two workflow runs ──────

#[tokio::test]
async fn send_id_back_edge_idempotency_through_a_real_workflow_walk() {
    let driver = Arc::new(StubTerminalDriver::new());
    let session_name = "eng-run1_TerminalSessionNode";
    seed_live_lease(&driver, session_name, "nonce-1");

    let build_workflow = |driver: Arc<StubTerminalDriver>| {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SeedSessionNode {
            session_name: session_name.to_string(),
            lease_nonce: "nonce-1".to_string(),
        }));
        registry.register(Box::new(TerminalSendNode::new(driver)));
        Workflow::new(registry, seed_then_schema("TerminalSendNode"))
    };

    let run_id = Uuid::new_v4();
    let event = serde_json::json!({
        "command": "cargo build",
        "send_id": "send-1",
    });

    // First run: a fresh send, driven through the workflow's own dispatch
    // loop rather than a direct `TerminalSendNode::process()` call.
    let first_workflow = build_workflow(driver.clone());
    let first = first_workflow
        .run_with(
            event.clone(),
            Box::new(|_c: &TaskContext| {}),
            RunOptions {
                run_id: Some(run_id),
                ..RunOptions::default()
            },
        )
        .await
        .expect("first send should succeed");
    let first_stored = first.nodes.get("TerminalSendNode").unwrap();
    assert_eq!(first_stored["sent"], serde_json::json!(true));
    let calls_after_first = send_keys_calls(&driver);
    assert!(
        calls_after_first > 0,
        "the first send must issue at least one driver call"
    );

    // `StubTerminalDriver::show_option` returns one configured value per
    // name regardless of what `set_option` just wrote (it does not model
    // tmux's real storage) — seed the read-back a real tmux would now show
    // for the send_id option the first run just recorded, so the second
    // run's idempotency check observes what production would.
    driver.set_show_option_result_for(
        format!("@engine_last_send_id@{session_name}"),
        StubOutcome::Ok("send-1".to_string()),
    );

    // Second run: a BACK-EDGE re-entry — a wholly fresh `Workflow`/`ctx`,
    // same `send_id`, driven through its own full workflow walk.
    let second_workflow = build_workflow(driver.clone());
    let second = second_workflow
        .run_with(
            event,
            Box::new(|_c: &TaskContext| {}),
            RunOptions {
                run_id: Some(run_id),
                ..RunOptions::default()
            },
        )
        .await
        .expect("a repeated send_id is a no-op success, not an error");

    let second_stored = second.nodes.get("TerminalSendNode").unwrap();
    assert_eq!(second_stored["sent"], serde_json::json!(false));
    assert_eq!(second_stored["deduplicated"], serde_json::json!(true));
    assert_eq!(
        send_keys_calls(&driver),
        calls_after_first,
        "a repeated send_id must issue zero additional driver send-keys calls"
    );
}

// ── Cancellation-driven abort returns within 5s of a 10-minute bound ─────

#[tokio::test]
async fn cancellation_driven_abort_returns_within_five_seconds_through_a_real_workflow_walk() {
    let driver = Arc::new(StubTerminalDriver::new());
    driver.set_capture_pane_result(StubOutcome::Ok("never satisfies".to_string()));
    let token = CancellationToken::new();

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SeedSessionNode {
        session_name: "eng-run1_TerminalSessionNode".to_string(),
        lease_nonce: "nonce-1".to_string(),
    }));
    registry.register(Box::new(
        TerminalAwaitNode::new(driver.clone()).with_cancellation_token(token.clone()),
    ));

    let workflow = Workflow::new(registry, seed_then_schema("TerminalAwaitNode"));

    let event = serde_json::json!({
        "predicate": { "type": "regex", "pattern": "NEVER MATCHES" },
        // A real 10-minute bound — cancellation must win long before this
        // fires.
        "policy": { "poll_interval_ms": 20, "timeout_ms": 600_000 },
    });

    let run_options = RunOptions {
        cancellation_token: Some(token.clone()),
        ..RunOptions::default()
    };

    // The cancel-trigger runs on its own spawned task (Send-friendly: it
    // only sleeps and flips the token); `run_with` itself is awaited
    // directly in this test's own future rather than spawned, since its
    // `OnProgress<'_>` closure is not `Send` and `tokio::spawn` requires
    // `Send + 'static`.
    let cancel_token = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        workflow.run_with(event, Box::new(|_c: &TaskContext| {}), run_options),
    )
    .await
    .expect("expected the run to return within 5 seconds of cancellation")
    .expect("a cancelled run returns Ok, not Err");

    // Cancel wins inside `TerminalAwaitNode`'s own poll loop: no result is
    // stamped for its identity, matching the module's documented
    // convention (`await_node.rs`'s own cancellation unit test asserts the
    // same thing at the direct-node-call level; this asserts it survives a
    // real workflow dispatch).
    assert!(!result.nodes.contains_key("TerminalAwaitNode"));
}
