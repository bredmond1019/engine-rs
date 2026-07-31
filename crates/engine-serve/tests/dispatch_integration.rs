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

// EN.3.K task 7: `REPO_REGISTRY_LOCK`'s std `MutexGuard` is held across
// `.await` points by design — it serializes tests that share the
// process-global repo registry seam, not data an async task contends over
// concurrently, so the guard's lifetime spanning the whole test body is
// intentional (mirrors `crates/engine-serve/src/http.rs`'s identical
// `registry_test_lock()` precedent).
#![allow(clippy::await_holding_lock)]

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
    dispatcher.register(
        fixture_schema(),
        Box::new(|_event: &serde_json::Value| Ok(fixture_workflow())),
    );

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
/// EN.5.F: the trigger spawns the run rather than awaiting it, so this test
/// waits for `RunRegistry` deregistration before reading the run's snapshot.
#[actix_web::test]
async fn triggering_through_dispatch_records_live_state_with_no_db_query() {
    let state = test_app_state();
    let live = state.live.clone();
    let runs = state.runs.clone();
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

    // EN.5.F: the trigger no longer awaits the run, so wait for the spawned
    // task's cleanup (registry deregistration) before reading the "final"
    // snapshot back.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while runs.get(run_id).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "spawned run did not finish cleanup within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

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

// --- EN.3.K task 7: the real SDLC_FLOW dispatch-target 422 cases ----------
//
// The cases above exercise a small fixture workflow; these exercise the
// *real* `SDLC_FLOW` workflow (`engine_serve::workflows::register_sdlc_flow`)
// registered through task 4's process-global repo-registry seam
// (`set_repo_registry`/`clear_repo_registry`), so the accept/reject decision
// is proven against the actual graph a served run would dispatch, not a
// stand-in. `crates/engine-serve/src/http.rs` already carries unit-level
// coverage of this same decision against a `MarkerNode` fixture (EN.3.K
// task 5) — this suite is the hermetic HTTP-layer re-assertion against the
// real workflow.
//
// Guards the process-global repo registry so these tests never race others
// in the same nextest process (nextest forks one process per test, but the
// guard keeps the intent explicit for any future addition to this file that
// might share a process).
static REPO_REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `AppState` whose dispatcher carries the real `SDLC_FLOW` workflow,
/// registered via `engine_serve::workflows::register_sdlc_flow` — which
/// reads whatever repo registry is currently installed on the process-global
/// seam at *registration* time. Callers must install the registry (or clear
/// it) before calling this.
fn test_app_state_with_real_sdlc_flow() -> AppState {
    let mut dispatcher = Dispatcher::new();
    engine_serve::workflows::register_sdlc_flow(&mut dispatcher);

    AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(None),
        runs: RunRegistry::new(),
        api_key: "integration-test-key".to_string(),
    }
}

/// A tempdir "brain root" with a single `[[repos]]` entry (`alpha`) whose
/// `planning/my-spec/` directory exists but carries no `tasks.json` — the
/// legitimate `GenerateTasksNode`/`SpecExistsRouterNode` path (Case C), and
/// also the "known repo" success path other cases build on.
fn tempdir_registry_with_alpha_spec_dir_no_tasks_json() -> (
    tempfile::TempDir,
    Arc<engine_core::repo_registry::RepoRegistry>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let alpha = dir.path().join("alpha");
    std::fs::create_dir_all(alpha.join("planning").join("my-spec"))
        .expect("mkdir alpha/planning/my-spec");
    std::fs::write(
        dir.path().join("brain.toml"),
        "[[repos]]\nslug = \"alpha\"\nrepo_path = \"alpha\"\n",
    )
    .expect("write brain.toml");
    let registry = Arc::new(
        engine_core::repo_registry::RepoRegistry::from_brain_root(dir.path())
            .expect("registry should build"),
    );
    (dir, registry)
}

/// Case A: a real `SDLC_FLOW` request naming an unknown `repo` slug is
/// rejected 422 and the response body names the offending slug. (The
/// rejection can surface either from the dispatch factory's own
/// `resolve_target_root` failure — `DispatchError::PolicyResolutionFailed`,
/// mapped to 422 by `crate::http::post_events` — or from the pre-flight
/// `unknown repo` check; either way, no run is spawned and the slug is
/// named.)
#[actix_web::test]
async fn real_sdlc_flow_unknown_repo_slug_returns_422_naming_the_slug() {
    let _guard = REPO_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let previous = engine_serve::workflows::repo_registry();
    let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
    engine_serve::workflows::set_repo_registry(registry);

    let state = test_app_state_with_real_sdlc_flow();
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
            "workflow_type": "SDLC_FLOW",
            "data": { "spec_slug": "my-spec", "repo": "not-a-real-repo" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = test::read_body_json(resp).await;
    let body_text = body.to_string();
    assert!(
        body_text.contains("not-a-real-repo"),
        "response body should name the unknown repo slug, got: {body_text}"
    );

    if let Some(prev) = previous {
        engine_serve::workflows::set_repo_registry(prev);
    } else {
        engine_serve::workflows::clear_repo_registry();
    }
}

/// Case B: a known `repo` slug with a `spec_slug` whose directory does not
/// exist under that repo's root is rejected 422 and the response body names
/// the offending spec slug.
#[actix_web::test]
async fn real_sdlc_flow_known_repo_absent_spec_dir_returns_422_naming_the_spec_slug() {
    let _guard = REPO_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let previous = engine_serve::workflows::repo_registry();
    let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
    engine_serve::workflows::set_repo_registry(registry);

    let state = test_app_state_with_real_sdlc_flow();
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
            "workflow_type": "SDLC_FLOW",
            "data": { "spec_slug": "does-not-exist", "repo": "alpha" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = test::read_body_json(resp).await;
    let body_text = body.to_string();
    assert!(
        body_text.contains("does-not-exist"),
        "response body should name the unknown spec_slug, got: {body_text}"
    );

    if let Some(prev) = previous {
        engine_serve::workflows::set_repo_registry(prev);
    } else {
        engine_serve::workflows::clear_repo_registry();
    }
}

/// Case C (the non-regression): a known `repo` slug with a spec directory
/// that EXISTS but carries no `tasks.json` still dispatches — 202 with a
/// `run_id` — confirming the legitimate `GenerateTasksNode` path is intact
/// against the real `SDLC_FLOW` graph. The spawned run is left to run and
/// fail harmlessly against the tempdir's stubbed/absent subprocesses (no
/// real git checkout, no real `harness.json` commands) — this test asserts
/// only the accept/reject decision at the HTTP layer, not that the flow
/// completes. Do not "fix" this into awaiting a live run.
#[actix_web::test]
async fn real_sdlc_flow_known_repo_spec_dir_without_tasks_json_still_dispatches() {
    let _guard = REPO_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let previous = engine_serve::workflows::repo_registry();
    let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
    engine_serve::workflows::set_repo_registry(registry);

    let state = test_app_state_with_real_sdlc_flow();
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
            "workflow_type": "SDLC_FLOW",
            "data": { "spec_slug": "my-spec", "repo": "alpha" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert!(body["run_id"].is_string());
    assert_eq!(body["run_id"], body["event_id"]);

    if let Some(prev) = previous {
        engine_serve::workflows::set_repo_registry(prev);
    } else {
        engine_serve::workflows::clear_repo_registry();
    }
}

/// Case D: no `repo` field at all, with a spec directory that exists
/// relative to the test process's cwd, behaves exactly as before this
/// block — 202 with `run_id` and `event_id` equal — proving the additive
/// default (an absent `repo` still resolves via `current_dir()`).
#[actix_web::test]
async fn real_sdlc_flow_no_repo_field_behaves_as_before_when_spec_dir_exists() {
    let _guard = REPO_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let previous_registry = engine_serve::workflows::repo_registry();
    engine_serve::workflows::clear_repo_registry();
    let previous_cwd = std::env::current_dir().expect("current_dir");

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("planning").join("my-spec"))
        .expect("mkdir planning/my-spec");
    std::env::set_current_dir(dir.path()).expect("set_current_dir");

    let state = test_app_state_with_real_sdlc_flow();
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
            "workflow_type": "SDLC_FLOW",
            "data": { "spec_slug": "my-spec" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = test::read_body_json(resp).await;
    assert!(body["run_id"].is_string());
    assert_eq!(body["run_id"], body["event_id"]);

    std::env::set_current_dir(previous_cwd).expect("restore cwd");
    if let Some(prev) = previous_registry {
        engine_serve::workflows::set_repo_registry(prev);
    }
}

/// Case E: a valid `repo`-bearing request still returns `202
/// {run_id, event_id}` with `event_id == run_id` — the EN.5.F contract,
/// re-asserted here because this block adds code on the path that produces
/// it (the pre-flight validation runs before `run_id` is even minted).
#[actix_web::test]
async fn real_sdlc_flow_valid_repo_bearing_request_returns_202_with_matching_ids() {
    let _guard = REPO_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let previous = engine_serve::workflows::repo_registry();
    let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
    engine_serve::workflows::set_repo_registry(registry);

    let state = test_app_state_with_real_sdlc_flow();
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
            "workflow_type": "SDLC_FLOW",
            "data": { "spec_slug": "my-spec", "repo": "alpha" },
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 202);
    let body: serde_json::Value = test::read_body_json(resp).await;
    let run_id = body["run_id"].as_str().expect("run_id is a string");
    let event_id = body["event_id"].as_str().expect("event_id is a string");
    assert_eq!(run_id, event_id);
    uuid::Uuid::parse_str(run_id).expect("run_id parses as a uuid");

    if let Some(prev) = previous {
        engine_serve::workflows::set_repo_registry(prev);
    } else {
        engine_serve::workflows::clear_repo_registry();
    }
}
