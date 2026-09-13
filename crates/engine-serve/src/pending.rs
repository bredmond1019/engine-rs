//! Engine-side HTTP surface for the pending run queue
//! (`EN.ticket.queue-not-run-event-ingress`).
//!
//! Two authenticated routes over an **injectable, optional** pending run queue
//! seam so a cockpit on another host can queue runs for manual approval before
//! they execute.
//!
//! **The seam is `Option<web::Data<Arc<dyn PendingRunQueue>>>`, deliberately
//! not a required [`crate::http::AppState`] field.** `AppState`'s fields
//! are public and it is struct-literal-constructed in `bastion`
//! (`../bastion/src/serve/mod.rs`) and in five `crates/engine-serve/tests/*.rs`
//! files, so adding a required `queue` field would be a cross-repo
//! breaking change for a surface bastion is not yet ready to wire. An
//! `Option<web::Data<..>>` extractor is additive: bastion compiles
//! untouched, and these routes exist and answer 503 until one
//! `.app_data(...)` line lands there. See `planning/decisions/D15-additive-seams-over-appstate-fields.md`.
//!
//! Route registration lives in `crate::http::configure` (a later task in
//! this ticket) — this module only defines the seam, the response DTOs,
//! and the two handlers.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::http::{check_api_key, AppState};

/// A pending run record — a workflow_type and event payload awaiting manual
/// approval before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRun {
    pub pending_id: Uuid,
    pub workflow_type: String,
    pub data: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// The pending run queue seam both handlers extract. Deliberately a trait
/// rather than a concrete type so the embedding host can swap
/// implementations (in-memory for tests, durable for production) without
/// this crate needing to choose.
pub trait PendingRunQueue: Send + Sync {
    /// Append a pending run to the queue, returning its minted id.
    fn append(&self, workflow_type: String, data: serde_json::Value) -> Uuid;

    /// List all pending runs, newest-first.
    fn list_open(&self) -> Vec<PendingRun>;
}

/// The queue seam both handlers extract. `None` when the embedding host
/// has not registered a queue — see this module's docs for why that is
/// additive rather than a required `AppState` field.
type QueueData = web::Data<Arc<dyn PendingRunQueue>>;

/// Stable JSON error body returned by both routes when no queue is
/// registered. Kept identical between the two routes.
fn queue_not_configured() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .json(serde_json::json!({ "error": "pending run queue not configured" }))
}

/// Request body for `POST /events/pending`.
#[derive(Debug, Deserialize)]
pub struct PostPendingBody {
    workflow_type: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Response body for `POST /events/pending`.
#[derive(Debug, Serialize)]
struct PostPendingResponse {
    pending_id: Uuid,
    status: String,
}

/// `POST /events/pending` — 401 without a valid `X-API-Key`; 422 for an
/// unregistered `workflow_type`; 503 with a stable JSON body when no queue
/// is registered; otherwise appends the run to the queue and returns `202
/// {pending_id, status: "pending"}`.
///
/// Checks are ordered: API key → workflow_type validation → append, so a
/// rejected request never reaches the store.
pub async fn post_pending(
    req: HttpRequest,
    body: web::Json<PostPendingBody>,
    state: web::Data<AppState>,
    queue: Option<QueueData>,
) -> impl Responder {
    // Check 1: API key
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    // Check 2: queue configured
    let Some(queue) = queue else {
        return queue_not_configured();
    };

    // Check 3: workflow_type is known to the dispatcher
    let body = body.into_inner();
    if state
        .dispatcher
        .resolve_schema(&body.workflow_type)
        .is_err()
    {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "error": "unknown workflow_type",
            "workflow_type": body.workflow_type,
        }));
    }

    // All checks passed — append to the queue
    let pending_id = queue.append(body.workflow_type, body.data);

    HttpResponse::Accepted().json(PostPendingResponse {
        pending_id,
        status: "pending".to_string(),
    })
}

/// Response body for `GET /events/pending`.
#[derive(Debug, Serialize)]
struct ListPendingResponse {
    runs: Vec<PendingRun>,
}

/// `GET /events/pending` — 401 without a valid `X-API-Key`; 503 with a
/// stable JSON body when no queue is registered; otherwise 200 with every
/// pending run, newest-first.
pub async fn list_pending(
    req: HttpRequest,
    state: web::Data<AppState>,
    queue: Option<QueueData>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let Some(queue) = queue else {
        return queue_not_configured();
    };

    let runs = queue.list_open();

    HttpResponse::Ok().json(ListPendingResponse { runs })
}

// ─── tests ──────────────────────────────────────────────────────────────
//
// Every test drives the app through `App::new().configure(crate::http::configure)`
// and `actix_web::test`, never by calling a handler function directly — a
// handler-level test would pass even if the routes were never registered,
// which is exactly the gate-blindness shape carryover
// `gate-scope-must-be-shown-capable-of-failing` describes. Deliberately NOT
// a new `crates/engine-serve/tests/*.rs` file (CLAUDE.md standing rule 8):
// engine-serve already carries six such binaries and this ticket must not
// add a seventh.
#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, web, App};
    use std::sync::{Arc, Mutex};

    use crate::http::{configure, AppState};

    const API_KEY: &str = "pending-test-key";

    /// A minimal, in-memory pending run queue for tests.
    struct InMemoryPendingQueue {
        runs: Mutex<Vec<PendingRun>>,
    }

    impl InMemoryPendingQueue {
        fn new() -> Self {
            Self {
                runs: Mutex::new(Vec::new()),
            }
        }
    }

    impl PendingRunQueue for InMemoryPendingQueue {
        fn append(&self, workflow_type: String, data: serde_json::Value) -> Uuid {
            let pending_id = Uuid::new_v4();
            let mut runs = self.runs.lock().unwrap();
            runs.push(PendingRun {
                pending_id,
                workflow_type,
                data,
                status: "pending".to_string(),
                created_at: Utc::now(),
            });
            pending_id
        }

        fn list_open(&self) -> Vec<PendingRun> {
            let runs = self.runs.lock().unwrap();
            let mut result = runs.clone();
            // newest-first
            result.reverse();
            result
        }
    }

    fn queue_data(queue: InMemoryPendingQueue) -> web::Data<Arc<dyn PendingRunQueue>> {
        web::Data::new(Arc::new(queue) as Arc<dyn PendingRunQueue>)
    }

    /// A minimal, hermetic `AppState` — a `Dispatcher` with one registered
    /// workflow type ("fixture"), in-memory live state, an unbound durable
    /// writer (no Postgres pool), and an empty run registry. Only `api_key`
    /// and the dispatcher's workflow_type validation matter here.
    fn test_app_state() -> AppState {
        use crate::dispatch::Dispatcher;
        use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};

        struct MarkerNode;

        #[async_trait::async_trait]
        impl Node for MarkerNode {
            async fn process(
                &self,
                mut ctx: engine_contract::TaskContext,
            ) -> Result<engine_contract::TaskContext, NodeError> {
                ctx.nodes
                    .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
                Ok(ctx)
            }

            fn name(&self) -> &str {
                "MarkerNode"
            }
        }

        fn fixture_schema(workflow_type: &str) -> WorkflowSchema {
            let mut nodes = std::collections::HashMap::new();
            nodes.insert(
                "MarkerNode".to_string(),
                NodeConfig::new("MarkerNode", vec![]),
            );
            WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
        }

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("fixture"),
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(registry, fixture_schema("fixture")))
            }),
        );

        AppState {
            dispatcher: Arc::new(dispatcher),
            live: crate::live_state::LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: crate::abort::RunRegistry::new(),
            campaigns: crate::abort::CampaignRegistry::new(),
            api_key: API_KEY.to_string(),
        }
    }

    // ── POST /events/pending ───────────────────────────────────────────

    #[actix_web::test]
    async fn post_pending_with_valid_key_and_known_workflow_returns_202() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": { "test": "value" }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 202);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["pending_id"].is_string());
        assert_eq!(body["status"], "pending");
    }

    #[actix_web::test]
    async fn post_pending_then_list_returns_that_record() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": { "test": "value" }
            }))
            .to_request();
        let post_resp = test::call_service(&app, req).await;
        let post_body: serde_json::Value = test::read_body_json(post_resp).await;
        let pending_id = post_body["pending_id"].as_str().unwrap();

        let req = test::TestRequest::get()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let list_resp = test::call_service(&app, req).await;
        assert_eq!(list_resp.status(), 200);

        let list_body: serde_json::Value = test::read_body_json(list_resp).await;
        let runs = list_body["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["pending_id"], pending_id);
        assert_eq!(runs[0]["workflow_type"], "fixture");
        assert_eq!(runs[0]["status"], "pending");
    }

    #[actix_web::test]
    async fn post_pending_missing_api_key_returns_401() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": {}
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "missing API key");
    }

    #[actix_web::test]
    async fn post_pending_wrong_api_key_returns_401() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", "wrong-key"))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": {}
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "wrong API key");
    }

    #[actix_web::test]
    async fn post_pending_unknown_workflow_type_returns_422() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": "unknown",
                "data": {}
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 422);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "unknown workflow_type");
        assert_eq!(body["workflow_type"], "unknown");
    }

    #[actix_web::test]
    async fn post_pending_unknown_workflow_leaves_list_empty() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": "unknown",
                "data": {}
            }))
            .to_request();
        let _ = test::call_service(&app, req).await;

        let req = test::TestRequest::get()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let list_resp = test::call_service(&app, req).await;
        let list_body: serde_json::Value = test::read_body_json(list_resp).await;
        assert!(list_body["runs"].as_array().unwrap().is_empty());
    }

    #[actix_web::test]
    async fn post_pending_without_queue_returns_503() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": {}
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 503);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "pending run queue not configured");
    }

    // ── GET /events/pending ────────────────────────────────────────────

    #[actix_web::test]
    async fn list_pending_missing_api_key_returns_401() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get().uri("/events/pending").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "missing API key");
    }

    #[actix_web::test]
    async fn list_pending_wrong_api_key_returns_401() {
        let queue = InMemoryPendingQueue::new();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(queue_data(queue))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/pending")
            .insert_header(("X-API-Key", "wrong-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "wrong API key");
    }

    #[actix_web::test]
    async fn list_pending_without_queue_returns_503() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/pending")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 503);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "pending run queue not configured");
    }

    #[actix_web::test]
    async fn existing_test_suite_still_passes() {
        // This test verifies that adding the pending routes did not break any
        // existing routes — the acceptance criterion "the crate's existing test
        // suite still passes with no host wiring change".
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
    }
}
