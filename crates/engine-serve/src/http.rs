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
//! - `POST /webhooks/email/inbound` (`EN.6.B`) — requires the same
//!   `X-API-Key` header (401 without it); parses a Resend inbound-mail
//!   payload into an `IngressEnvelope` (400 naming the missing field on a
//!   malformed payload, dispatching nothing) and dispatches one
//!   `CONTENT_PIPELINE` run carrying it, returning `202 {run_id, event_id,
//!   envelope_id}`. See `crate::email_webhooks`.
//! - `POST /webhooks/email/events` (`EN.6.B`) — requires the same
//!   `X-API-Key` header (401 without it); maps a Resend delivery/bounce
//!   webhook payload onto `EN.7.B`'s `OPPORTUNITY_ADD_ACTION` workflow via
//!   the `opportunity_slug` tag echoed back on send. A correlated event
//!   dispatches one run and returns `202 {run_id, event_id}`; an untagged or
//!   unrecognized event is explicitly skipped — `202 {skipped: true,
//!   reason}` — with nothing dispatched; a structurally malformed payload is
//!   `400`. Both routes defer Svix signature verification — see
//!   `crate::email_webhooks`'s module doc.

use std::collections::HashMap as StdHashMap;
use std::sync::{Arc, OnceLock, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{
    Budget, CancellationToken, PauseSignal, BUDGET_METADATA_KEY, CANCELLATION_METADATA_KEY,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::abort::RunRegistry;
use crate::dispatch::{DispatchError, Dispatcher};
use crate::durable::DurableHandle;
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
        // MUST be registered before `/events/{event_id}` -- actix-web
        // resolves routes first-registration-wins, so the literal
        // "suspended" segment would otherwise be swallowed by the
        // `{event_id}` uuid extractor.
        .route(
            "/events/suspended",
            web::get().to(crate::resume::list_suspended),
        )
        .route("/events/{event_id}", web::get().to(get_event))
        .route(
            "/events/{run_id}/abort",
            web::post().to(crate::abort::abort_run),
        )
        .route(
            "/events/{run_id}/pause",
            web::post().to(crate::resume::pause_run),
        )
        .route(
            "/events/{event_id}/resume",
            web::post().to(crate::resume::resume_run),
        )
        .route(
            "/events/{event_id}/stream",
            web::get().to(crate::stream::stream_event),
        )
        .route(
            "/webhooks/email/inbound",
            web::post().to(crate::email_webhooks::inbound_email),
        )
        .route(
            "/webhooks/email/events",
            web::post().to(crate::email_webhooks::email_delivery_events),
        )
        // MUST be registered before `/approvals/ledger` -- actix-web
        // resolves routes first-registration-wins. Neither literal path
        // contains a dynamic segment today, so they cannot actually shadow
        // each other yet, but registering `stats` first keeps a future
        // `/approvals/{item_id}`-style pattern from silently swallowing it.
        .route(
            "/approvals/ledger/stats",
            web::get().to(crate::approvals::ledger_stats),
        )
        .route(
            "/approvals/ledger",
            web::get().to(crate::approvals::list_ledger),
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
pub(crate) fn default_budget_from_env() -> Budget {
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
pub(crate) fn live_run_metadata() -> &'static RwLock<StdHashMap<Uuid, RunMetadata>> {
    static LIVE_RUN_METADATA: OnceLock<RwLock<StdHashMap<Uuid, RunMetadata>>> = OnceLock::new();
    LIVE_RUN_METADATA.get_or_init(|| RwLock::new(StdHashMap::new()))
}

/// Public read accessor onto [`live_run_metadata`] for embedded consumers
/// (e.g. `bastion serve`, mounted in-process per D48) that need a live
/// (non-terminal) run's `workflow_type`/`created_at` without reaching into
/// `engine-serve`-private internals. Returns `None` for a run that is not
/// currently tracked as live (unknown `run_id`, or a run that has already
/// gone terminal and been cleared from this side table).
///
/// `GET /events/{event_id}` (see [`get_event`]) consumes this same accessor
/// rather than reading [`live_run_metadata`] inline, so there is exactly one
/// code path producing this data.
pub fn live_run_workflow_type(run_id: Uuid) -> Option<(String, DateTime<Utc>)> {
    live_run_metadata()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&run_id)
        .cloned()
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
/// An empty `TaskContext`, used as the fallback when a run fails or panics
/// before `on_progress` ever recorded a snapshot to fall back to.
pub(crate) fn empty_task_context() -> TaskContext {
    TaskContext {
        event: serde_json::Value::Null,
        nodes: StdHashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: StdHashMap::new(),
    }
}

/// Stamp `ctx.metadata.failure = { failed: true, error: message }` so
/// [`derive_terminal_status`] reports `"failed"` instead of falling through
/// to its `"succeeded"` default.
pub(crate) fn stamp_failure(ctx: &mut TaskContext, message: &str) {
    let failure = serde_json::json!({ "failed": true, "error": message });
    match ctx.metadata.as_object_mut() {
        Some(metadata) => {
            metadata.insert("failure".to_string(), failure);
        }
        None => ctx.metadata = serde_json::json!({ "failure": failure }),
    }
}

/// Extract a human-readable message from a `catch_unwind` panic payload —
/// the two shapes `std::panic!`/`.unwrap()`/`.expect()` actually produce
/// (`&str` for a literal, `String` for a formatted message), falling back to
/// a placeholder for anything else (a custom payload type via
/// `std::panic::panic_any`).
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

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

/// Status for a run that is still in the live map. `derive_terminal_status`
/// is deliberately NOT extended: a suspended run is not terminal, and that
/// function is only reached for the completed ring and `publish_terminal`.
pub(crate) fn derive_live_status(snapshot: &TaskContext) -> &'static str {
    if engine_core::suspend::is_suspended(&snapshot.metadata) {
        "suspended"
    } else {
        "running"
    }
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
///
/// EN.6.J task 5: the `run_id` minted here now reaches the workflow itself,
/// not just the surrounding bookkeeping — `spawn_run` passes it through
/// `engine_core::RunOptions { run_id: Some(run_id), .. }`, and
/// `Workflow::run_with`/`run_from` (EN.6.J task 2) stamp it into
/// `ctx.metadata` before the first node dispatches. That is what makes a
/// flow's committed `sdlc-flow-state.json` joinable back to the engine run
/// that produced it.
///
/// EN.3.K task 5: for `workflow_type: "SDLC_FLOW"` two additional 422
/// conditions are checked **before** `run_id` is minted (so a rejection
/// mints no run id, registers no token, and leaves no live-state entry to
/// clean up): an unknown `repo` slug (`{"error": "unknown repo", "repo":
/// ..}`, resolved via the process-global `crate::workflows::repo_registry()`
/// installed by task 4), and a `spec_slug` whose directory does not exist
/// under the resolved target root (`{"error": "unknown spec_slug",
/// "spec_slug": ..}`). A spec directory that exists but has no `tasks.json`
/// is NOT rejected — that is the legitimate `GenerateTasksNode` path. The
/// `202 {run_id, event_id}` contract (EN.5.F) is unchanged for every valid
/// request, `repo`-bearing or not.
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

    // EN.3.K task 5: pre-flight dispatch-target validation for SDLC_FLOW,
    // run before a run id is minted / spawn_run is called — no run id, no
    // token, no live-state entry to clean up on rejection. Gated on
    // workflow_type so it never touches the channel/API-shaped workflows.
    if body.workflow_type == "SDLC_FLOW" {
        let repo_slug = body.data.get("repo").and_then(|v| v.as_str());

        // Check 1 — the repo slug names a registry entry (or is absent,
        // which resolves to current_dir(), exactly as `resolve_target_root`
        // does for every other SDLC_FLOW node).
        let resolved_root = match repo_slug {
            Some(slug) => match crate::workflows::repo_registry() {
                Some(registry) => match registry.resolve(slug) {
                    Ok(root) => root,
                    Err(err) => {
                        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                            "error": "unknown repo",
                            "repo": slug,
                            "message": err.to_string(),
                        }));
                    }
                },
                None => {
                    return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                        "error": "unknown repo",
                        "repo": slug,
                        "message": format!(
                            "SDLC_FLOW event named repo slug '{slug}' but no repo registry is \
                             available to resolve it"
                        ),
                    }));
                }
            },
            None => match std::env::current_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                        "error": "unresolvable target root",
                        "message": err.to_string(),
                    }));
                }
            },
        };

        // Check 2 — the spec directory exists under the resolved root.
        // Deliberately `dir.exists()`, NEVER `dir.join("tasks.json").exists()`
        // — a spec dir that exists without a `tasks.json` is the legitimate
        // `GenerateTasksNode` path (SpecExistsRouterNode routes it there);
        // only an absent directory is the defect this closes.
        if let Some(spec_slug) = body.data.get("spec_slug").and_then(|v| v.as_str()) {
            let spec_dir = resolved_root.join("planning").join(spec_slug);
            if !spec_dir.is_dir() {
                return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                    "error": "unknown spec_slug",
                    "spec_slug": spec_slug,
                    "message": format!(
                        "spec directory '{}' does not exist",
                        spec_dir.display()
                    ),
                }));
            }
        }
    }

    let run_id = Uuid::new_v4();
    let workflow_type = body.workflow_type.clone();
    let data = body.data.clone();
    let live = state.live.clone();
    let runs = state.runs.clone();
    let durable_handle = state.durable.clone();

    let token = CancellationToken::new();
    runs.register(run_id, token.clone());

    // Registered alongside the cancellation token so `Workflow::walk`'s
    // operator-pause check (EN.6.F task 4) has a signal to consult for this
    // run from the first node boundary onward, and `POST
    // /events/{run_id}/pause` (task 11) has somewhere to find it.
    let pause = PauseSignal::new();
    crate::suspend::register_pause_signal(run_id, pause.clone());

    let budget = default_budget_from_env();

    let created_at = Utc::now();
    live_run_metadata()
        .write()
        .expect("live run metadata lock poisoned on write")
        .insert(run_id, (workflow_type.clone(), created_at));

    crate::suspend::spawn_run(crate::suspend::SpawnedRun {
        run_id,
        workflow,
        workflow_type,
        data: data.clone(),
        created_at,
        start: crate::suspend::RunStart::Fresh(data),
        live,
        durable: durable_handle,
        runs,
        token,
        pause,
        budget,
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
        let (workflow_type, created_at) =
            live_run_workflow_type(event_id).unwrap_or_else(|| ("unknown".to_string(), Utc::now()));
        let status = derive_live_status(&snapshot);
        return HttpResponse::Ok().json(serde_json::json!({
            "event_id": event_id,
            "workflow_type": workflow_type,
            "status": status,
            "created_at": created_at,
            "updated_at": Utc::now(),
            "task_context": snapshot,
        }));
    }

    // Registered by `post_events` before the run is spawned, but the run
    // only gets its first `LiveStateStore` snapshot once `on_progress` fires
    // at the first node boundary — a poll landing in that window must still
    // read back "running", not 404 a run_id the client was just handed.
    if let Some((workflow_type, created_at)) = live_run_workflow_type(event_id) {
        return HttpResponse::Ok().json(serde_json::json!({
            "event_id": event_id,
            "workflow_type": workflow_type,
            "status": "running",
            "created_at": created_at,
            "updated_at": Utc::now(),
            "task_context": TaskContext {
                event: serde_json::Value::Null,
                nodes: StdHashMap::new(),
                metadata: serde_json::json!({}),
                node_runs: StdHashMap::new(),
            },
        }));
    }

    HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or malformed event_id" }))
}

#[cfg(test)]
// `registry_test_lock()`'s std `MutexGuard` is held across `.await` points by design — it
// serializes tests that share the global suspend registry, not data an async task contends
// over concurrently, so the guard's lifetime spanning the whole test body is intentional.
#[allow(clippy::await_holding_lock)]
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

    // --- EN.3.K task 5: pre-flight repo/spec_slug validation on SDLC_FLOW ---

    /// Guards the process-global repo registry (`crate::workflows::set_repo_registry`)
    /// and `std::env::current_dir()` so these tests never race others in the
    /// same nextest process (there aren't any today, but this makes the
    /// intent explicit for future additions to this module).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An `AppState` whose sole registration is under the real `SDLC_FLOW`
    /// workflow_type name, so `post_events`'s `body.workflow_type ==
    /// "SDLC_FLOW"` gate fires. The registered factory is a harmless
    /// `MarkerNode` fixture — task 5 validates dispatch-target inputs, not
    /// the real SDLC_FLOW graph.
    fn test_app_state_with_sdlc_flow() -> AppState {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("SDLC_FLOW"),
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(registry, fixture_schema("SDLC_FLOW")))
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

    /// A tempdir "brain root" with a single `alpha` repo entry whose
    /// `planning/my-spec/` directory exists but — deliberately — carries no
    /// `tasks.json`, so it exercises the "exists but no tasks.json" carve-out
    /// as well as the "known repo" success path.
    fn tempdir_registry_with_alpha_spec_dir_no_tasks_json() -> (
        tempfile::TempDir,
        Arc<engine_core::repo_registry::RepoRegistry>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join("planning").join("my-spec"))
            .expect("mkdir alpha planning/my-spec");
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

    #[actix_web::test]
    async fn post_events_unknown_repo_slug_returns_422_naming_the_slug() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = crate::workflows::repo_registry();
        let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
        crate::workflows::set_repo_registry(registry);

        let state = test_app_state_with_sdlc_flow();
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
                "workflow_type": "SDLC_FLOW",
                "data": { "spec_slug": "my-spec", "repo": "not-a-real-repo" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["repo"], "not-a-real-repo");
        assert!(body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not-a-real-repo"));

        if let Some(prev) = previous {
            crate::workflows::set_repo_registry(prev);
        } else {
            crate::workflows::clear_repo_registry();
        }
    }

    #[actix_web::test]
    async fn post_events_known_repo_absent_spec_dir_returns_422_naming_the_spec_slug() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = crate::workflows::repo_registry();
        let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
        crate::workflows::set_repo_registry(registry);

        let state = test_app_state_with_sdlc_flow();
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
                "workflow_type": "SDLC_FLOW",
                "data": { "spec_slug": "does-not-exist", "repo": "alpha" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["spec_slug"], "does-not-exist");

        if let Some(prev) = previous {
            crate::workflows::set_repo_registry(prev);
        } else {
            crate::workflows::clear_repo_registry();
        }
    }

    #[actix_web::test]
    async fn post_events_known_repo_spec_dir_without_tasks_json_still_dispatches() {
        // The non-regression: SpecExistsRouterNode's "generate a plan"
        // path is legitimate and must not be rejected by this pre-flight.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = crate::workflows::repo_registry();
        let (_dir, registry) = tempdir_registry_with_alpha_spec_dir_no_tasks_json();
        crate::workflows::set_repo_registry(registry);

        let state = test_app_state_with_sdlc_flow();
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
                "workflow_type": "SDLC_FLOW",
                "data": { "spec_slug": "my-spec", "repo": "alpha" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["run_id"].is_string());

        if let Some(prev) = previous {
            crate::workflows::set_repo_registry(prev);
        } else {
            crate::workflows::clear_repo_registry();
        }
    }

    #[actix_web::test]
    async fn post_events_no_repo_field_behaves_as_before_when_spec_dir_exists() {
        // No `repo` field at all: the resolved root is `current_dir()`,
        // exactly as it was before this task. With a real spec dir present
        // under that cwd, the request dispatches (202) exactly as it did
        // before this pre-flight existed.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous_registry = crate::workflows::repo_registry();
        crate::workflows::clear_repo_registry();
        let previous_cwd = std::env::current_dir().expect("current_dir");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("planning").join("my-spec"))
            .expect("mkdir planning/my-spec");
        std::env::set_current_dir(dir.path()).expect("set_current_dir");

        let state = test_app_state_with_sdlc_flow();
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
                "workflow_type": "SDLC_FLOW",
                "data": { "spec_slug": "my-spec" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);

        std::env::set_current_dir(previous_cwd).expect("restore cwd");
        if let Some(prev) = previous_registry {
            crate::workflows::set_repo_registry(prev);
        }
    }

    #[actix_web::test]
    async fn post_events_non_sdlc_flow_workflow_untouched_by_new_validation() {
        // A `fixture`-type request with a garbage `spec_slug` and an
        // unknown `repo` must dispatch exactly as it did before this task
        // — the new pre-flight block is gated on `workflow_type ==
        // "SDLC_FLOW"` and must never fire for anything else.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = crate::workflows::repo_registry();
        crate::workflows::clear_repo_registry();

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
            .set_json(serde_json::json!({
                "workflow_type": "fixture",
                "data": { "spec_slug": "totally-bogus-spec", "repo": "totally-bogus-repo" },
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);

        if let Some(prev) = previous {
            crate::workflows::set_repo_registry(prev);
        }
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
    struct PanicNode;

    #[async_trait::async_trait]
    impl Node for PanicNode {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            panic!("PanicNode always panics — regression fixture for the catch_unwind guard");
        }

        fn name(&self) -> &str {
            "PanicNode"
        }
    }

    fn panic_fixture_schema() -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "PanicNode".to_string(),
            engine_core::NodeConfig::new("PanicNode", vec![]),
        );
        WorkflowSchema::new("panic-fixture", "PanicNode", nodes)
    }

    fn test_app_state_with_panic_node() -> AppState {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            panic_fixture_schema(),
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(PanicNode));
                Ok(Workflow::new(registry, panic_fixture_schema()))
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
    async fn a_panicking_node_reads_back_as_failed_instead_of_leaking_the_run_forever() {
        // Regression test for the confirmed-but-previously-unfixed gap: a
        // node panicking (as opposed to returning `Err`) used to abort the
        // spawned task before its cleanup ran, leaking the run in
        // `live_run_metadata()`/`RunRegistry` forever (`GET /events/{id}`
        // reporting "running" indefinitely, `POST /events/{id}/abort` never
        // 404ing). `catch_unwind` around `run_with` now guarantees cleanup
        // always runs.
        let state = test_app_state_with_panic_node();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "panic-fixture", "data": {} }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let event_id = trigger_body["event_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("event_id should be a parseable UUID");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{event_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] != "running" {
                assert_eq!(
                    poll_body["status"], "failed",
                    "a panicking node must read back as failed, not succeeded or stuck running"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never left \"running\" — the panic leaked the run instead of terminating it"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Cleanup must have fully run: an abort against a run that has
        // already gone terminal reads as unknown, proving
        // `RunRegistry::deregister` executed on the panic path too.
        let abort_req = test::TestRequest::post()
            .uri(&format!("/events/{event_id}/abort"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let abort_resp = test::call_service(&app, abort_req).await;
        assert_eq!(
            abort_resp.status(),
            404,
            "the cancellation token should have been deregistered once the panicked run went terminal"
        );
    }

    use crate::test_fixtures::WaitNode;

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
                registry.register(Box::new(WaitNode::new(release_for_factory.clone())));
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
    async fn get_event_is_never_404_immediately_after_trigger() {
        // Regression test for the early-poll 404: `GET /events/{event_id}`
        // used to consult only `LiveStateStore`, which has no entry for a
        // run until its first `on_progress` snapshot lands — a window that
        // spans at least one executor yield after `POST /events/` returns.
        // Poll with no sleep and no retry loop: the very next request must
        // already read back "running" via `live_run_metadata()`, whether or
        // not the spawned task has been polled yet.
        let (state, _release) = test_app_state_with_wait_node();
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

        let poll_req = test::TestRequest::get()
            .uri(&format!("/events/{event_id}"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let poll_resp = test::call_service(&app, poll_req).await;
        assert_eq!(
            poll_resp.status(),
            200,
            "a run_id just handed back by POST /events/ must never 404"
        );
        let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
        assert_eq!(poll_body["status"], "running");
        assert_eq!(poll_body["workflow_type"], "wait-fixture");
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

    fn unresolvable_fixture_schema(workflow_type: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "GhostNode".to_string(),
            engine_core::NodeConfig::new("GhostNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "GhostNode", nodes)
    }

    fn test_app_state_with_unresolvable_workflow() -> AppState {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            unresolvable_fixture_schema("ghost-fixture"),
            Box::new(|_event: &serde_json::Value| {
                // Deliberately empty registry: the schema's start node
                // "GhostNode" is never registered, so `run_with` returns a
                // structural `WorkflowError` instead of `Ok`.
                let registry = NodeRegistry::new();
                Ok(Workflow::new(
                    registry,
                    unresolvable_fixture_schema("ghost-fixture"),
                ))
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
    async fn a_structural_workflow_error_reads_back_as_failed_not_succeeded() {
        let state = test_app_state_with_unresolvable_workflow();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "ghost-fixture", "data": {} }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let event_id = trigger_body["event_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("event_id should be a parseable UUID");

        for _ in 0..200 {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{event_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] != "running" {
                assert_eq!(
                    poll_body["status"], "failed",
                    "a structural WorkflowError must read back as failed, not succeeded"
                );
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

    // -- EN.6.F task 10: spawn_run forks on suspension --------------------

    /// `SuspendNode(enabled: true) -> MarkerNode`: the walk suspends
    /// immediately after `SuspendNode` finishes, before `MarkerNode` ever
    /// runs, so `MarkerNode` is the resume pointer.
    fn suspend_fixture_schema(workflow_type: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "SuspendNode".to_string(),
            engine_core::NodeConfig::new("SuspendNode", vec!["MarkerNode".to_string()]),
        );
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "SuspendNode", nodes)
    }

    fn test_app_state_with_suspend_fixture() -> AppState {
        const WORKFLOW_TYPE: &str = "suspend-fixture";
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            suspend_fixture_schema(WORKFLOW_TYPE),
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(
                    engine_core::nodes::SuspendNode::new("SuspendNode").with_enabled(true),
                ));
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(
                    registry,
                    suspend_fixture_schema(WORKFLOW_TYPE),
                ))
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

    /// A suspended run's exit path (task 10's fork) diverges from the
    /// terminal path exactly as documented: it stays in the live map and in
    /// `live_run_metadata()`, lands in the suspended index with the correct
    /// `resume_at`, and still deregisters its cancellation token and pause
    /// signal even though the run itself is not over.
    #[actix_web::test]
    async fn a_suspended_run_stays_live_and_lands_in_the_suspended_index() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "suspend-fixture", "data": {} }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let run_id = body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        // Poll until the run lands in the suspended index — the spawned
        // task's fork may not have landed yet right after the 202 response.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let entry = loop {
            if let Some((_, entry)) = crate::suspend::list_suspended()
                .into_iter()
                .find(|(id, _)| *id == run_id)
            {
                break entry;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never landed in the suspended index"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };

        assert_eq!(entry.workflow_type, "suspend-fixture");
        assert_eq!(
            entry.resume_at, "MarkerNode",
            "resume_at should be the successor of SuspendNode, not re-derived"
        );
        assert!(
            !entry.resuming,
            "a freshly suspended run must not already be marked resuming"
        );

        // Still live: `GET /events/{id}` reads "suspended" (task 12's
        // `derive_live_status`), not a 404 or a terminal record —
        // `live_run_metadata()` was kept, not removed.
        let get_req = test::TestRequest::get()
            .uri(&format!("/events/{run_id}"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let get_resp = test::call_service(&app, get_req).await;
        assert_eq!(get_resp.status(), 200);
        let get_body: serde_json::Value = test::read_body_json(get_resp).await;
        assert_eq!(get_body["status"], "suspended");
        assert_eq!(get_body["workflow_type"], "suspend-fixture");

        // The pause signal was deregistered even though the run stayed
        // live — nobody is checking it while suspended.
        assert!(
            crate::suspend::get_pause_signal(run_id).is_none(),
            "pause signal should be removed on the suspended exit path"
        );

        // The cancellation token was likewise deregistered -- but a
        // suspended run must still be killable (EN.6.F task 13's "abort of
        // a suspended run" coverage), so `abort_run` (`crate::abort`) falls
        // back to the suspended index instead of 404ing on the missing
        // token.
        let abort_req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/abort"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let abort_resp = test::call_service(&app, abort_req).await;
        assert_eq!(
            abort_resp.status(),
            202,
            "a suspended run has no live token, but must still be killable via the suspended-index fallback"
        );

        assert!(
            crate::suspend::list_suspended()
                .into_iter()
                .all(|(id, _)| id != run_id),
            "aborting a suspended run must remove it from the suspended index"
        );

        let get_after_abort_req = test::TestRequest::get()
            .uri(&format!("/events/{run_id}"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let get_after_abort_resp = test::call_service(&app, get_after_abort_req).await;
        let get_after_abort_body: serde_json::Value =
            test::read_body_json(get_after_abort_resp).await;
        assert_eq!(get_after_abort_body["status"], "cancelled");
    }

    // -- EN.6.J task 5: run_id stamp + failure-path terminal write ---------

    #[actix_web::test]
    async fn a_completed_run_carries_its_run_id_in_context_metadata() {
        // `spawn_run` now sets `RunOptions { run_id: Some(run_id), .. }`
        // (task 5), and `Workflow::run_with` stamps it into `ctx.metadata`
        // (task 2) before the first node dispatches -- so even the plain
        // `MarkerNode` fixture (no SDLC-flow machinery at all) should read
        // back its own run_id once the walk completes.
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({ "workflow_type": "fixture", "data": {} }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let run_id = trigger_body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{run_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] != "running" {
                assert_eq!(poll_body["status"], "succeeded");
                assert_eq!(
                    poll_body["task_context"]["metadata"]["run_id"],
                    serde_json::json!(run_id.to_string()),
                    "the completed context's metadata should carry the run_id that produced it"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never left \"running\""
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Stamps the SDLC-flow shape a real `SetupWorktreeNode` /
    /// `UpdateTaskStatusNode` walk would leave behind (worktree path +
    /// `SDLCState` + resolved policy), so [`write_terminal_blocked_state`]
    /// and its `latest_state`/`worktree_path` readers can find something to
    /// terminate. Mirrors `wrap_up.rs`'s test-only `ctx_with_state` fixture.
    struct SeedSdlcContextNode {
        worktree: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl Node for SeedSdlcContextNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            use engine_core::workflows::sdlc_flow::policy::SdlcPolicy;
            use engine_core::workflows::sdlc_flow::schema::{SDLCState, SDLCTask, SDLCTaskStatus};

            ctx.nodes.insert(
                "SetupWorktreeNode".to_string(),
                serde_json::json!({ "worktree_path": self.worktree.to_string_lossy() }),
            );

            let mut sdlc_state = SDLCState::new("EN.6.J-suspend-fixture");
            let mut task = SDLCTask::new(1, "One", "d1");
            task.status = SDLCTaskStatus::Failed;
            sdlc_state.tasks.push(task);
            ctx.nodes.insert(
                "UpdateTaskStatusNode".to_string(),
                serde_json::to_value(&sdlc_state).expect("SDLCState serializes"),
            );
            ctx.nodes.insert(
                engine_core::policy::RESOLVED_POLICY_IDENTITY.to_string(),
                serde_json::to_value(SdlcPolicy::default()).expect("SdlcPolicy serializes"),
            );

            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SeedSdlcContextNode"
        }
    }

    struct AlwaysFailNode;

    #[async_trait::async_trait]
    impl Node for AlwaysFailNode {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Err(NodeError::new("boom"))
        }

        fn name(&self) -> &str {
            "AlwaysFailNode"
        }
    }

    fn sdlc_fail_fixture_schema() -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "SeedSdlcContextNode".to_string(),
            engine_core::NodeConfig::new("SeedSdlcContextNode", vec!["AlwaysFailNode".to_string()]),
        );
        nodes.insert(
            "AlwaysFailNode".to_string(),
            engine_core::NodeConfig::new("AlwaysFailNode", vec![]),
        );
        WorkflowSchema::new(
            engine_core::workflows::sdlc_flow::graph::WORKFLOW_TYPE,
            "SeedSdlcContextNode",
            nodes,
        )
    }

    fn temp_worktree_for_terminal_write_test() -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-serve-sdlc-flow-terminal-write-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see engine-core's `setup.rs` `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Seeds `<worktree>/planning/<spec_slug>/sdlc/sdlc-flow-state.json`
    /// exactly as a prior `SaveStateNode`/`WrapUpNode` write would --
    /// [`write_terminal_blocked_state`]'s state-dir-must-already-exist guard
    /// requires this before it will write anything.
    fn seed_sdlc_state_file(worktree: &std::path::Path) {
        use engine_core::workflows::sdlc_flow::schema::{RunMeta, SDLCState};

        let spec_slug = "EN.6.J-suspend-fixture";
        let state_dir = worktree.join("planning").join(spec_slug).join("sdlc");
        std::fs::create_dir_all(&state_dir).unwrap();

        let state = SDLCState::new(spec_slug);
        let run_meta = RunMeta {
            branch: "sdlc/x".to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            started_at: "2026-07-01T00:00:00Z".to_string(),
            updated_at: "2026-07-01T00:00:00Z".to_string(),
            run_id: None,
        };
        let committed = state.to_committed_state_json(&run_meta, None, None, None, None, None);
        let json = serde_json::to_string_pretty(&committed).unwrap();
        std::fs::write(state_dir.join("sdlc-flow-state.json"), json).unwrap();
    }

    fn read_sdlc_state_file(worktree: &std::path::Path) -> serde_json::Value {
        let path = worktree
            .join("planning")
            .join("EN.6.J-suspend-fixture")
            .join("sdlc")
            .join("sdlc-flow-state.json");
        let raw = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[actix_web::test]
    async fn a_failed_sdlc_flow_walk_leaves_a_blocked_terminal_status_on_disk() {
        let worktree = temp_worktree_for_terminal_write_test();
        seed_sdlc_state_file(&worktree);

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            sdlc_fail_fixture_schema(),
            Box::new({
                let worktree = worktree.clone();
                move |_event: &serde_json::Value| {
                    let mut registry = NodeRegistry::new();
                    registry.register(Box::new(SeedSdlcContextNode {
                        worktree: worktree.clone(),
                    }));
                    registry.register(Box::new(AlwaysFailNode));
                    Ok(Workflow::new(registry, sdlc_fail_fixture_schema()))
                }
            }),
        );
        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            api_key: "test-key".to_string(),
        };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({
                "workflow_type": engine_core::workflows::sdlc_flow::graph::WORKFLOW_TYPE,
                "data": {},
            }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let run_id = trigger_body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{run_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] != "running" {
                assert_eq!(poll_body["status"], "failed");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never left \"running\""
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let on_disk = read_sdlc_state_file(&worktree);
        assert_eq!(on_disk["status"], serde_json::json!("blocked"));
        assert!(
            on_disk["bail_reason"]
                .as_str()
                .expect("bail_reason should be a string")
                .contains("boom"),
            "bail_reason should name the failure: {on_disk}"
        );
        assert_eq!(on_disk["run_id"], serde_json::json!(run_id.to_string()));

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[actix_web::test]
    async fn a_clean_sdlc_flow_run_does_not_touch_the_committed_state_file() {
        // Same SDLC-flow-shaped fixture, but the second node succeeds instead
        // of failing -- the failure-path writer must be a complete no-op on
        // this path, leaving whatever a prior `SaveStateNode`/`WrapUpNode`
        // write left on disk untouched.
        let worktree = temp_worktree_for_terminal_write_test();
        seed_sdlc_state_file(&worktree);
        let before = read_sdlc_state_file(&worktree);

        let mut nodes = StdHashMap::new();
        nodes.insert(
            "SeedSdlcContextNode".to_string(),
            engine_core::NodeConfig::new("SeedSdlcContextNode", vec!["MarkerNode".to_string()]),
        );
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        let schema = WorkflowSchema::new(
            engine_core::workflows::sdlc_flow::graph::WORKFLOW_TYPE,
            "SeedSdlcContextNode",
            nodes,
        );

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(schema.clone(), {
            let worktree = worktree.clone();
            Box::new(move |_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(SeedSdlcContextNode {
                    worktree: worktree.clone(),
                }));
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(registry, schema.clone()))
            })
        });
        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            api_key: "test-key".to_string(),
        };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let trigger_req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(serde_json::json!({
                "workflow_type": engine_core::workflows::sdlc_flow::graph::WORKFLOW_TYPE,
                "data": {},
            }))
            .to_request();
        let trigger_resp = test::call_service(&app, trigger_req).await;
        assert_eq!(trigger_resp.status(), 202);
        let trigger_body: serde_json::Value = test::read_body_json(trigger_resp).await;
        let run_id = trigger_body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let poll_req = test::TestRequest::get()
                .uri(&format!("/events/{run_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let poll_resp = test::call_service(&app, poll_req).await;
            let poll_body: serde_json::Value = test::read_body_json(poll_resp).await;
            if poll_body["status"] != "running" {
                assert_eq!(poll_body["status"], "succeeded");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "run never left \"running\""
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let after = read_sdlc_state_file(&worktree);
        assert_eq!(
            after, before,
            "a clean run must not modify the committed state file via the failure-path writer"
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    mod live_run_workflow_type_tests {
        use super::{live_run_metadata, live_run_workflow_type};
        use chrono::Utc;
        use uuid::Uuid;

        /// `live_run_workflow_type` returns the `(workflow_type, created_at)`
        /// pair verbatim for a run inserted via the same path `post_events`
        /// uses (a direct insert into `live_run_metadata()`), and `None` for
        /// a `run_id` never inserted. Uses a fresh random `Uuid` for the
        /// "unknown" case specifically so it cannot collide with an id
        /// another test in this process-global-backed suite happens to have
        /// inserted.
        #[test]
        fn round_trips_and_is_absent_for_unknown_run() {
            let run_id = Uuid::new_v4();
            let created_at = Utc::now();
            live_run_metadata()
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(run_id, ("fixture".to_string(), created_at));

            let found = live_run_workflow_type(run_id);
            assert_eq!(found, Some(("fixture".to_string(), created_at)));

            let unknown_run_id = Uuid::new_v4();
            assert_eq!(live_run_workflow_type(unknown_run_id), None);
        }
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

    mod derive_live_status_tests {
        use super::derive_live_status;
        use engine_contract::TaskContext;
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
        fn suspended_when_suspension_marker_is_set() {
            let ctx = empty_context(serde_json::json!({
                "suspension": {
                    "suspended": true,
                    "at": "2026-01-01T00:00:00Z",
                    "resume_at": "MarkerNode",
                    "reason": "operator_pause",
                    "origin_identity": null,
                    "ledger": { "total_tokens": 0, "total_cost_usd": 0.0 },
                    "resume_count": 0,
                    "requested": false
                }
            }));
            assert_eq!(derive_live_status(&ctx), "suspended");
        }

        #[test]
        fn running_when_suspension_marker_is_resumed() {
            let ctx = empty_context(serde_json::json!({
                "suspension": {
                    "suspended": false,
                    "at": "2026-01-01T00:00:00Z",
                    "resume_at": "MarkerNode",
                    "reason": "operator_pause",
                    "origin_identity": null,
                    "ledger": { "total_tokens": 0, "total_cost_usd": 0.0 },
                    "resume_count": 1,
                    "requested": false
                }
            }));
            assert_eq!(derive_live_status(&ctx), "running");
        }

        #[test]
        fn running_when_metadata_has_no_suspension_marker() {
            let ctx = empty_context(serde_json::json!({}));
            assert_eq!(derive_live_status(&ctx), "running");
        }

        #[test]
        fn running_when_metadata_is_null() {
            let ctx = empty_context(serde_json::Value::Null);
            assert_eq!(derive_live_status(&ctx), "running");
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
