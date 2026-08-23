//! EN.2.B task 5 integration test: an in-process actix harness triggers a
//! fixture workflow through `POST /events/`, aborts it mid-run through
//! `POST /events/{run_id}/abort`, and asserts the 401 / 404 / success paths
//! plus the live-state snapshot for that run reflecting the cancelled
//! terminal state stamped by `Workflow::run_with` (task 3).
//!
//! EN.5.F: the trigger is now non-blocking — `POST /events/` spawns the run
//! and returns before it finishes — so the trigger response is no longer a
//! valid "the run is done" signal. The full round-trip test waits on
//! `RunRegistry` deregistration (the spawned task's last action) instead.

use std::collections::HashMap as StdHashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{test, web, App};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};
use engine_serve::abort::RunRegistry;
use engine_serve::dispatch::Dispatcher;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;
use engine_serve::test_fixtures::WaitNode;
use tokio::sync::Notify;

const FIXTURE_WORKFLOW_TYPE: &str = "abort-fixture";
const API_KEY: &str = "abort-test-key";

/// Second node in the fixture graph. A cancellation triggered while
/// `WaitNode` is running is only observed at the next node boundary (task 3
/// semantics), so this node should stay `Pending` once the run is aborted.
struct SuccessNode;

#[async_trait::async_trait]
impl Node for SuccessNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SuccessNode"
    }
}

fn fixture_schema() -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "WaitNode".to_string(),
        NodeConfig::new("WaitNode", vec!["SuccessNode".to_string()]),
    );
    nodes.insert(
        "SuccessNode".to_string(),
        NodeConfig::new("SuccessNode", vec![]),
    );
    WorkflowSchema::new(FIXTURE_WORKFLOW_TYPE, "WaitNode", nodes)
}

fn test_app_state(release: Arc<Notify>) -> AppState {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        fixture_schema(),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode::new(release.clone())));
            registry.register(Box::new(SuccessNode));
            Ok(Workflow::new(registry, fixture_schema()))
        }),
    );

    AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        campaigns: engine_serve::abort::CampaignRegistry::new(),
        api_key: API_KEY.to_string(),
    }
}

#[actix_web::test]
async fn abort_without_api_key_is_rejected() {
    let state = test_app_state(Arc::new(Notify::new()));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/abort", uuid::Uuid::new_v4()))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn abort_unknown_run_id_returns_404() {
    let state = test_app_state(Arc::new(Notify::new()));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/abort", uuid::Uuid::new_v4()))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 404);
}

/// Full round trip: trigger the fixture workflow, abort it mid-run, assert
/// the abort call succeeds and the run's live-state snapshot reflects the
/// cancelled terminal state (D6 `metadata` shape) rather than a failure, with
/// `SuccessNode` left `Pending` since the halt happens at the node boundary
/// before it is dispatched.
#[actix_web::test]
async fn aborting_a_live_run_stamps_cancelled_and_returns_success() {
    let release = Arc::new(Notify::new());
    let state = test_app_state(release.clone());
    let live = state.live.clone();
    let runs = state.runs.clone();
    let app = Rc::new(
        test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await,
    );

    // `actix_web::rt::spawn` (not `tokio::spawn`) runs on the same
    // single-threaded local set as this `#[actix_web::test]`, so a `!Send`
    // future sharing the (`Rc`-wrapped, non-`Send`) test service is fine —
    // this is what lets the trigger and the abort race concurrently.
    let trigger_app = app.clone();
    let trigger_handle = actix_web::rt::spawn(async move {
        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": FIXTURE_WORKFLOW_TYPE,
                "data": {},
            }))
            .to_request();
        test::call_service(&*trigger_app, req).await
    });

    // Poll the live-state store until the freshly-minted run_id shows up —
    // `WaitNode`'s RUNNING transition is recorded via `on_progress` before
    // its `process()` call blocks on `release`, so this window is real but
    // not instantaneous.
    let run_id = loop {
        let active = live.list_active();
        if let Some(run_id) = active.into_iter().next() {
            break run_id;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    let abort_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/abort"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let abort_resp = test::call_service(&*app, abort_req).await;
    assert_eq!(abort_resp.status(), 202);

    // Let `WaitNode` finish now that the token is triggered; the run loop
    // observes the cancellation at the next boundary, before dispatching
    // `SuccessNode`.
    release.notify_one();

    let trigger_resp = trigger_handle.await.expect("trigger task panicked");
    assert_eq!(trigger_resp.status(), 202);
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    assert_eq!(trigger_body["run_id"], serde_json::json!(run_id));
    assert_eq!(trigger_body["event_id"], serde_json::json!(run_id));

    // The trigger now returns before the run finishes, so wait for the
    // spawned task's cleanup instead of treating the 202 as the finish
    // line. Deregistration is the last thing that task does (after
    // `mark_terminal`), so observing it means the final snapshot and the
    // terminal marking are both already visible.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while runs.get(run_id).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "spawned run did not finish cleanup within 5s"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let snapshot = live
        .get(run_id)
        .expect("live_state should hold the aborted run's snapshot");

    assert_eq!(
        snapshot.node_runs["WaitNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        snapshot.node_runs["SuccessNode"].status,
        NodeRunStatus::Pending,
        "SuccessNode was never dispatched — the halt happens at the node boundary"
    );
    assert_eq!(
        snapshot.metadata["cancellation"]["cancelled"],
        serde_json::json!(true)
    );

    // Aborting again now that the run has ended (and been deregistered)
    // reads as unknown, not as a stale success.
    let repeat_abort_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/abort"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let repeat_resp = test::call_service(&*app, repeat_abort_req).await;
    assert_eq!(repeat_resp.status(), 404);
}
