//! The three suspend/resume routes (EN.6.F task 11): `POST
//! /events/{run_id}/pause`, `POST /events/{event_id}/resume`, and `GET
//! /events/suspended`.
//!
//! **Route ordering matters.** `configure` (`crate::http::configure`) MUST
//! register `/events/suspended` before `/events/{event_id}` — actix-web
//! resolves routes first-registration-wins, so a literal path registered
//! after a `{event_id}` segment would never be reached; the uuid extractor
//! swallows it first.
//!
//! All three handlers gate on `X-API-Key` via [`crate::http::check_api_key`],
//! matching every other run route.

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_core::workflow::ResumeState;
use engine_core::{BudgetLedger, CancellationToken, PauseSignal};
use uuid::Uuid;

use crate::dispatch::DispatchError;
use crate::http::{check_api_key, default_budget_from_env, AppState};
use crate::suspend::{self, RunStart, SpawnedRun, SuspendedEntry, TakeForResume};

/// `POST /events/{run_id}/pause` — 401 without a valid `X-API-Key`; `404`
/// for a `run_id` that is neither live nor suspended; `409` if it is already
/// suspended; otherwise sets the run's [`PauseSignal`] and returns `202
/// {run_id, status: "pausing"}`. Idempotent: a repeat call against an
/// already-pausing (but not yet suspended) run also 202s.
pub async fn pause_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();

    if suspend::list_suspended()
        .iter()
        .any(|(id, _)| *id == run_id)
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "run_id": run_id,
            "status": "suspended",
            "error": "run is already suspended",
        }));
    }

    match suspend::get_pause_signal(run_id) {
        Some(signal) => {
            signal.pause();
            HttpResponse::Accepted().json(serde_json::json!({
                "run_id": run_id,
                "status": "pausing",
            }))
        }
        None => {
            HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or finished run" }))
        }
    }
}

/// `GET /events/suspended` — `200 [{run_id, workflow_type, created_at,
/// suspended_at, resume_at, reason}]`, newest first. `X-API-Key` gated like
/// every other run route.
pub async fn list_suspended(req: HttpRequest, state: web::Data<AppState>) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let body: Vec<serde_json::Value> = suspend::list_suspended()
        .into_iter()
        .map(|(run_id, entry)| {
            serde_json::json!({
                "run_id": run_id,
                "workflow_type": entry.workflow_type,
                "created_at": entry.created_at,
                "suspended_at": entry.suspended_at,
                "resume_at": entry.resume_at,
                "reason": entry.reason,
            })
        })
        .collect();

    HttpResponse::Ok().json(body)
}

/// Falls back to Postgres for a suspended run this process never held
/// in-memory (e.g. a restart) — only reachable when `state.durable` was
/// constructed with a pool. Returns `None` on any failure (no pool, no such
/// row, or the row is not currently suspended) so the caller can 404
/// uniformly.
async fn rehydrate_from_store(state: &AppState, run_id: Uuid) -> Option<SuspendedEntry> {
    let pool = state.durable.pool()?;
    let row = engine_store::get_event(pool, run_id).await.ok()?;

    if !engine_core::suspend::is_suspended(&row.task_context.metadata) {
        return None;
    }

    let suspension = engine_core::suspend::read_suspension(&row.task_context.metadata);
    let resume_at = suspension
        .as_ref()
        .and_then(|s| s.resume_at.clone())
        .unwrap_or_default();
    let reason = suspension
        .as_ref()
        .and_then(|s| s.reason)
        .and_then(|r| serde_json::to_value(r).ok())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();

    Some(SuspendedEntry {
        workflow_type: row.workflow_type,
        data: row.data,
        snapshot: row.task_context,
        created_at: row.created_at,
        suspended_at: row.updated_at,
        resume_at,
        reason,
        // Never inserted into the in-memory index, so `remove_suspended`/
        // `clear_resuming` against this run_id are harmless no-ops on every
        // exit path below -- this flag exists only so the struct shape
        // matches the in-memory case.
        resuming: true,
    })
}

/// `POST /events/{event_id}/resume` — rehydrates a suspended run's
/// `Workflow`/`TaskContext`/budget ledger and continues the walk from the
/// stored `resume_at` pointer. No request body: an operator `{"at": ..}`
/// override is a deliberate non-goal (see task 11's spec).
///
/// | condition | response |
/// |---|---|
/// | unknown, or not suspended | `404` |
/// | already resuming (concurrent) | `409` |
/// | factory policy resolution fails | `422` |
/// | `resume_at` not in the rebuilt workflow | `422` |
/// | ok | `202 {run_id, event_id, status: "resuming", resume_at}` |
///
/// Every failure path from step 4 onward calls [`suspend::clear_resuming`]
/// so a transient failure (a bad policy resolution, a stale `resume_at`)
/// leaves the run retryable rather than permanently bricked.
pub async fn resume_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();

    let entry = match suspend::take_for_resume(run_id) {
        TakeForResume::Ready(entry) => *entry,
        TakeForResume::AlreadyResuming => {
            return HttpResponse::Conflict()
                .json(serde_json::json!({ "error": "resume already in flight" }));
        }
        TakeForResume::NotFound => match rehydrate_from_store(&state, run_id).await {
            Some(entry) => entry,
            None => {
                return HttpResponse::NotFound()
                    .json(serde_json::json!({ "error": "unknown or non-resumable run" }));
            }
        },
    };

    // Step 4: rebuild the budget ledger from the marker's snapshot,
    // falling back to the lossy `from_context` reconstruction when the
    // marker carries no ledger (an older/foreign snapshot).
    let marker = engine_core::suspend::read_suspension(&entry.snapshot.metadata);
    let ledger = marker
        .as_ref()
        .and_then(|m| m.ledger)
        .map(|snapshot| BudgetLedger::from_parts(snapshot.total_tokens, snapshot.total_cost_usd))
        .unwrap_or_else(|| BudgetLedger::from_context(&entry.snapshot));

    // Step 5: rebuild the Workflow from the ORIGINAL trigger payload, then
    // drop any seeded_nodes -- the rehydrated ctx already carries the
    // original run's resolved policy, and a factory rebuilt for the resume
    // must never re-seed/overwrite it.
    let workflow = match state
        .dispatcher
        .dispatch_with_event(&entry.workflow_type, &entry.data)
    {
        Ok(workflow) => workflow.without_seeded_nodes(),
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            suspend::clear_resuming(run_id);
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "policy resolution failed",
                "message": format!("unknown workflow_type '{workflow_type}'"),
            }));
        }
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            suspend::clear_resuming(run_id);
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "policy resolution failed",
                "message": message,
            }));
        }
    };

    // Step 6: the resume point must still exist in the rebuilt graph.
    if !workflow.has_node(&entry.resume_at) {
        suspend::clear_resuming(run_id);
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "error": "resume point no longer exists in the workflow graph",
            "resume_at": entry.resume_at,
        }));
    }

    // Step 7: re-register fresh live-run bookkeeping -- a fresh
    // CancellationToken, a fresh PauseSignal, and the ORIGINAL created_at
    // (not `Utc::now()`) back into `live_run_metadata()`.
    let token = CancellationToken::new();
    state.runs.register(run_id, token.clone());

    let pause = PauseSignal::new();
    suspend::register_pause_signal(run_id, pause.clone());

    crate::http::live_run_metadata()
        .write()
        .expect("live run metadata lock poisoned on write")
        .insert(run_id, (entry.workflow_type.clone(), entry.created_at));

    // Step 8: invalidate the cached terminal ("suspended") SSE frame so a
    // subscriber attached after this resume gets live frames instead of the
    // stale suspended one.
    crate::stream::clear_terminal(run_id);

    // Step 9: the run is no longer suspended -- drop it from the index
    // (a no-op if it was never inserted, i.e. the Postgres-fallback path),
    // then spawn the resumed walk.
    suspend::remove_suspended(run_id);

    let resume_at = entry.resume_at.clone();
    let budget = default_budget_from_env();

    suspend::spawn_run(SpawnedRun {
        run_id,
        workflow,
        workflow_type: entry.workflow_type,
        data: entry.data,
        created_at: entry.created_at,
        start: RunStart::Resume(ResumeState {
            ctx: entry.snapshot,
            at_identity: resume_at.clone(),
            ledger,
        }),
        live: state.live.clone(),
        durable: state.durable.clone(),
        runs: state.runs.clone(),
        token,
        pause,
        budget,
    });

    HttpResponse::Accepted().json(serde_json::json!({
        "run_id": run_id,
        "event_id": run_id,
        "status": "resuming",
        "resume_at": resume_at,
    }))
}

#[cfg(test)]
// These tests hold `registry_test_lock()`'s std `MutexGuard` across `.await` points by
// design — it exists solely to serialize tests that share the global suspend registry, not
// to guard data an async task contends over concurrently, so the guard's lifetime spanning
// the whole test body is intentional rather than a correctness hazard.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use chrono::Utc;
    use engine_contract::TaskContext;
    use engine_core::dispatch::Dispatcher;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;
    use std::sync::Arc;

    use crate::abort::RunRegistry;
    use crate::live_state::LiveStateStore;

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

    struct OnlyNode;

    #[async_trait::async_trait]
    impl Node for OnlyNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "OnlyNode"
        }
    }

    /// `SuspendNode -> MarkerNode`, mirroring `http.rs`'s own suspend
    /// fixture: `SuspendNode`'s successor is the resume pointer.
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
            campaigns: crate::abort::CampaignRegistry::new(),
            api_key: "test-key".to_string(),
        }
    }

    /// Triggers the suspend-fixture workflow against `$app` and blocks until
    /// its run lands in the suspended index, yielding the `run_id`. A macro
    /// (rather than a generic async fn) so it works against whatever
    /// unnameable `impl Service<..>` type `test::init_service` produces for
    /// each test's own `App`.
    macro_rules! suspend_a_run {
        ($app:expr) => {{
            let req = test::TestRequest::post()
                .uri("/events/")
                .insert_header(("X-API-Key", "test-key"))
                .set_json(serde_json::json!({ "workflow_type": "suspend-fixture", "data": {} }))
                .to_request();
            let resp = test::call_service(&$app, req).await;
            assert_eq!(resp.status(), 202);
            let body: serde_json::Value = test::read_body_json(resp).await;
            let run_id = body["run_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .expect("run_id should be a parseable UUID");

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if suspend::list_suspended()
                    .into_iter()
                    .any(|(id, _)| id == run_id)
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "run never landed in the suspended index"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            run_id
        }};
    }

    // -- pause -------------------------------------------------------------

    #[actix_web::test]
    async fn pause_without_api_key_is_unauthorized() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
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
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/pause", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn pause_a_live_run_is_202_and_idempotent() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        let sig = PauseSignal::new();
        suspend::register_pause_signal(run_id, sig.clone());

        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        for _ in 0..2 {
            let req = test::TestRequest::post()
                .uri(&format!("/events/{run_id}/pause"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), 202);
        }
        assert!(sig.is_paused());

        suspend::remove_pause_signal(run_id);
    }

    #[actix_web::test]
    async fn pause_an_already_suspended_run_is_409() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/pause"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 409);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "suspended");

        suspend::remove_suspended(run_id);
    }

    // -- suspended list ------------------------------------------------

    #[actix_web::test]
    async fn suspended_route_resolves_as_a_literal_not_an_event_id() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            200,
            "the literal /events/suspended path must not be swallowed by the {{event_id}} extractor"
        );
        let body: Vec<serde_json::Value> = test::read_body_json(resp).await;
        assert!(body.is_empty());
    }

    #[actix_web::test]
    async fn suspended_list_contains_a_suspended_run_and_omits_it_after_resume() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: Vec<serde_json::Value> = test::read_body_json(resp).await;
        assert!(body
            .iter()
            .any(|entry| entry["run_id"] == run_id.to_string()));

        let resume_req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resume_resp = test::call_service(&app, resume_req).await;
        assert_eq!(resume_resp.status(), 202);

        let after_req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let after_resp = test::call_service(&app, after_req).await;
        let after_body: Vec<serde_json::Value> = test::read_body_json(after_resp).await;
        assert!(!after_body
            .iter()
            .any(|entry| entry["run_id"] == run_id.to_string()));
    }

    // -- resume --------------------------------------------------------

    #[actix_web::test]
    async fn resume_unknown_run_is_404() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/resume", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn resume_without_api_key_is_unauthorized() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/resume", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn resume_succeeds_with_no_database_url_and_completes_the_run() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "resuming");
        assert_eq!(body["resume_at"], "MarkerNode");

        // Poll the readback until the resumed run reaches its terminal
        // state -- MarkerNode is the only remaining node.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let get_req = test::TestRequest::get()
                .uri(&format!("/events/{run_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let get_resp = test::call_service(&app, get_req).await;
            let get_body: serde_json::Value = test::read_body_json(get_resp).await;
            if get_body["status"] == "succeeded" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "resumed run never reached succeeded"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[actix_web::test]
    async fn a_second_concurrent_resume_is_409_and_the_first_still_succeeds() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        // Take the resuming flag directly, simulating a first caller already
        // in flight, then assert the HTTP layer reports 409 for a second.
        match suspend::take_for_resume(run_id) {
            TakeForResume::Ready(_) => {}
            _ => panic!("expected Ready"),
        }

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 409);

        // The first caller's in-flight resume is still retryable/valid --
        // clearing it and resuming for real now succeeds.
        suspend::clear_resuming(run_id);
        let retry_req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let retry_resp = test::call_service(&app, retry_req).await;
        assert_eq!(retry_resp.status(), 202);
    }

    #[actix_web::test]
    async fn resume_with_unresolvable_resume_point_is_422_and_clears_resuming() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A workflow whose factory rebuilds a *different* graph (no
        // "MarkerNode") than the one the run suspended in -- simulating
        // schema drift between suspend and resume.
        const WORKFLOW_TYPE: &str = "drifted-fixture";
        let mut dispatcher = Dispatcher::new();
        let drifted_schema = || {
            let mut nodes = StdHashMap::new();
            nodes.insert(
                "OnlyNode".to_string(),
                engine_core::NodeConfig::new("OnlyNode", vec![]),
            );
            WorkflowSchema::new(WORKFLOW_TYPE, "OnlyNode", nodes)
        };
        dispatcher.register(
            drifted_schema(),
            Box::new(move |_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(OnlyNode));
                Ok(Workflow::new(registry, drifted_schema()))
            }),
        );

        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            campaigns: crate::abort::CampaignRegistry::new(),
            api_key: "test-key".to_string(),
        };

        let run_id = Uuid::new_v4();
        let mut metadata = serde_json::json!({});
        engine_core::suspend::stamp_suspended(
            &mut metadata,
            engine_core::suspend::Suspension {
                resume_at: "MarkerNode",
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
            SuspendedEntry {
                workflow_type: WORKFLOW_TYPE.to_string(),
                data: serde_json::json!({}),
                snapshot,
                created_at: Utc::now(),
                suspended_at: Utc::now(),
                resume_at: "MarkerNode".to_string(),
                reason: "operator_pause".to_string(),
                resuming: false,
            },
        );

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["resume_at"], "MarkerNode");

        // Retryable: `resuming` was cleared, so a follow-up resume against
        // the (still-broken) run is not falsely rejected as already-in-flight.
        match suspend::take_for_resume(run_id) {
            TakeForResume::Ready(_) => {}
            _ => panic!("expected the failed resume to have cleared `resuming`, got a state that is not Ready"),
        }

        suspend::remove_suspended(run_id);
    }
}
