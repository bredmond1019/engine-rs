//! The four-endpoint HTTP surface (D3: `actix-web`) wiring dispatch (task 1),
//! live-state (task 2), and the durable writer (task 3) into `bastion serve`'s
//! embedded engine.
//!
//! Routes (registered via [`configure`], shared by both the serve binary and
//! the in-process test harness):
//!
//! - `POST /events/` — requires a valid `X-API-Key` header (401 without it);
//!   resolves `workflow_type` via the [`crate::dispatch::Dispatcher`] (422 on an
//!   unregistered type); runs the workflow, feeding both the live-state
//!   recorder and the durable writer through `on_progress`.
//! - `GET /health` — 200.
//! - `GET /workflows` — the list of registered workflow types.
//! - `GET /workflows/{type}/graph` — the schema/graph for a registered type,
//!   404 for an unknown one.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_core::OnProgress;
use serde::Deserialize;
use uuid::Uuid;

use crate::dispatch::{DispatchError, Dispatcher};
use crate::durable::{durable_on_progress, DurableHandle};
use crate::live_state::LiveStateStore;

/// Shared application state handed to every handler via `web::Data<AppState>`.
pub struct AppState {
    pub dispatcher: Arc<Dispatcher>,
    pub live: LiveStateStore,
    pub durable: DurableHandle,
    pub api_key: String,
}

/// Register all four routes on `cfg`, so the serve binary and the test
/// harness share one route table.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health))
        .route("/workflows", web::get().to(list_workflows))
        .route(
            "/workflows/{workflow_type}/graph",
            web::get().to(workflow_graph),
        )
        .route("/events/", web::post().to(post_events));
}

async fn health() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

/// The registered workflow types (sorted for a deterministic response).
async fn list_workflows(state: web::Data<AppState>) -> impl Responder {
    let mut types = state.dispatcher.registered_types();
    types.sort();
    HttpResponse::Ok().json(types)
}

/// The declared `WorkflowSchema` for a registered `workflow_type`; 404 for an
/// unknown one.
async fn workflow_graph(path: web::Path<String>, state: web::Data<AppState>) -> impl Responder {
    let workflow_type = path.into_inner();
    match state.dispatcher.resolve_schema(&workflow_type) {
        Ok(schema) => HttpResponse::Ok().json(schema),
        Err(DispatchError::UnknownWorkflowType(_)) => {
            HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown workflow_type" }))
        }
    }
}

/// Body accepted by `POST /events/`: the target `workflow_type` and the
/// triggering event payload (defaults to an empty object when omitted).
#[derive(Debug, Deserialize)]
struct TriggerBody {
    workflow_type: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Whether `req` carries an `X-API-Key` header matching `expected`.
fn check_api_key(req: &HttpRequest, expected: &str) -> bool {
    req.headers()
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected)
        .unwrap_or(false)
}

/// `POST /events/` — trigger dispatch: 401 on a missing/bad `X-API-Key`, 422
/// on an unregistered `workflow_type`, otherwise runs the workflow feeding the
/// live-state store and the durable writer through `on_progress`, returning
/// the freshly-minted `run_id` the local Console reads live state by.
async fn post_events(
    req: HttpRequest,
    body: web::Json<TriggerBody>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let workflow = match state.dispatcher.dispatch(&body.workflow_type) {
        Ok(workflow) => workflow,
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "unknown workflow_type",
                "workflow_type": workflow_type,
            }));
        }
    };

    let run_id = Uuid::new_v4();
    let workflow_type = body.workflow_type.clone();
    let data = body.data.clone();
    let live = state.live.clone();
    let durable_handle = state.durable.clone();

    // Build the `OnProgress` box *inside* the blocking closure: its captures
    // (`live`, `durable_progress`) are individually `Send`, but the trait
    // object `OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>` carries no
    // `Send` bound of its own, so it cannot be moved across the
    // `web::block`/`spawn_blocking` boundary as a value.
    let result = web::block(move || {
        let mut durable_progress =
            durable_on_progress(durable_handle, run_id, workflow_type, data.clone());
        let on_progress: OnProgress<'_> = Box::new(move |snapshot| {
            live.record(run_id, snapshot);
            durable_progress(snapshot);
        });
        workflow.run(data, on_progress)
    })
    .await
    .map(|inner| inner.map_err(|err| err.to_string()));

    match result {
        Ok(Ok(_task_context)) => {
            HttpResponse::Accepted().json(serde_json::json!({ "run_id": run_id }))
        }
        Ok(Err(message)) => HttpResponse::InternalServerError()
            .json(serde_json::json!({ "error": message, "run_id": run_id })),
        Err(err) => HttpResponse::InternalServerError()
            .json(serde_json::json!({ "error": err.to_string(), "run_id": run_id })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use engine_contract::TaskContext;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;

    struct MarkerNode;

    impl Node for MarkerNode {
        fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    fn fixture_schema(workflow_type: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
    }

    fn test_app_state() -> AppState {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("fixture"),
            Box::new(|| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Workflow::new(registry, fixture_schema("fixture"))
            }),
        );

        AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            api_key: "test-key".to_string(),
        }
    }

    #[actix_web::test]
    async fn health_returns_200() {
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

    #[actix_web::test]
    async fn post_events_without_key_is_rejected() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/")
            .set_json(serde_json::json!({ "workflow_type": "fixture", "data": {} }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn post_events_unknown_workflow_type_returns_422() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "nope", "data": {} }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 422);
    }

    #[actix_web::test]
    async fn workflow_graph_unknown_type_returns_404() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/workflows/nope/graph")
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn list_workflows_lists_registered() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get().uri("/workflows").to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 200);
        let body: Vec<String> = test::read_body_json(resp).await;
        assert!(body.contains(&"fixture".to_string()));
    }

    #[actix_web::test]
    async fn post_events_with_valid_key_and_known_type_is_accepted() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "fixture", "data": {} }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
    }
}
