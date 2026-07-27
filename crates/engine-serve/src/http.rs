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
//! - `GET /events/{event_id}` — requires a valid `X-API-Key` header (401
//!   without it); `404` for an unknown id or a malformed/non-UUID path
//!   segment (never a `500`). Returns `200 {event_id, workflow_type, status,
//!   created_at, updated_at, task_context}` — the canonical readback shape.
//!   `status` is derived server-side (see [`derive_terminal_status`]) from
//!   whether the run is still live, and from the `metadata.cancellation` /
//!   `metadata.budget` / `metadata.failure` annotations and `node_runs`
//!   statuses once terminal. Serves only from [`crate::live_state::LiveStateStore`]
//!   — no Postgres access on this path.
//! - `GET /health` — 200.
//! - `GET /workflows` — the list of registered workflow types.
//! - `GET /workflows/{type}/graph` — the schema/graph for a registered type,
//!   404 for an unknown one.
//! - `POST /events/{run_id}/abort` — requires the same `X-API-Key` header
//!   (401 without it); 404 for an unknown/finished `run_id`; otherwise
//!   triggers that run's `CancellationToken` and returns 202 (task 5, see
//!   `crate::abort`).
//! - `GET /events/{event_id}/stream` — requires the same `X-API-Key` header
//!   (401 without it); 404 for an unknown/malformed id. Serves
//!   `text/event-stream`: one frame per node transition plus a terminal
//!   frame, then closes — including for a run that is already terminal by
//!   the time the client connects. Engine-rs-only extension, not part of the
//!   canonical data contract (see `crate::stream`).

use std::collections::HashMap as StdHashMap;
use std::sync::{Arc, OnceLock, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{
    Budget, CancellationToken, OnProgress, RunOptions, BUDGET_METADATA_KEY,
    CANCELLATION_METADATA_KEY,
};
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
        .route("/events/{event_id}", web::get().to(get_event))
        .route(
            "/events/{run_id}/abort",
            web::post().to(crate::abort::abort_run),
        )
        .route(
            "/events/{event_id}/stream",
            web::get().to(crate::stream::stream_event),
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

/// `(workflow_type, created_at)` for a run that is still live — everything
/// the `GET /events/{event_id}` readback (below) needs for a running run
/// beyond the `TaskContext` snapshot `LiveStateStore` already carries.
/// `LiveStateStore::record` only stores the snapshot, and `mark_terminal` is
/// the first place `workflow_type`/`created_at` get attached (task 1's
/// `RunRecord`) — so a *running* run's readback needs this side table.
type RunMetadata = (String, DateTime<Utc>);

/// Same not-an-`AppState`-field trick as [`default_budget_from_env`]: a
/// process-global map keyed by `run_id`, populated by `post_events` right
/// after minting the id (before spawning) and cleared by the spawned task's
/// cleanup once the run goes terminal (task 1's retained `RunRecord` takes
/// over from there).
fn live_run_metadata() -> &'static RwLock<StdHashMap<Uuid, RunMetadata>> {
    static LIVE_RUN_METADATA: OnceLock<RwLock<StdHashMap<Uuid, RunMetadata>>> = OnceLock::new();
    LIVE_RUN_METADATA.get_or_init(|| RwLock::new(StdHashMap::new()))
}

/// The server-derived `status` string for the `GET /events/{event_id}`
/// readback (contract: `{event_id, workflow_type, status, created_at,
/// updated_at, task_context}`, `status` derived server-side).
///
/// Derivation table, checked in order against a **terminal** run's retained
/// snapshot (a non-terminal run short-circuits to `"running"` before this is
/// ever called — see [`get_event`]):
///
/// | condition                                                            | status          |
/// |-----------------------------------------------------------------------|-----------------|
/// | `metadata.cancellation.cancelled == true` (`CANCELLATION_METADATA_KEY`, `engine_core::stamp_cancelled`) | `cancelled` |
/// | `metadata.budget.halted == true` (`BUDGET_METADATA_KEY`, `engine_core::workflow::stamp_budget_halt`) | `budget_halted` |
/// | `metadata.failure.failed == true` (contract v1.2.0's `metadata.failure`, not currently stamped by engine-rs, checked defensively), or any `node_runs[..].status == NodeRunStatus::Failed` | `failed` |
/// | none of the above                                                    | `succeeded`     |
pub(crate) fn derive_terminal_status(snapshot: &TaskContext) -> &'static str {
    let cancelled = snapshot
        .metadata
        .get(CANCELLATION_METADATA_KEY)
        .and_then(|v| v.get("cancelled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if cancelled {
        return "cancelled";
    }

    let budget_halted = snapshot
        .metadata
        .get(BUDGET_METADATA_KEY)
        .and_then(|v| v.get("halted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if budget_halted {
        return "budget_halted";
    }

    let failure_marker = snapshot
        .metadata
        .get("failure")
        .and_then(|v| v.get("failed"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let any_node_failed = snapshot
        .node_runs
        .values()
        .any(|node_run| node_run.status == NodeRunStatus::Failed);
    if failure_marker || any_node_failed {
        return "failed";
    }

    "succeeded"
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

    let created_at = Utc::now();
    live_run_metadata()
        .write()
        .expect("live run metadata lock poisoned on write")
        .insert(run_id, (workflow_type.clone(), created_at));

    actix_web::rt::spawn(async move {
        let mut durable_progress =
            durable_on_progress(durable_handle, run_id, workflow_type.clone(), data.clone());
        let progress_live = live.clone();
        let on_progress: OnProgress<'static> = Box::new(move |snapshot| {
            progress_live.record(run_id, snapshot);
            durable_progress(snapshot);
            // Third fan-out alongside live-state and the durable writer —
            // not a second progress mechanism (task 4).
            crate::stream::publish(run_id, snapshot);
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
        // Publish the SSE terminal frame before marking the readback
        // terminal, so a client racing the two never sees a terminal
        // readback with no terminal frame having gone out yet.
        crate::stream::publish_terminal(run_id, &final_ctx);
        // Order matters: mark terminal *before* deregistering.
        // Deregistration is the externally-observable "this run is over"
        // edge (an abort against a deregistered run 404s), so anything a
        // client can read after that edge must already be in place.
        live.mark_terminal(run_id, &final_ctx, workflow_type, created_at, updated_at);
        live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned on write")
            .remove(&run_id);
        runs.deregister(run_id);
    });

    HttpResponse::Accepted().json(serde_json::json!({
        "run_id": run_id,
        "event_id": run_id,
    }))
}

/// `GET /events/{event_id}` — the canonical readback (contract § HTTP
/// surface parity): `200 {event_id, workflow_type, status, created_at,
/// updated_at, task_context}`, `status` derived server-side (see
/// [`derive_terminal_status`]). `X-API-Key` gated (401 without); `404` for
/// an unknown id **and** for a malformed/non-UUID path segment — never a
/// `500`.
///
/// Serves only from task 1's [`LiveStateStore`] — a terminal run from the
/// retained completed ring, a still-live run from the live map (paired with
/// [`live_run_metadata`] for the `workflow_type`/`created_at` a live run
/// hasn't recorded into a `RunRecord` yet). **No Postgres fallback** — CI has
/// no `DATABASE_URL` and this route must stay DB-free.
async fn get_event(
    path: web::Path<String>,
    req: HttpRequest,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let raw_id = path.into_inner();
    let event_id = match Uuid::parse_str(&raw_id) {
        Ok(id) => id,
        Err(_) => {
            return HttpResponse::NotFound()
                .json(serde_json::json!({ "error": "unknown or malformed event_id" }));
        }
    };

    // Terminal first: once a run is marked terminal it moves out of the live
    // map entirely (`LiveStateStore::mark_terminal`), so a retained record
    // always means "this is the terminal readback", never a stale race
    // against a still-live snapshot.
    if let Some(record) = state.live.get_record(event_id) {
        let status = derive_terminal_status(&record.snapshot);
        return HttpResponse::Ok().json(serde_json::json!({
            "event_id": event_id,
            "workflow_type": record.workflow_type,
            "status": status,
            "created_at": record.created_at,
            "updated_at": record.updated_at,
            "task_context": record.snapshot,
        }));
    }

    if let Some(snapshot) = state.live.get(event_id) {
        let (workflow_type, created_at) = live_run_metadata()
            .read()
            .expect("live run metadata lock poisoned on read")
            .get(&event_id)
            .cloned()
            .unwrap_or_else(|| ("unknown".to_string(), Utc::now()));
        return HttpResponse::Ok().json(serde_json::json!({
            "event_id": event_id,
            "workflow_type": workflow_type,
            "status": "running",
            "created_at": created_at,
            "updated_at": Utc::now(),
            "task_context": snapshot,
        }));
    }

    HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or malformed event_id" }))
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

    /// Fixture node that blocks in `process` until `release` is notified —
    /// gives a test a window to read `GET /events/{event_id}` while the run
    /// is still live, before letting it complete. Mirrors
    /// `abort_integration.rs`'s `WaitNode`.
    struct WaitNode {
        release: std::sync::Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Node for WaitNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            self.release.notified().await;
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "WaitNode"
        }
    }

    fn wait_fixture_schema() -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "WaitNode".to_string(),
            engine_core::NodeConfig::new("WaitNode", vec![]),
        );
        WorkflowSchema::new("wait-fixture", "WaitNode", nodes)
    }

    /// An `AppState` whose sole registration is a single-node `WaitNode`
    /// workflow, plus the `Notify` used to release it.
    fn test_app_state_with_wait_node() -> (AppState, std::sync::Arc<tokio::sync::Notify>) {
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let release_for_factory = release.clone();

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            wait_fixture_schema(),
            Box::new(move |_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(WaitNode {
                    release: release_for_factory.clone(),
                }));
                Ok(Workflow::new(registry, wait_fixture_schema()))
            }),
        );

        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            api_key: "test-key".to_string(),
        };
        (state, release)
    }

    #[actix_web::test]
    async fn get_event_without_key_is_rejected() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/events/{}", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn get_event_unknown_id_returns_404() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/events/{}", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn get_event_malformed_id_returns_404_not_500() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/not-a-uuid")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn get_event_reads_running_then_terminal_for_the_same_run() {
        let (state, release) = test_app_state_with_wait_node();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "wait-fixture", "data": {} }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let event_id = trigger_body["event_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("event_id should be a parseable UUID");

        // The spawned task's initial `on_progress` snapshot (RUNNING,
        // recorded before `WaitNode::process` ever blocks on `release`) may
        // not have been polled yet at this point — `POST /events/` returns
        // as soon as the run is spawned, not once it starts — so poll until
        // the readback observes it, same as `abort_integration.rs`'s
        // `live.list_active()` loop.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mid_run_req = test::TestRequest::get()
                .uri(&format!("/events/{event_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let mid_run_resp = test::call_service(&app, mid_run_req).await;
            if mid_run_resp.status() == 200 {
                let mid_run_body: serde_json::Value = test::read_body_json(mid_run_resp).await;
                if mid_run_body["status"] == "running" {
                    assert_eq!(mid_run_body["event_id"], event_id.to_string());
                    assert_eq!(mid_run_body["workflow_type"], "wait-fixture");
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never reached a running status readback"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        release.notify_one();
        // Give the spawned task a chance to run to completion and mark
        // itself terminal before the next readback.
        for _ in 0..200 {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{event_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] == "succeeded" {
                assert_eq!(poll_body["event_id"], event_id.to_string());
                assert_eq!(poll_body["workflow_type"], "wait-fixture");
                assert!(poll_body.get("task_context").is_some());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("run never reached a terminal status");
    }

    #[actix_web::test]
    async fn stream_event_without_key_is_rejected() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/events/{}/stream", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn stream_event_unknown_id_returns_404() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/events/{}/stream", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn stream_event_malformed_id_returns_404_not_500() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/not-a-uuid/stream")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    /// End-to-end: trigger a run, connect to its SSE stream before it
    /// completes, release the blocked node, and confirm the collected body
    /// carries at least one `"running"` frame followed by a `"terminal":
    /// true` frame, served as `text/event-stream`.
    #[actix_web::test]
    async fn stream_event_delivers_running_then_terminal_frames() {
        let (state, release) = test_app_state_with_wait_node();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "wait-fixture", "data": {} }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let event_id = trigger_body["event_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("event_id should be a parseable UUID");

        // `GET /events/{event_id}/stream` 404s until `state.live` has
        // *some* record of the run (the spawned task's first `on_progress`
        // call), which — same as `get_event_reads_running_then_terminal_..`
        // above — may not have been polled yet right after `POST /events/`
        // returns. Poll until it connects.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stream_resp = loop {
            let stream_req = test::TestRequest::get()
                .uri(&format!("/events/{event_id}/stream"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let resp = test::call_service(&app, stream_req).await;
            if resp.status() == 200 {
                break resp;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "stream endpoint never connected for the live run"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert_eq!(stream_resp.status(), 200);
        assert_eq!(
            stream_resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );

        // Release the blocked node concurrently with draining the stream
        // body — the body future won't resolve until the stream ends
        // (terminal frame published + sender dropped), which only happens
        // once the run completes.
        let release_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            release.notify_one();
        });
        let body_bytes = test::read_body(stream_resp).await;
        release_handle.await.expect("release task should not panic");

        // The connection is a second HTTP request racing the spawned task's
        // very first `on_progress` call, so whether an intermediate
        // `"running"` frame lands ahead of the subscribe is inherently
        // timing-dependent (covered deterministically by `stream.rs`'s
        // publish/subscribe unit tests instead). What every timing must
        // still guarantee is that the connection sees a terminal frame and
        // then closes.
        let body = String::from_utf8(body_bytes.to_vec()).expect("SSE body should be UTF-8");
        assert!(
            body.contains("\"terminal\":true"),
            "expected a terminal frame, got: {body}"
        );
        assert!(
            body.contains("\"status\":\"succeeded\""),
            "expected the terminal frame's status to be succeeded, got: {body}"
        );
    }

    mod derive_terminal_status_tests {
        use super::{derive_terminal_status, NodeRunStatus};
        use engine_contract::{NodeRun, TaskContext};
        use std::collections::HashMap as StdHashMap;

        fn empty_context(metadata: serde_json::Value) -> TaskContext {
            TaskContext {
                event: serde_json::Value::Null,
                nodes: StdHashMap::new(),
                metadata,
                node_runs: StdHashMap::new(),
            }
        }

        #[test]
        fn succeeded_when_no_markers_and_no_failed_nodes() {
            let ctx = empty_context(serde_json::json!({}));
            assert_eq!(derive_terminal_status(&ctx), "succeeded");
        }

        #[test]
        fn cancelled_when_cancellation_marker_is_set() {
            let ctx = empty_context(serde_json::json!({
                "cancellation": { "cancelled": true, "at": "2026-01-01T00:00:00Z" }
            }));
            assert_eq!(derive_terminal_status(&ctx), "cancelled");
        }

        #[test]
        fn budget_halted_when_budget_marker_is_set() {
            let ctx = empty_context(serde_json::json!({
                "budget": { "halted": true, "reason": { "cap": "cost" } }
            }));
            assert_eq!(derive_terminal_status(&ctx), "budget_halted");
        }

        #[test]
        fn failed_when_failure_marker_is_set() {
            let ctx = empty_context(serde_json::json!({
                "failure": { "failed": true, "error": "boom", "at": "2026-01-01T00:00:00Z" }
            }));
            assert_eq!(derive_terminal_status(&ctx), "failed");
        }

        #[test]
        fn failed_when_any_node_run_failed() {
            let mut ctx = empty_context(serde_json::json!({}));
            ctx.node_runs.insert(
                "SomeNode".to_string(),
                NodeRun {
                    status: NodeRunStatus::Failed,
                    started_at: None,
                    completed_at: None,
                    error: Some("boom".to_string()),
                    input: None,
                    usage: None,
                },
            );
            assert_eq!(derive_terminal_status(&ctx), "failed");
        }

        #[test]
        fn cancellation_takes_precedence_over_budget_and_failure() {
            let ctx = empty_context(serde_json::json!({
                "cancellation": { "cancelled": true, "at": "2026-01-01T00:00:00Z" },
                "budget": { "halted": true },
                "failure": { "failed": true }
            }));
            assert_eq!(derive_terminal_status(&ctx), "cancelled");
        }
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
