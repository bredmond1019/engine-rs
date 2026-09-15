//! `POST /webhooks/pre-plan/inbound` (`EN.19.A` task 5): dispatch one
//! `PRE_PLAN` run from a short free-text idea, with no human back-and-forth.
//!
//! Mirrors `crate::email_webhooks::inbound_email` exactly: the same
//! `crate::http::check_api_key` gate (`401` on failure, nothing dispatched),
//! and the same [`crate::email_webhooks::dispatch_and_spawn`] fresh-trigger
//! path (`pub(crate)` there specifically so this module can reuse it rather
//! than re-implementing dispatch/spawn).
//!
//! **Deploy boundary:** this route only becomes reachable once the running
//! `bastion serve` process is rebuilt and restarted — registering the route
//! (this file) and calling `register_pre_plan` (`crates/engine-serve/src/
//! workflows.rs`) are source changes, not something a live process picks up
//! on its own.
//!
//! **Same-slug in-flight conflict guard.** Before dispatching, this checks
//! `state.live` for another live (non-terminal) `PRE_PLAN` run whose
//! `ctx.event["slug"]` matches this request's slug, via
//! `crate::http::live_run_workflow_type` (the same side table `GET
//! /events/{event_id}` reads a live run's `workflow_type` from). A match
//! returns `409 Conflict` without dispatching, rather than letting two runs
//! race to write the same `notes.md`. This only detects a same-slug
//! collision — general concurrent-dispatch protection for unrelated slugs is
//! explicitly out of scope for this block (see `planning/EN.19.A.json`'s
//! `out_of_scope`).

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde_json::json;

use crate::email_webhooks::dispatch_and_spawn;
use crate::http::AppState;

/// The `workflow_type` this route dispatches.
const PRE_PLAN_WORKFLOW_TYPE: &str = "PRE_PLAN";

/// Read `event["slug"]` as a trimmed, non-empty string, if present. Used both
/// to validate the inbound body up front (so a malformed request never
/// reaches the dispatcher) and to compare against a live run's own stamped
/// slug for the in-flight conflict check.
fn slug_of(event: &serde_json::Value) -> Option<&str> {
    event
        .get("slug")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether another **live** (non-terminal) `PRE_PLAN` run already carries the
/// same `slug` as this request. Consults `state.live.list_live_records()`
/// (every currently-live run's snapshot) filtered through
/// `crate::http::live_run_workflow_type` for the `workflow_type` — a run not
/// yet resolved as `PRE_PLAN` in that side table (e.g. one dispatched by a
/// different route) can never collide here.
fn in_flight_pre_plan_slug_conflict(state: &web::Data<AppState>, slug: &str) -> bool {
    state
        .live
        .list_live_records()
        .iter()
        .any(|(run_id, ctx, _)| {
            let is_pre_plan = crate::http::live_run_workflow_type(*run_id)
                .map(|(workflow_type, _)| workflow_type == PRE_PLAN_WORKFLOW_TYPE)
                .unwrap_or(false);
            is_pre_plan && slug_of(&ctx.event) == Some(slug)
        })
}

/// `POST /webhooks/pre-plan/inbound` — parse a `{idea, slug, ...}` payload
/// and dispatch one `PRE_PLAN` run carrying it verbatim as the event
/// (`IntakeIdeaNode`/`CheckExistingNotesNode` validate the required fields
/// once the workflow itself runs).
///
/// `401` without a valid `X-API-Key` (nothing parsed or dispatched). `400`
/// naming the missing field when `idea`/`slug` is absent or empty (nothing
/// dispatched). `409` when another live `PRE_PLAN` run already carries the
/// same `slug` (nothing dispatched). Otherwise `202 {run_id, event_id}` —
/// `event_id` always equals `run_id`, matching `inbound_email`'s and `POST
/// /events/`'s contract.
pub async fn inbound_pre_plan(
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !crate::http::check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let event = body.into_inner();

    let idea_present = event
        .get("idea")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if !idea_present {
        return HttpResponse::BadRequest().json(json!({
            "error": "malformed pre-plan idea",
            "message": "missing or empty 'idea'",
        }));
    }

    let slug = match slug_of(&event) {
        Some(slug) => slug.to_string(),
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": "malformed pre-plan idea",
                "message": "missing or empty 'slug'",
            }));
        }
    };

    if in_flight_pre_plan_slug_conflict(&state, &slug) {
        return HttpResponse::Conflict().json(json!({
            "error": "pre-plan run already in flight for this slug",
            "slug": slug,
        }));
    }

    match dispatch_and_spawn(&state, PRE_PLAN_WORKFLOW_TYPE, event) {
        Err(response) => response,
        Ok(run_id) => HttpResponse::Accepted().json(json!({
            "run_id": run_id,
            "event_id": run_id,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use engine_contract::TaskContext;
    use engine_core::dispatch::Dispatcher;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;
    use std::sync::{Arc, Mutex};

    /// A node that records the event it was constructed against into a
    /// shared sink and does nothing else — same observation-point shape as
    /// `email_webhooks.rs`'s own `MarkerNode`.
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

    /// `AppState` whose dispatcher registers `PRE_PLAN` against a fixture
    /// single-node graph, recording every dispatched event into `recorded`.
    fn test_app_state() -> (AppState, Arc<Mutex<Vec<serde_json::Value>>>) {
        let recorded: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

        let mut dispatcher = Dispatcher::new();
        let recorded_for_factory = recorded.clone();
        dispatcher.register(
            fixture_schema(PRE_PLAN_WORKFLOW_TYPE),
            Box::new(move |event: &serde_json::Value| {
                recorded_for_factory
                    .lock()
                    .expect("recorded lock poisoned")
                    .push(event.clone());
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(
                    registry,
                    fixture_schema(PRE_PLAN_WORKFLOW_TYPE),
                ))
            }),
        );

        let state = AppState::builder(
            Arc::new(dispatcher),
            crate::live_state::LiveStateStore::new(),
            crate::durable::spawn_durable_writer(None),
            "test-key".to_string(),
        )
        .build();
        (state, recorded)
    }

    fn configure(cfg: &mut web::ServiceConfig) {
        cfg.route(
            "/webhooks/pre-plan/inbound",
            web::post().to(inbound_pre_plan),
        );
    }

    fn valid_payload() -> serde_json::Value {
        json!({"idea": "build a widget", "slug": "widget-idea"})
    }

    #[actix_web::test]
    async fn inbound_without_the_api_key_is_401_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .set_json(valid_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn inbound_valid_payload_dispatches_one_pre_plan_run() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(valid_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["run_id"].as_str().is_some());
        assert_eq!(body["run_id"], body["event_id"]);

        let events = recorded.lock().expect("recorded lock poisoned").clone();
        assert_eq!(events.len(), 1, "exactly one run should be dispatched");
        assert_eq!(events[0]["idea"], json!("build a widget"));
        assert_eq!(events[0]["slug"], json!("widget-idea"));
    }

    #[actix_web::test]
    async fn inbound_missing_idea_is_400_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(json!({"slug": "widget-idea"}))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 400);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn inbound_missing_slug_is_400_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(json!({"idea": "build a widget"}))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 400);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn inbound_second_dispatch_for_an_in_flight_slug_is_409_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();

        // Seed a live, non-terminal PRE_PLAN run for the same slug, exactly
        // as `post_events`/`dispatch_and_spawn` would leave one mid-flight:
        // a snapshot in `state.live` plus the side-table `workflow_type`
        // entry `crate::http::live_run_workflow_type` reads.
        let run_id = uuid::Uuid::new_v4();
        let snapshot = TaskContext {
            event: valid_payload(),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        state.live.record(run_id, &snapshot);
        crate::http::live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned")
            .insert(
                run_id,
                (PRE_PLAN_WORKFLOW_TYPE.to_string(), chrono::Utc::now()),
            );

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(valid_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 409);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());

        // Clean up the process-global side table so this test cannot leak
        // state into another test in this binary.
        crate::http::live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned")
            .remove(&run_id);
    }

    #[actix_web::test]
    async fn inbound_different_slug_while_one_is_in_flight_still_dispatches() {
        let (state, recorded) = test_app_state();

        let run_id = uuid::Uuid::new_v4();
        let snapshot = TaskContext {
            event: json!({"idea": "another idea", "slug": "other-slug"}),
            nodes: StdHashMap::new(),
            metadata: json!({}),
            node_runs: StdHashMap::new(),
        };
        state.live.record(run_id, &snapshot);
        crate::http::live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned")
            .insert(
                run_id,
                (PRE_PLAN_WORKFLOW_TYPE.to_string(), chrono::Utc::now()),
            );

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/pre-plan/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(valid_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        assert_eq!(
            recorded.lock().expect("recorded lock poisoned").len(),
            1,
            "a different slug must not be blocked by an unrelated in-flight run"
        );

        crate::http::live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned")
            .remove(&run_id);
    }
}
