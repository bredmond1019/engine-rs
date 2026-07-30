//! EN.6.F task 13: hermetic, in-process actix harness for the three
//! suspend/resume routes (`POST /events/{run_id}/pause`, `POST
//! /events/{event_id}/resume`, `GET /events/suspended`) plus the
//! suspend-aware readback (task 12), the SSE `clear_terminal` regression
//! (task 9), and abort against a suspended run.
//!
//! Patterned on `abort_integration.rs` / `async_lifecycle.rs`: every fixture
//! node is a plain in-memory `Node` impl, `AppState::durable` is built with
//! `spawn_durable_writer(None)`, and the whole suite runs with **no
//! `DATABASE_URL`** — resume must work purely off the in-memory suspended
//! index (`crate::suspend`).

use std::collections::HashMap as StdHashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{test, web, App};
use chrono::Utc;
use engine_contract::TaskContext;
use engine_core::dispatch::Dispatcher;
use engine_core::{
    BudgetLedger, Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema,
};
use engine_serve::abort::RunRegistry;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;
use engine_serve::suspend;
use engine_serve::test_fixtures::WaitNode;
use tokio::sync::Notify;
use uuid::Uuid;

const API_KEY: &str = "suspend-resume-http-test-key";

// -- fixture nodes -----------------------------------------------------

/// Completes immediately, stamping `{ "ran": true }` under its own identity.
struct SuccessNode {
    identity: &'static str,
}

impl SuccessNode {
    fn new(identity: &'static str) -> Self {
        Self { identity }
    }
}

#[async_trait::async_trait]
impl Node for SuccessNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
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

// -- fixture schemas -----------------------------------------------------

fn two_wait_then_success_schema(workflow_type: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "NodeA".to_string(),
        NodeConfig::new("NodeA", vec!["NodeB".to_string()]),
    );
    nodes.insert(
        "NodeB".to_string(),
        NodeConfig::new("NodeB", vec!["NodeC".to_string()]),
    );
    nodes.insert("NodeC".to_string(), NodeConfig::new("NodeC", vec![]));
    WorkflowSchema::new(workflow_type, "NodeA", nodes)
}

fn suspend_node_schema(workflow_type: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "SuspendNode".to_string(),
        NodeConfig::new("SuspendNode", vec!["MarkerNode".to_string()]),
    );
    nodes.insert(
        "MarkerNode".to_string(),
        NodeConfig::new("MarkerNode", vec![]),
    );
    WorkflowSchema::new(workflow_type, "SuspendNode", nodes)
}

fn single_success_schema(workflow_type: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert("OnlyNode".to_string(), NodeConfig::new("OnlyNode", vec![]));
    WorkflowSchema::new(workflow_type, "OnlyNode", nodes)
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

/// A dispatcher with a single `SuspendNode(enabled: true) -> MarkerNode`
/// workflow registered under `workflow_type` — suspends almost immediately
/// after trigger, so this is the fixture of choice for tests that just need
/// *a* suspended run to exist without controlling timing.
fn dispatcher_with_suspend_fixture(workflow_type: &str) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    let wt = workflow_type.to_string();
    dispatcher.register(
        suspend_node_schema(workflow_type),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(
                engine_core::nodes::SuspendNode::new("SuspendNode").with_enabled(true),
            ));
            registry.register(Box::new(SuccessNode::new("MarkerNode")));
            Ok(Workflow::new(registry, suspend_node_schema(&wt)))
        }),
    );
    dispatcher
}

/// Triggers `workflow_type` against `$app` and blocks until its run lands in
/// the suspended index, evaluating to the `run_id`. A macro (rather than a
/// generic async fn) so it works against whatever unnameable `impl
/// Service<..>` type `test::init_service` produces for each test's own
/// `App` -- mirrors `resume.rs`'s own `suspend_a_run!` precedent.
macro_rules! trigger_and_wait_suspended {
    ($app:expr, $workflow_type:expr) => {{
        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({ "workflow_type": $workflow_type, "data": {} }))
            .to_request();
        let resp = test::call_service(&$app, req).await;
        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let run_id = body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if suspend::list_suspended().into_iter().any(|(id, _)| id == run_id) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "run never landed in the suspended index"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        run_id
    }};
}

macro_rules! get_event {
    ($app:expr, $event_id:expr) => {{
        let req = test::TestRequest::get()
            .uri(&format!("/events/{}", $event_id))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&$app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        body
    }};
}

macro_rules! wait_for_status {
    ($app:expr, $event_id:expr, $status:expr) => {{
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let body = get_event!($app, $event_id);
            if body["status"] == $status {
                break body;
            }
            assert!(
                Instant::now() < deadline,
                "run never reached status {}, last body: {body:?}",
                $status
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }};
}

// -- pause ---------------------------------------------------------------

#[actix_web::test]
async fn pause_without_api_key_is_unauthorized() {
    let state = app_state_with(dispatcher_with_suspend_fixture("pause-401"));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/pause", Uuid::new_v4()))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn pause_unknown_run_is_404() {
    let state = app_state_with(dispatcher_with_suspend_fixture("pause-404"));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/pause", Uuid::new_v4()))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

/// `202` on a live run, `202` again (idempotent) while still live, then
/// `409` once the run has actually suspended -- the full pause-request
/// lifecycle against a real `NodeA -> NodeB -> NodeC` walk.
#[actix_web::test]
async fn pause_is_202_then_idempotent_then_409_once_suspended() {
    const WORKFLOW_TYPE: &str = "pause-lifecycle";
    let release_a = Arc::new(Notify::new());
    let release_a_for_factory = release_a.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_wait_then_success_schema(WORKFLOW_TYPE),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode::named(
                "NodeA",
                release_a_for_factory.clone(),
            )));
            registry.register(Box::new(SuccessNode::new("NodeB")));
            registry.register(Box::new(SuccessNode::new("NodeC")));
            Ok(Workflow::new(
                registry,
                two_wait_then_success_schema(WORKFLOW_TYPE),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let live = state.live.clone();
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

    // Wait until the run is live (NodeA blocked on `release_a`).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if live.list_active().contains(&run_id) {
            break;
        }
        assert!(Instant::now() < deadline, "run never went live");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    for _ in 0..2 {
        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/pause"))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 202, "pause on a live run should 202");
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "pausing");
    }

    // Release NodeA -- the loop-top pause check suspends before NodeB.
    release_a.notify_one();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if suspend::list_suspended()
            .into_iter()
            .any(|(id, _)| id == run_id)
        {
            break;
        }
        assert!(Instant::now() < deadline, "run never suspended");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/pause"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        409,
        "pausing an already-suspended run should 409"
    );
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "suspended");

    suspend::remove_suspended(run_id);
}

// -- suspended list --------------------------------------------------------

#[actix_web::test]
async fn suspended_list_without_api_key_is_unauthorized() {
    let state = app_state_with(dispatcher_with_suspend_fixture("suspended-list-401"));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/events/suspended")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn suspended_list_contains_a_suspended_run_and_omits_it_after_resume() {
    const WORKFLOW_TYPE: &str = "suspended-list-lifecycle";
    let state = app_state_with(dispatcher_with_suspend_fixture(WORKFLOW_TYPE));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let run_id = trigger_and_wait_suspended!(app, WORKFLOW_TYPE);

    let req = test::TestRequest::get()
        .uri("/events/suspended")
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = test::read_body_json(resp).await;
    assert!(body
        .iter()
        .any(|entry| entry["run_id"] == run_id.to_string()));

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&app, resume_req).await;
    assert_eq!(resume_resp.status(), 202);

    let after_req = test::TestRequest::get()
        .uri("/events/suspended")
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let after_resp = test::call_service(&app, after_req).await;
    let after_body: Vec<serde_json::Value> = test::read_body_json(after_resp).await;
    assert!(!after_body
        .iter()
        .any(|entry| entry["run_id"] == run_id.to_string()));
}

// -- resume ----------------------------------------------------------------

#[actix_web::test]
async fn resume_without_api_key_is_unauthorized() {
    let state = app_state_with(dispatcher_with_suspend_fixture("resume-401"));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/resume", Uuid::new_v4()))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn resume_unknown_run_is_404() {
    let state = app_state_with(dispatcher_with_suspend_fixture("resume-404-unknown"));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri(&format!("/events/{}/resume", Uuid::new_v4()))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn resume_an_already_completed_run_is_404() {
    const WORKFLOW_TYPE: &str = "resume-404-completed";
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        single_success_schema(WORKFLOW_TYPE),
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(SuccessNode::new("OnlyNode")));
            Ok(Workflow::new(
                registry,
                single_success_schema(WORKFLOW_TYPE),
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
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let run_id = trigger_body["run_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("run_id should be a parseable UUID");

    wait_for_status!(app, run_id, "succeeded");

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&app, resume_req).await;
    assert_eq!(resume_resp.status(), 404);
}

#[actix_web::test]
async fn a_second_concurrent_resume_is_409() {
    const WORKFLOW_TYPE: &str = "resume-409-concurrent";
    let state = app_state_with(dispatcher_with_suspend_fixture(WORKFLOW_TYPE));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let run_id = trigger_and_wait_suspended!(app, WORKFLOW_TYPE);

    // Simulate a first caller already in flight by taking the resume slot
    // directly, then assert the HTTP layer rejects a second caller.
    match suspend::take_for_resume(run_id) {
        resume_state @ suspend::TakeForResume::Ready(_) => drop(resume_state),
        _ => panic!("expected Ready for the first, direct take_for_resume call"),
    }

    let req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 409);

    // The first (simulated) caller's resume is still retryable.
    suspend::clear_resuming(run_id);
    let retry_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let retry_resp = test::call_service(&app, retry_req).await;
    assert_eq!(retry_resp.status(), 202);
}

/// A `resume_at` that no longer exists in the rebuilt workflow graph --
/// simulating schema drift between suspend and resume -- 422s and leaves the
/// run retryable (`resuming` cleared).
#[actix_web::test]
async fn resume_with_unresolvable_resume_point_is_422_and_clears_resuming() {
    const WORKFLOW_TYPE: &str = "resume-422-drifted";
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        single_success_schema(WORKFLOW_TYPE),
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(SuccessNode::new("OnlyNode")));
            Ok(Workflow::new(
                registry,
                single_success_schema(WORKFLOW_TYPE),
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

    let run_id = Uuid::new_v4();
    let mut metadata = serde_json::json!({});
    engine_core::suspend::stamp_suspended(
        &mut metadata,
        engine_core::suspend::Suspension {
            resume_at: "SomeNodeThatDoesNotExist",
            reason: engine_core::suspend::SuspendReason::OperatorPause,
            origin_identity: Some("OnlyNode"),
            ledger: &BudgetLedger::new(),
        },
    );
    let snapshot = TaskContext {
        event: serde_json::Value::Null,
        nodes: StdHashMap::new(),
        metadata,
        node_runs: StdHashMap::new(),
    };
    suspend::insert_suspended(
        run_id,
        suspend::SuspendedEntry {
            workflow_type: WORKFLOW_TYPE.to_string(),
            data: serde_json::json!({}),
            snapshot,
            created_at: Utc::now(),
            suspended_at: Utc::now(),
            resume_at: "SomeNodeThatDoesNotExist".to_string(),
            reason: "operator_pause".to_string(),
            resuming: false,
        },
    );

    let req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["resume_at"], "SomeNodeThatDoesNotExist");

    match suspend::take_for_resume(run_id) {
        suspend::TakeForResume::Ready(_) => {}
        _ => panic!("expected the failed resume to have cleared `resuming`"),
    }

    suspend::remove_suspended(run_id);
}

// -- full round trip + no re-execution -------------------------------------

/// The block's core acceptance criteria, driven through one real
/// `NodeA -> NodeB -> NodeC` walk: trigger -> pause -> readback `suspended`
/// -> resume -> readback `succeeded`, with `event_id`/`created_at` unchanged
/// and `NodeA`'s `completed_at` untouched by the resume (proof that it is
/// never re-executed).
#[actix_web::test]
async fn full_round_trip_resumes_without_re_executing_completed_nodes() {
    const WORKFLOW_TYPE: &str = "round-trip-lifecycle";
    let release_a = Arc::new(Notify::new());
    let release_b = Arc::new(Notify::new());
    let release_a_for_factory = release_a.clone();
    let release_b_for_factory = release_b.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_wait_then_success_schema(WORKFLOW_TYPE),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode::named(
                "NodeA",
                release_a_for_factory.clone(),
            )));
            registry.register(Box::new(WaitNode::named(
                "NodeB",
                release_b_for_factory.clone(),
            )));
            registry.register(Box::new(SuccessNode::new("NodeC")));
            Ok(Workflow::new(
                registry,
                two_wait_then_success_schema(WORKFLOW_TYPE),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let live = state.live.clone();
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
    let event_id = trigger_body["event_id"].clone();
    assert_eq!(event_id, serde_json::json!(run_id));

    // Wait until live, capture the original `created_at`.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if live.list_active().contains(&run_id) {
            break;
        }
        assert!(Instant::now() < deadline, "run never went live");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let running_body = get_event!(app, run_id);
    let original_created_at = running_body["created_at"].clone();

    let pause_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/pause"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let pause_resp = test::call_service(&app, pause_req).await;
    assert_eq!(pause_resp.status(), 202);

    release_a.notify_one();

    let suspended_body = wait_for_status!(app, run_id, "suspended");
    assert_eq!(suspended_body["event_id"], event_id);
    assert_eq!(suspended_body["created_at"], original_created_at);
    let node_a_completed_at =
        suspended_body["task_context"]["node_runs"]["NodeA"]["completed_at"].clone();
    assert!(
        !node_a_completed_at.is_null(),
        "NodeA should have completed before the suspend"
    );
    assert_eq!(
        suspended_body["task_context"]["node_runs"]["NodeB"]["status"],
        serde_json::json!("pending"),
        "NodeB must not have started before the suspend"
    );

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&app, resume_req).await;
    assert_eq!(resume_resp.status(), 202);
    let resume_body: serde_json::Value = test::read_body_json(resume_resp).await;
    assert_eq!(resume_body["event_id"], event_id);
    assert_eq!(resume_body["resume_at"], "NodeB");

    release_b.notify_one();

    let final_body = wait_for_status!(app, run_id, "succeeded");
    assert_eq!(final_body["event_id"], event_id);
    assert_eq!(
        final_body["created_at"], original_created_at,
        "created_at must be unchanged across suspend/resume"
    );
    assert_eq!(
        final_body["task_context"]["node_runs"]["NodeA"]["completed_at"], node_a_completed_at,
        "NodeA's completed_at must be untouched -- it must not be re-executed on resume"
    );
    assert_eq!(
        final_body["task_context"]["node_runs"]["NodeB"]["status"],
        serde_json::json!("success")
    );
    assert_eq!(
        final_body["task_context"]["node_runs"]["NodeC"]["status"],
        serde_json::json!("success")
    );
}

// -- SSE / clear_terminal regression ---------------------------------------

/// A subscriber attached before the pause receives a `suspended`, `terminal:
/// true` frame and the stream closes; a **fresh** subscriber attached after
/// the resume receives live frames and a real terminal frame. This is the
/// `clear_terminal` regression test (task 9) -- without it, the second
/// subscribe would replay the stale cached `suspended` frame forever.
#[actix_web::test]
async fn sse_subscriber_sees_suspended_terminal_then_a_fresh_subscriber_sees_the_real_terminal() {
    const WORKFLOW_TYPE: &str = "sse-clear-terminal-regression";
    let release_a = Arc::new(Notify::new());
    let release_b = Arc::new(Notify::new());
    let release_a_for_factory = release_a.clone();
    let release_b_for_factory = release_b.clone();

    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        two_wait_then_success_schema(WORKFLOW_TYPE),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(WaitNode::named(
                "NodeA",
                release_a_for_factory.clone(),
            )));
            registry.register(Box::new(WaitNode::named(
                "NodeB",
                release_b_for_factory.clone(),
            )));
            registry.register(Box::new(SuccessNode::new("NodeC")));
            Ok(Workflow::new(
                registry,
                two_wait_then_success_schema(WORKFLOW_TYPE),
            ))
        }),
    );
    let state = app_state_with(dispatcher);
    let app = Rc::new(
        test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await,
    );

    let trigger_req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", API_KEY))
        .set_json(serde_json::json!({ "workflow_type": WORKFLOW_TYPE, "data": {} }))
        .to_request();
    let trigger_resp = test::call_service(&*app, trigger_req).await;
    let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
    let event_id = trigger_body["event_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("event_id should be a parseable UUID");

    // Connect the first subscriber before pausing/releasing anything.
    let deadline = Instant::now() + Duration::from_secs(5);
    let first_stream_resp = loop {
        let stream_req = test::TestRequest::get()
            .uri(&format!("/events/{event_id}/stream"))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&*app, stream_req).await;
        if resp.status() == 200 {
            break resp;
        }
        assert!(Instant::now() < deadline, "stream endpoint never connected");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    let pause_req = test::TestRequest::post()
        .uri(&format!("/events/{event_id}/pause"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let pause_resp = test::call_service(&*app, pause_req).await;
    assert_eq!(pause_resp.status(), 202);

    let release_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        release_a.notify_one();
    });
    let first_body_bytes = test::read_body(first_stream_resp).await;
    release_handle.await.expect("release task should not panic");

    let first_body =
        String::from_utf8(first_body_bytes.to_vec()).expect("SSE body should be UTF-8");
    let first_frames: Vec<serde_json::Value> = first_body
        .split("\n\n")
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let payload = line
                .strip_prefix("data: ")
                .expect("every frame carries data:");
            serde_json::from_str(payload).expect("frame payload should be valid JSON")
        })
        .collect();
    assert!(
        !first_frames.is_empty(),
        "expected at least the suspended terminal frame"
    );
    let last_first = first_frames.last().unwrap();
    assert_eq!(last_first["terminal"], serde_json::json!(true));
    assert_eq!(last_first["status"], serde_json::json!("suspended"));

    // Wait for the run to actually land in the suspended index before
    // resuming (the stream body finishing is a downstream effect of the
    // same transition, but this makes the ordering explicit and avoids a
    // resume racing ahead of the suspended-index insert).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if suspend::list_suspended()
            .into_iter()
            .any(|(id, _)| id == event_id)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "run never landed in the suspended index"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{event_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&*app, resume_req).await;
    assert_eq!(resume_resp.status(), 202);

    // A fresh subscribe after the resume must NOT replay the stale cached
    // suspended frame -- this is exactly what `clear_terminal` fixes.
    let deadline = Instant::now() + Duration::from_secs(5);
    let second_stream_resp = loop {
        let stream_req = test::TestRequest::get()
            .uri(&format!("/events/{event_id}/stream"))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&*app, stream_req).await;
        if resp.status() == 200 {
            break resp;
        }
        assert!(
            Instant::now() < deadline,
            "post-resume stream never connected"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    let release_b_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        release_b.notify_one();
    });
    let second_body_bytes = test::read_body(second_stream_resp).await;
    release_b_handle
        .await
        .expect("release task should not panic");

    let second_body =
        String::from_utf8(second_body_bytes.to_vec()).expect("SSE body should be UTF-8");
    let second_frames: Vec<serde_json::Value> = second_body
        .split("\n\n")
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let payload = line
                .strip_prefix("data: ")
                .expect("every frame carries data:");
            serde_json::from_str(payload).expect("frame payload should be valid JSON")
        })
        .collect();

    assert!(
        !second_frames.is_empty(),
        "the post-resume subscriber must receive real frames, not nothing"
    );
    let last_second = second_frames.last().unwrap();
    assert_eq!(
        last_second["terminal"],
        serde_json::json!(true),
        "the post-resume stream must end on a real terminal frame"
    );
    assert_eq!(
        last_second["status"],
        serde_json::json!("succeeded"),
        "without clear_terminal this would still read \"suspended\" from the stale cache"
    );
    assert!(
        second_frames
            .iter()
            .any(|f| f["terminal"] == serde_json::json!(false)),
        "the post-resume subscriber should also see at least one live (non-terminal) frame: {second_frames:?}"
    );
}

// -- abort of a suspended run ------------------------------------------------

/// A suspended run has no live `CancellationToken` (nobody is checking it
/// once suspended), but it must still be killable: `POST
/// /events/{run_id}/abort` falls back to the suspended index.
#[actix_web::test]
async fn aborting_a_suspended_run_returns_202_and_the_readback_becomes_cancelled() {
    const WORKFLOW_TYPE: &str = "abort-suspended-lifecycle";
    let state = app_state_with(dispatcher_with_suspend_fixture(WORKFLOW_TYPE));
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let run_id = trigger_and_wait_suspended!(app, WORKFLOW_TYPE);

    let abort_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/abort"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let abort_resp = test::call_service(&app, abort_req).await;
    assert_eq!(abort_resp.status(), 202);

    let body = wait_for_status!(app, run_id, "cancelled");
    assert_eq!(body["event_id"], serde_json::json!(run_id));

    assert!(
        !suspend::list_suspended()
            .into_iter()
            .any(|(id, _)| id == run_id),
        "an aborted suspended run must leave the suspended index"
    );
}
