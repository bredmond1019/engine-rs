//! EN.5.F task 5: the block's acceptance-criteria proof, driven through the
//! real in-process actix harness (mirrors `abort_integration.rs`'s and
//! `dispatch_integration.rs`'s style).
//!
//! Covers, one test per bullet in the block's Acceptance Criteria:
//! - fast `202` well under the sleeping first node's duration;
//! - `GET /events/{event_id}` readback: running mid-run, terminal after;
//! - `GET /events/{event_id}/stream` delivers one frame per node transition
//!   plus a terminal frame;
//! - aborting a spawned run still stamps a cancelled terminal readback;
//! - a run exceeding the default budget halts with the budget marker.
//!
//! Hermetic: no real `claude` subprocess, no network, no `DATABASE_URL` — every
//! fixture node is a plain in-memory `Node` impl and `AppState::durable` is
//! built with `spawn_durable_writer(None)` (durable writes self-skip with no
//! pool configured).

use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{test, web, App};
use engine_contract::TaskContext;
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};
use engine_serve::abort::RunRegistry;
use engine_serve::dispatch::Dispatcher;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;
use tokio::sync::Notify;
use uuid::Uuid;

const API_KEY: &str = "async-lifecycle-test-key";

/// A node that sleeps `duration` before completing — used to prove the HTTP
/// response returns long before the run finishes.
struct SleepNode {
    duration: Duration,
}

#[async_trait::async_trait]
impl Node for SleepNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        tokio::time::sleep(self.duration).await;
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SleepNode"
    }
}

/// A node that blocks in `process` until `release` is notified — gives the
/// test a deterministic window to observe the run mid-flight before letting
/// it proceed. Mirrors `abort_integration.rs`'s and `http.rs`'s `WaitNode`.
struct WaitNode {
    identity: &'static str,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Node for WaitNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.release.notified().await;
        ctx.nodes.insert(
            self.identity.to_string(),
            serde_json::json!({ "ran": true }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// A node that reports a `cost_usd` above the default HTTP budget
/// (`$5.0`, `http.rs::DEFAULT_MAX_COST_USD`) in its own output, so the
/// pre-dispatch budget gate halts the walk before the next node — without
/// needing to touch `ENGINE_RUN_MAX_COST_USD` (which is read once and
/// memoized for the whole test binary via a `OnceLock`, making per-test env
/// overrides unsafe to rely on under parallel `cargo test` execution).
struct ExpensiveNode;

#[async_trait::async_trait]
impl Node for ExpensiveNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "ran": true, "cost_usd": 10.0 }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "ExpensiveNode"
    }
}

/// A node that must never run once `ExpensiveNode`'s cost trips the budget
/// gate — if this node's marker shows up in the readback, the halt did not
/// actually stop the walk.
struct NeverNode;

#[async_trait::async_trait]
impl Node for NeverNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "NeverNode"
    }
}

fn single_node_schema(workflow_type: &str, node_identity: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        node_identity.to_string(),
        NodeConfig::new(node_identity, vec![]),
    );
    WorkflowSchema::new(workflow_type, node_identity, nodes)
}

fn two_node_schema(workflow_type: &str, first: &str, second: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        first.to_string(),
        NodeConfig::new(first, vec![second.to_string()]),
    );
    nodes.insert(second.to_string(), NodeConfig::new(second, vec![]));
    WorkflowSchema::new(workflow_type, first, nodes)
}

fn app_state_with(dispatcher: Dispatcher) -> AppState {
    AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        api_key: API_KEY.to_string(),
    }
}

/// Fetch `GET /events/{event_id}` against `app` and return `(status, body)`.
/// A macro rather than a free function: `test::init_service`'s return type is
/// an opaque `impl Service<..>` that differs per call site (each test builds
/// its own `App`), so a shared helper can't name a single concrete or
/// trait-object parameter type without pulling in `actix_http` as an
/// explicit dependency just for the `Request` type.
macro_rules! get_event {
    ($app:expr, $event_id:expr) => {{
        let req = test::TestRequest::get()
            .uri(&format!("/events/{}", $event_id))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&$app, req).await;
        let status = resp.status().as_u16();
        let body: serde_json::Value = test::read_body_json(resp).await;
        (status, body)
    }};
}

/// **Fast 202.** `POST /events/` returns `202 {run_id, event_id}` (both
/// present and equal) in well under ~100ms against a workflow whose first
/// (only) node sleeps for 500ms — the response cannot be waiting on the run.
#[actix_web::test]
async fn post_events_returns_202_fast_against_a_slow_workflow() {
    const WORKFLOW_TYPE: &str = "async-lifecycle-sleep";

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        single_node_schema(WORKFLOW_TYPE, "SleepNode"),
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(SleepNode {
                duration: Duration::from_millis(500),
            }));
            Ok(Workflow::new(
                registry,
                single_node_schema(WORKFLOW_TYPE, "SleepNode"),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();

    let started = Instant::now();
    let resp = test::call_service(&app, req).await;
    let elapsed = started.elapsed();

    assert_eq!(resp.status(), 202);
    assert!(
        elapsed < Duration::from_millis(100),
        "expected the trigger response well under 100ms (node sleeps 500ms), got {elapsed:?}"
    );

    let body: serde_json::Value = test::read_body_json(resp).await;
    let run_id = body["run_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("run_id should be a parseable UUID");
    let event_id = body["event_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("event_id should be a parseable UUID");
    assert_eq!(run_id, event_id, "event_id must equal run_id");
}

/// **Readback through terminal status.** `GET /events/{event_id}` returns a
/// running status while the fixture's `WaitNode` blocks, then a terminal
/// (`succeeded`) status for the same id once released — carrying the full
/// canonical field set on both reads.
#[actix_web::test]
async fn readback_transitions_from_running_to_terminal_for_the_same_run() {
    const WORKFLOW_TYPE: &str = "async-lifecycle-readback";

    let release = Arc::new(Notify::new());
    let release_for_factory = release.clone();
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        single_node_schema(WORKFLOW_TYPE, "WaitNode"),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode {
                identity: "WaitNode",
                release: release_for_factory.clone(),
            }));
            Ok(Workflow::new(
                registry,
                single_node_schema(WORKFLOW_TYPE, "WaitNode"),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let trigger_req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger_req).await;
    assert_eq!(trigger_resp.status(), 202);
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let event_id = trigger_body["event_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("event_id should be a parseable UUID");

    // Poll until the readback observes the running state — the spawned
    // task's first `on_progress` call may not have landed yet right after
    // the trigger response comes back.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get_event!(app, event_id);
        if status == 200 && body["status"] == "running" {
            assert_eq!(body["event_id"], event_id.to_string());
            assert_eq!(body["workflow_type"], WORKFLOW_TYPE);
            assert!(body.get("created_at").is_some());
            assert!(body.get("updated_at").is_some());
            assert!(body.get("task_context").is_some());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "run never reached a running status readback"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    release.notify_one();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get_event!(app, event_id);
        if status == 200 && body["status"] == "succeeded" {
            assert_eq!(body["event_id"], event_id.to_string());
            assert_eq!(body["workflow_type"], WORKFLOW_TYPE);
            assert!(body.get("created_at").is_some());
            assert!(body.get("updated_at").is_some());
            assert!(body.get("task_context").is_some());
            return;
        }
        assert!(
            Instant::now() < deadline,
            "run never reached a terminal status readback"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// **One SSE frame per node transition.** A two-node `WaitNode` workflow
/// blocks each node on its own `Notify`, giving the test a deterministic
/// window to connect the SSE stream before releasing either node. Once
/// connected (the endpoint's `subscribe()` call is synchronous, so a `200`
/// response guarantees the subscription is already registered), every frame
/// published from that point on is guaranteed delivered: node A's success
/// transition, node B's running transition, node B's success transition, and
/// the terminal frame — four frames, strictly increasing in information and
/// ending on `"terminal":true`.
#[actix_web::test]
async fn stream_delivers_one_frame_per_node_transition_then_a_terminal_frame() {
    const WORKFLOW_TYPE: &str = "async-lifecycle-stream";

    let release_a = Arc::new(Notify::new());
    let release_b = Arc::new(Notify::new());
    let release_a_for_factory = release_a.clone();
    let release_b_for_factory = release_b.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_node_schema(WORKFLOW_TYPE, "NodeA", "NodeB"),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode {
                identity: "NodeA",
                release: release_a_for_factory.clone(),
            }));
            registry.register(Box::new(WaitNode {
                identity: "NodeB",
                release: release_b_for_factory.clone(),
            }));
            Ok(Workflow::new(
                registry,
                two_node_schema(WORKFLOW_TYPE, "NodeA", "NodeB"),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let trigger_req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger_req).await;
    assert_eq!(trigger_resp.status(), 202);
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let event_id = trigger_body["event_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("event_id should be a parseable UUID");

    // Connect once the run is known to the live store (node A is still
    // blocked on `release_a` at this point, so the stream connect happens
    // strictly before node A's success transition — everything published
    // after this point is guaranteed captured).
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream_resp = loop {
        let stream_req = test::TestRequest::get()
            .uri(&format!("/events/{event_id}/stream"))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, stream_req).await;
        if resp.status() == 200 {
            break resp;
        }
        assert!(
            Instant::now() < deadline,
            "stream endpoint never connected for the live run"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(
        stream_resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    // Release both nodes concurrently with draining the stream body — the
    // body future only resolves once the terminal frame is published and
    // the sender dropped.
    let release_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        release_a.notify_one();
        tokio::time::sleep(Duration::from_millis(20)).await;
        release_b.notify_one();
    });
    let body_bytes = test::read_body(stream_resp).await;
    release_handle.await.expect("release task should not panic");

    let body = String::from_utf8(body_bytes.to_vec()).expect("SSE body should be UTF-8");
    let frames: Vec<serde_json::Value> = body
        .split("\n\n")
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let payload = line
                .strip_prefix("data: ")
                .expect("every SSE frame should carry a data: prefix");
            serde_json::from_str(payload).expect("frame payload should be valid JSON")
        })
        .collect();

    // Guaranteed-captured frames, in order: NodeA success, NodeB running,
    // NodeB success, terminal.
    assert!(
        frames.len() >= 4,
        "expected at least 4 frames (NodeA success, NodeB running, NodeB success, terminal), got {}: {frames:?}",
        frames.len()
    );

    let terminal_frames: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|f| f["terminal"].as_bool() == Some(true))
        .collect();
    assert_eq!(
        terminal_frames.len(),
        1,
        "expected exactly one terminal frame, got {terminal_frames:?}"
    );
    assert_eq!(terminal_frames[0]["status"], "succeeded");
    assert_eq!(
        frames.last().unwrap()["terminal"].as_bool(),
        Some(true),
        "the terminal frame must be the last frame delivered"
    );

    // Every non-terminal frame is `"running"`, and the informational content
    // is strictly increasing: eventually a frame shows NodeA succeeded and
    // NodeB running, before the terminal frame shows both succeeded.
    let non_terminal: Vec<&serde_json::Value> = frames
        .iter()
        .filter(|f| f["terminal"].as_bool() == Some(false))
        .collect();
    assert!(
        non_terminal.iter().all(|f| f["status"] == "running"),
        "every non-terminal frame should carry status \"running\": {non_terminal:?}"
    );
    assert!(
        non_terminal.iter().any(|f| {
            f["task_context"]["node_runs"]["NodeB"]["status"] == "Running"
                || f["task_context"]["node_runs"]["NodeB"]["status"] == "running"
        }),
        "expected a frame showing NodeB's running transition: {non_terminal:?}"
    );
}

/// **Abort of a spawned run still stamps terminal state.** Trigger, abort
/// mid-run via `POST /events/{run_id}/abort`, then assert the readback shows
/// the cancelled terminal status with the cancelled marker in `metadata`.
#[actix_web::test]
async fn aborting_a_spawned_run_reads_back_cancelled_with_the_marker_in_metadata() {
    const WORKFLOW_TYPE: &str = "async-lifecycle-abort";

    let release = Arc::new(Notify::new());
    let release_for_factory = release.clone();
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_node_schema(WORKFLOW_TYPE, "WaitNode", "SuccessNode"),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode {
                identity: "WaitNode",
                release: release_for_factory.clone(),
            }));
            registry.register(Box::new(WaitNode {
                identity: "SuccessNode",
                release: Arc::new(Notify::new()), // never awaited — this node should never run
            }));
            Ok(Workflow::new(
                registry,
                two_node_schema(WORKFLOW_TYPE, "WaitNode", "SuccessNode"),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let runs = state.runs.clone();
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let trigger_req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger_req).await;
    assert_eq!(trigger_resp.status(), 202);
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let run_id = trigger_body["run_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("run_id should be a parseable UUID");

    // Wait for the run to be running before aborting it.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get_event!(app, run_id);
        if status == 200 && body["status"] == "running" {
            break;
        }
        assert!(Instant::now() < deadline, "run never reached running");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let abort_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/abort"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let abort_resp = test::call_service(&app, abort_req).await;
    assert_eq!(abort_resp.status(), 202);

    // Let `WaitNode` observe the cancellation and finish; the run halts at
    // the next node boundary, before `SuccessNode` dispatches.
    release.notify_one();

    let deadline = Instant::now() + Duration::from_secs(5);
    while runs.get(run_id).is_some() {
        assert!(
            Instant::now() < deadline,
            "spawned run did not finish cleanup within 5s"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let (status, body) = get_event!(app, run_id);
    assert_eq!(status, 200);
    assert_eq!(body["status"], "cancelled");
    assert_eq!(
        body["task_context"]["metadata"]["cancellation"]["cancelled"],
        serde_json::json!(true)
    );
}

/// **Budget halt.** A run whose first node reports a cost above the default
/// HTTP budget (`$5.0`) halts before dispatching the next node, and reads
/// back terminal with the budget marker in `metadata`.
#[actix_web::test]
async fn a_run_exceeding_the_default_budget_halts_with_the_budget_marker() {
    const WORKFLOW_TYPE: &str = "async-lifecycle-budget";

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_node_schema(WORKFLOW_TYPE, "ExpensiveNode", "NeverNode"),
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(ExpensiveNode));
            registry.register(Box::new(NeverNode));
            Ok(Workflow::new(
                registry,
                two_node_schema(WORKFLOW_TYPE, "ExpensiveNode", "NeverNode"),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let trigger_req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();
    let trigger_resp = test::call_service(&app, trigger_req).await;
    assert_eq!(trigger_resp.status(), 202);
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let event_id = trigger_body["event_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("event_id should be a parseable UUID");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get_event!(app, event_id);
        if status == 200 && body["status"] != "running" {
            assert_eq!(body["status"], "budget_halted");
            assert_eq!(
                body["task_context"]["metadata"]["budget"]["halted"],
                serde_json::json!(true)
            );
            assert!(
                body["task_context"]["node_runs"]["NeverNode"]["status"] == "Pending"
                    || body["task_context"]["node_runs"]["NeverNode"]["status"] == "pending",
                "NeverNode must never have been dispatched once the budget gate tripped: {body}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "run never reached a terminal status readback"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
