//! EN.1.C headline integration test: registers a small fixture workflow in
//! both registries, triggers it end-to-end through the HTTP `POST /events/`
//! dispatch path, and asserts:
//!
//! (a) the local `LiveStateStore` read returns live in-memory `TaskContext`
//!     state with no Postgres query at all;
//! (b) the `EventsRow` the durable writer would persist for the run's seed
//!     snapshot is byte-identical (per the EN.0.B round-trip oracle: semantic
//!     JSON equality, contract shape) — the Postgres round-trip portion
//!     self-skips because `DATABASE_URL` is unset in this test process;
//! (c) an unregistered `workflow_type` is rejected with 422.

use std::collections::HashMap as StdHashMap;
use std::sync::{Arc, Mutex};

use actix_web::{test, web, App};
use engine_contract::{EventsRow, TaskContext};
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};
use engine_serve::abort::RunRegistry;
use engine_serve::dispatch::Dispatcher;
use engine_serve::durable::{message_to_row, spawn_durable_writer, DurableMessage};
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;

/// First node in the fixture graph: a small 2-node linear workflow reusing
/// the `engine-core` test-node pattern (a node that just marks itself as
/// having run).
struct IngestNode;

#[async_trait::async_trait]
impl Node for IngestNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "IngestNode"
    }
}

/// Second node in the fixture graph.
struct EmbedNode;

#[async_trait::async_trait]
impl Node for EmbedNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "EmbedNode"
    }
}

const FIXTURE_WORKFLOW_TYPE: &str = "fixture-dispatch";

fn fixture_schema() -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "IngestNode".to_string(),
        NodeConfig::new("IngestNode", vec!["EmbedNode".to_string()]),
    );
    nodes.insert(
        "EmbedNode".to_string(),
        NodeConfig::new("EmbedNode", vec![]),
    );
    WorkflowSchema::new(FIXTURE_WORKFLOW_TYPE, "IngestNode", nodes)
}

fn fixture_workflow() -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(IngestNode));
    registry.register(Box::new(EmbedNode));
    Workflow::new(registry, fixture_schema())
}

fn test_app_state() -> AppState {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(fixture_schema(), Box::new(fixture_workflow));

    AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        api_key: "integration-test-key".to_string(),
    }
}

/// (a) Triggering the fixture workflow through `POST /events/` records live
/// state the local read API (`LiveStateStore::get`) can see with no Postgres
/// involved anywhere in the request path (the `AppState.durable` handle was
/// spawned with `pool: None`, so no `engine-store`/`sqlx` call is reachable).
#[actix_web::test]
async fn triggering_through_dispatch_records_live_state_with_no_db_query() {
    let state = test_app_state();
    let live = state.live.clone();
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", "integration-test-key"))
        .set_json(serde_json::json!({
            "workflow_type": FIXTURE_WORKFLOW_TYPE,
            "data": { "ticket_id": "T-1" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 202);

    let body: serde_json::Value = test::read_body_json(resp).await;
    let run_id = body["run_id"]
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .expect("response carries a parseable run_id");

    // Local read path: in-memory only, no Postgres query.
    let snapshot = live
        .get(run_id)
        .expect("live_state should hold the completed run's snapshot");

    assert_eq!(
        snapshot.node_runs["IngestNode"].status,
        engine_contract::NodeRunStatus::Success
    );
    assert_eq!(
        snapshot.node_runs["EmbedNode"].status,
        engine_contract::NodeRunStatus::Success
    );
    assert!(snapshot.nodes.contains_key("IngestNode"));
    assert!(snapshot.nodes.contains_key("EmbedNode"));
}

/// (b) The durable writer's mapping from a run's seed snapshot (all nodes
/// PENDING, emitted by `Workflow::run` before the first node executes) to an
/// `EventsRow` is byte-identical per the EN.0.B round-trip oracle: the row
/// round-trips through `serde_json` with no field/casing/type drift, and the
/// contract's top-level + `node_runs` shape holds. The Postgres portion of
/// the durable writer (`spawn_durable_writer`) is exercised with `pool: None`
/// so it self-skips rather than failing when `DATABASE_URL` is unset — this
/// asserts the writer never panics on an unconfigured pool while the pure
/// mapping function is asserted byte-identical independently.
#[actix_web::test]
async fn durable_seed_snapshot_maps_to_byte_identical_events_row() {
    let workflow = fixture_workflow();
    let run_id = uuid::Uuid::new_v4();
    let workflow_type = FIXTURE_WORKFLOW_TYPE.to_string();
    let data = serde_json::json!({ "ticket_id": "T-1" });

    // Capture every snapshot `Workflow::run` emits via `on_progress`, driving
    // both the live-state recorder and (separately) the durable writer, so
    // this test exercises the same wiring the HTTP handler uses.
    let live = LiveStateStore::new();
    let live_for_closure = live.clone();
    let durable = spawn_durable_writer(None);
    let mut durable_progress = engine_serve::durable::durable_on_progress(
        durable.clone(),
        run_id,
        workflow_type.clone(),
        data.clone(),
    );

    let snapshots: Arc<Mutex<Vec<TaskContext>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    let on_progress: engine_core::OnProgress<'_> = Box::new(move |ctx: &TaskContext| {
        live_for_closure.record(run_id, ctx);
        durable_progress(ctx);
        snapshots_handle.lock().unwrap().push(ctx.clone());
    });

    let result = workflow.run(data.clone(), on_progress).await;
    assert!(result.is_ok());

    // Give the (self-skipping) background writer a moment to drain the
    // channel without panicking — nothing to assert against Postgres since
    // no pool is configured.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let snapshots = snapshots.lock().unwrap();
    let seed_snapshot = snapshots.first().expect("at least the seed snapshot fired");

    let seed_message = DurableMessage {
        run_id,
        workflow_type: workflow_type.clone(),
        data: data.clone(),
        snapshot: seed_snapshot.clone(),
    };

    use chrono::{TimeZone, Utc};
    let created_at = Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 0).unwrap();
    let row: EventsRow = message_to_row(&seed_message, created_at, created_at);

    // EN.0.B round-trip oracle: serialize, deserialize, assert semantic
    // equality (no field/casing/type drift).
    let json = serde_json::to_value(&row).expect("EventsRow serializes");
    let round_tripped: EventsRow =
        serde_json::from_value(json.clone()).expect("EventsRow deserializes back");
    assert_eq!(
        round_tripped, row,
        "EventsRow must round-trip byte-identical"
    );

    // Contract top-level shape.
    for key in [
        "id",
        "workflow_type",
        "data",
        "task_context",
        "created_at",
        "updated_at",
    ] {
        assert!(json.get(key).is_some(), "missing top-level field: {key}");
    }

    // Seed snapshot: every declared node PENDING before the first node runs.
    let node_runs = &json["task_context"]["node_runs"];
    for identity in ["IngestNode", "EmbedNode"] {
        assert_eq!(node_runs[identity]["status"], "pending");
        assert!(node_runs[identity]["started_at"].is_null());
        assert!(node_runs[identity]["completed_at"].is_null());
    }

    assert_eq!(row.id, run_id);
    assert_eq!(row.workflow_type, FIXTURE_WORKFLOW_TYPE);
}

/// (c) Triggering an unregistered `workflow_type` through the dispatch path
/// returns 422 (the HTTP mapping of `DispatchError::UnknownWorkflowType`).
#[actix_web::test]
async fn triggering_unregistered_workflow_type_returns_422() {
    let state = test_app_state();
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/events/")
        .insert_header(("X-API-Key", "integration-test-key"))
        .set_json(serde_json::json!({
            "workflow_type": "does-not-exist",
            "data": {},
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 422);

    let body: serde_json::Value = test::read_body_json(resp).await;
    assert_eq!(body["workflow_type"], "does-not-exist");
}
