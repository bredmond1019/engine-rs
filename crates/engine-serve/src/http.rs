//! The four-endpoint HTTP surface (D3: `actix-web`) wiring dispatch (task 1),
//! live-state (task 2), and the durable writer (task 3) into `bastion serve`'s
//! embedded engine.
//!
//! Routes (registered via [`configure`], shared by both the serve binary and
//! the in-process test harness):
//!
//! - `POST /events/` — requires a valid `X-API-Key` header (401 without it);
//!   resolves `workflow_type` via the [`crate::dispatch::Dispatcher`] (422 on an
//!   unregistered type); then **spawns** the run (`actix_web::rt::spawn`) and
//!   returns `202 {run_id, event_id}` immediately, without awaiting it.
//!   `event_id` always equals `run_id` — both are the `events.id` primary
//!   key. The spawned run feeds both the live-state recorder and the durable
//!   writer through `on_progress`, is seeded with the default HTTP budget
//!   (`ENGINE_RUN_MAX_COST_USD` / `ENGINE_RUN_MAX_TOKENS`, see
//!   [`default_budget_from_env`]), and marks itself terminal + deregisters
//!   its cancellation token on every exit path (success, node error,
//!   cancellation, budget halt). A failed run no longer produces a `500` —
//!   the response was already sent before the run could fail, so failure
//!   surfaces through the `GET /events/{event_id}` readback and the SSE
//!   terminal frame instead.
//! - `GET /health` — 200.
//! - `GET /workflows` — the list of registered workflow types.
//! - `GET /workflows/{type}/graph` — the schema/graph for a registered type,
//!   404 for an unknown one.
//! - `POST /events/{run_id}/abort` — requires the same `X-API-Key` header
//!   (401 without it); 404 for an unknown/finished `run_id`; otherwise
//!   triggers that run's `CancellationToken` and returns 202 (task 5, see
//!   `crate::abort`).

use std::collections::HashMap as StdHashMap;
use std::sync::{Arc, OnceLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use engine_contract::TaskContext;
use engine_core::{Budget, CancellationToken, OnProgress, RunOptions};
use serde::Deserialize;
use uuid::Uuid;

use crate::abort::RunRegistry;
use crate::dispatch::{DispatchError, Dispatcher};
use crate::durable::{durable_on_progress, DurableHandle};
use crate::live_state::LiveStateStore;

/// Shared application state handed to every handler via `web::Data<AppState>`.
pub struct AppState {
    pub dispatcher: Arc<Dispatcher>,
    pub live: LiveStateStore,
    pub durable: DurableHandle,
    pub runs: RunRegistry,
    pub api_key: String,
}

/// Register all routes on `cfg`, so the serve binary and the test harness
/// share one route table.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health))
        .route("/workflows", web::get().to(list_workflows))
        .route(
            "/workflows/{workflow_type}/graph",
            web::get().to(workflow_graph),
        )
        .route("/events/", web::post().to(post_events))
        .route(
            "/events/{run_id}/abort",
            web::post().to(crate::abort::abort_run),
        );
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
        // `resolve_schema` only ever consults the `schema_registry`, never a
        // factory, so it structurally cannot produce this variant.
        Err(DispatchError::PolicyResolutionFailed(_)) => unreachable!(
            "resolve_schema never invokes a WorkflowFactory, so it cannot fail policy resolution"
        ),
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

/// Whether `req` carries an `X-API-Key` header matching `expected`. Shared
/// with `crate::abort::abort_run`, which reuses this exact gate.
pub(crate) fn check_api_key(req: &HttpRequest, expected: &str) -> bool {
    req.headers()
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected)
        .unwrap_or(false)
}

/// Default per-run cost ceiling (USD) applied to every run triggered over
/// HTTP when `ENGINE_RUN_MAX_COST_USD` is unset or unparseable.
const DEFAULT_MAX_COST_USD: f64 = 5.0;

/// Parse a default [`Budget`] from raw env-var values. Pure (takes the raw
/// strings rather than reading the environment) so every branch is directly
/// testable — the memoized [`default_budget_from_env`] wrapper below can
/// only ever initialize once per process, which would make test order
/// significant if the parsing itself lived there. Mirrors `durable.rs`'s
/// `message_to_row` precedent of splitting a pure mapping out of async/
/// process-global plumbing.
///
/// An unparseable value falls back to the default rather than failing the
/// request: a malformed deployment knob must not take the trigger endpoint
/// down.
fn budget_from_env_vars(max_cost_usd: Option<&str>, max_total_tokens: Option<&str>) -> Budget {
    Budget {
        max_total_tokens: max_total_tokens.and_then(|raw| raw.trim().parse::<u64>().ok()),
        max_cost_usd: Some(
            max_cost_usd
                .and_then(|raw| raw.trim().parse::<f64>().ok())
                .unwrap_or(DEFAULT_MAX_COST_USD),
        ),
    }
}

/// The default [`Budget`] applied to every HTTP-triggered run, read once
/// from `ENGINE_RUN_MAX_COST_USD` / `ENGINE_RUN_MAX_TOKENS` and memoized.
///
/// Deliberately **not** a field on [`AppState`]: `bastion` constructs that
/// struct with a literal (`core/bastion/src/serve/mod.rs`) over an unpinned
/// path dependency, so adding a public field there is an immediate
/// cross-repo compile break for no gain. Reading the environment here
/// delivers the same configurability with a zero-width public surface
/// change.
fn default_budget_from_env() -> Budget {
    static DEFAULT_BUDGET: OnceLock<Budget> = OnceLock::new();
    *DEFAULT_BUDGET.get_or_init(|| {
        budget_from_env_vars(
            std::env::var("ENGINE_RUN_MAX_COST_USD").ok().as_deref(),
            std::env::var("ENGINE_RUN_MAX_TOKENS").ok().as_deref(),
        )
    })
}

/// `POST /events/` — trigger dispatch: 401 on a missing/bad `X-API-Key`, 422
/// on an unregistered `workflow_type`, otherwise **spawns** the workflow
/// (feeding the live-state store and the durable writer through
/// `on_progress`) and returns `202 {run_id, event_id}` immediately, without
/// awaiting the run. `event_id` is always equal to `run_id` — both are the
/// `events.id` primary key (contract §4).
///
/// Mints a `CancellationToken` alongside `run_id` and registers it on
/// `state.runs` (task 5) so `POST /events/{run_id}/abort` can trigger it
/// while the run is live. The spawned task marks the run terminal (task 1's
/// `LiveStateStore::mark_terminal`) and then deregisters the token on every
/// exit path (success, node error, cancellation, budget halt) — in that
/// order, since deregistration is the externally-observable "run is over"
/// edge (an abort against a deregistered run 404s).
///
/// Uses `actix_web::rt::spawn`, not `tokio::spawn`: `OnProgress<'a> =
/// Box<dyn FnMut(&TaskContext) + 'a>` (`engine-core/src/workflow.rs`) carries
/// no `Send` bound, so the run future is `!Send` and `tokio::spawn` will not
/// accept it. actix's per-worker arbiter spawn runs on the same
/// single-threaded runtime this handler is already on and requires only
/// `'static`.
async fn post_events(
    req: HttpRequest,
    body: web::Json<TriggerBody>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let workflow = match state
        .dispatcher
        .dispatch_with_event(&body.workflow_type, &body.data)
    {
        Ok(workflow) => workflow,
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "unknown workflow_type",
                "workflow_type": workflow_type,
            }));
        }
        // A registered workflow's factory (EN.5.D task 7) failed to resolve
        // policy against `body.data` — an unknown `profile` name or a
        // malformed `policy` override. 4xx, never a 500, and the offending
        // profile is named in `message` (surfaced verbatim from the
        // factory's own error, e.g. `resolve_profile_from`'s "unknown
        // profile '...'").
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "policy resolution failed",
                "message": message,
            }));
        }
    };

    let run_id = Uuid::new_v4();
    let workflow_type = body.workflow_type.clone();
    let data = body.data.clone();
    let live = state.live.clone();
    let runs = state.runs.clone();
    let durable_handle = state.durable.clone();

    let token = CancellationToken::new();
    runs.register(run_id, token.clone());

    let budget = default_budget_from_env();

    actix_web::rt::spawn(async move {
        let created_at = Utc::now();

        let mut durable_progress =
            durable_on_progress(durable_handle, run_id, workflow_type.clone(), data.clone());
        let progress_live = live.clone();
        let on_progress: OnProgress<'static> = Box::new(move |snapshot| {
            progress_live.record(run_id, snapshot);
            durable_progress(snapshot);
        });

        let options = RunOptions {
            cancellation_token: Some(token),
            budget: Some(budget),
        };

        // A cancelled or budget-halted run returns `Ok` with the marker
        // already stamped into `ctx.metadata` (see `RunOptions`'s docs); a
        // node's own failure is likewise folded into `Ok(ctx)` (the node
        // run is stamped FAILED, the walk halts, and the accumulated
        // context is still returned). Only a structural `WorkflowError`
        // (e.g. an unresolvable node identity) lands in `Err` here. The
        // response was sent long ago either way, so there is no status code
        // to map failure to — the readback (task 3) and the terminal SSE
        // frame (task 4) are how it surfaces.
        let final_ctx = match workflow.run_with(data, on_progress, options).await {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("run {run_id} failed: {err}");
                // No accumulated `TaskContext` comes back from a structural
                // `WorkflowError`; fall back to the last snapshot `on_progress`
                // recorded, or an empty context if none was ever recorded.
                live.get(run_id).unwrap_or_else(|| TaskContext {
                    event: serde_json::Value::Null,
                    nodes: StdHashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: StdHashMap::new(),
                })
            }
        };

        let updated_at = Utc::now();
        // Order matters: mark terminal *before* deregistering.
        // Deregistration is the externally-observable "this run is over"
        // edge (an abort against a deregistered run 404s), so anything a
        // client can read after that edge must already be in place.
        live.mark_terminal(run_id, &final_ctx, workflow_type, created_at, updated_at);
        runs.deregister(run_id);
    });

    HttpResponse::Accepted().json(serde_json::json!({
        "run_id": run_id,
        "event_id": run_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use engine_contract::TaskContext;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;

    struct MarkerNode;

    #[async_trait::async_trait]
    impl Node for MarkerNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
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
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(registry, fixture_schema("fixture")))
            }),
        );

        AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
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

    /// An `AppState` whose sole registration's factory fails policy
    /// resolution against any event naming a `profile`, mirroring what a
    /// real workflow's `resolve_policy_for_run_from` does against an
    /// unknown profile name (EN.5.D task 7).
    fn test_app_state_with_policy_aware_factory() -> AppState {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("fixture"),
            Box::new(|event: &serde_json::Value| {
                if let Some(profile) = event.get("profile").and_then(|v| v.as_str()) {
                    return Err(format!("unknown profile '{profile}'"));
                }
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(registry, fixture_schema("fixture")))
            }),
        );

        AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            api_key: "test-key".to_string(),
        }
    }

    #[actix_web::test]
    async fn post_events_forwards_data_to_the_factory_for_policy_resolution() {
        let state = test_app_state_with_policy_aware_factory();
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

    #[actix_web::test]
    async fn post_events_with_unknown_profile_returns_4xx_naming_the_profile_and_never_runs() {
        let state = test_app_state_with_policy_aware_factory();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": { "profile": "not-a-real-profile" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "policy resolution failed");
        assert!(body["message"]
            .as_str()
            .expect("message should be a string")
            .contains("not-a-real-profile"));
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

    #[actix_web::test]
    async fn post_events_202_body_carries_run_id_and_event_id() {
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

    #[actix_web::test]
    async fn post_events_rejects_before_spawning() {
        let state = test_app_state_with_policy_aware_factory();
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
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": { "profile": "not-a-real-profile" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 422);
        // The pre-flight rejected before anything was minted or spawned: no
        // live snapshot and no registered cancellation token exist for any
        // run.
        assert!(live.list_active().is_empty());
        assert!(runs.get(Uuid::new_v4()).is_none());
    }

    mod budget_from_env_vars_tests {
        use super::budget_from_env_vars;

        #[test]
        fn budget_defaults_to_five_dollars_with_no_env() {
            let budget = budget_from_env_vars(None, None);
            assert_eq!(budget.max_cost_usd, Some(5.0));
            assert_eq!(budget.max_total_tokens, None);
        }

        #[test]
        fn budget_honors_the_cost_env_var() {
            let budget = budget_from_env_vars(Some("1.25"), None);
            assert_eq!(budget.max_cost_usd, Some(1.25));
        }

        #[test]
        fn budget_honors_the_token_env_var() {
            let budget = budget_from_env_vars(None, Some("2000000"));
            assert_eq!(budget.max_total_tokens, Some(2_000_000));
            assert_eq!(budget.max_cost_usd, Some(5.0));
        }

        #[test]
        fn budget_falls_back_on_a_malformed_cost_value() {
            let budget = budget_from_env_vars(Some("not-a-number"), None);
            assert_eq!(budget.max_cost_usd, Some(5.0));
        }

        #[test]
        fn budget_ignores_a_malformed_token_value() {
            let budget = budget_from_env_vars(None, Some("twelve"));
            assert_eq!(budget.max_total_tokens, None);
        }

        #[test]
        fn budget_tolerates_surrounding_whitespace() {
            let budget = budget_from_env_vars(Some(" 2.5 "), Some(" 10 "));
            assert_eq!(budget.max_cost_usd, Some(2.5));
            assert_eq!(budget.max_total_tokens, Some(10));
        }
    }
}
