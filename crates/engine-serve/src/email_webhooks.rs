//! Email-channel webhook routes (`EN.6.B` task 6): `POST /webhooks/email/inbound`
//! and `POST /webhooks/email/events`.
//!
//! Both routes are gated by the same `X-API-Key` check every other mutating
//! route uses ([`crate::http::check_api_key`]) — `401` without a valid key,
//! nothing dispatched. Resend signs its webhooks with Svix signatures rather
//! than a custom header, so the deployment contract is that these routes sit
//! behind a shim or tunnel that supplies `X-API-Key`; **Svix signature
//! verification is deliberately out of scope for this block** (see
//! `planning/EN.6.B-email-adapter/tasks.md`'s Notes) and is a follow-up
//! tightening.
//!
//! Neither handler refactors `crate::http::post_events` — its
//! dispatch/spawn machinery (`crate::suspend::SpawnedRun` +
//! `crate::suspend::spawn_run`) is already extracted and `pub(crate)`, so
//! this module calls it directly through the shared [`dispatch_and_spawn`]
//! helper below, mirroring `post_events`'s fresh-trigger path exactly.

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use engine_core::{CancellationToken, PauseSignal};
use serde_json::json;
use uuid::Uuid;

use crate::dispatch::DispatchError;
use crate::http::AppState;

/// The `workflow_type` a well-formed inbound-mail payload dispatches.
const INBOUND_WORKFLOW_TYPE: &str = "CONTENT_PIPELINE";

/// Dispatch `workflow_type` with `event` and spawn it, mirroring
/// `crate::http::post_events`'s fresh-trigger path exactly: dispatch through
/// the registered factory, mint a fresh `run_id`, register a cancellation
/// token and a pause signal, seed the default HTTP budget, record
/// `live_run_metadata`, then hand off to `crate::suspend::spawn_run`.
///
/// Returns the minted `run_id` on success, or the same 422 shape
/// `post_events` returns for an unregistered `workflow_type` / a policy
/// resolution failure.
fn dispatch_and_spawn(
    state: &web::Data<AppState>,
    workflow_type: &str,
    event: serde_json::Value,
) -> Result<Uuid, HttpResponse> {
    let workflow = match state.dispatcher.dispatch_with_event(workflow_type, &event) {
        Ok(workflow) => workflow,
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            return Err(HttpResponse::UnprocessableEntity().json(json!({
                "error": "unknown workflow_type",
                "workflow_type": workflow_type,
            })));
        }
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            return Err(HttpResponse::UnprocessableEntity().json(json!({
                "error": "policy resolution failed",
                "message": message,
            })));
        }
    };

    let run_id = Uuid::new_v4();
    let workflow_type = workflow_type.to_string();
    let data = event.clone();
    let live = state.live.clone();
    let runs = state.runs.clone();
    let durable_handle = state.durable.clone();

    let token = CancellationToken::new();
    runs.register(run_id, token.clone());

    let pause = PauseSignal::new();
    crate::suspend::register_pause_signal(run_id, pause.clone());

    let budget = crate::http::default_budget_from_env();

    let created_at = Utc::now();
    crate::http::live_run_metadata()
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

    Ok(run_id)
}

/// `POST /webhooks/email/inbound` — parse a Resend inbound-mail payload into
/// an `IngressEnvelope` ([`engine_core::nodes::email::parse_inbound_email`])
/// and dispatch one `CONTENT_PIPELINE` run carrying it.
///
/// `401` without a valid `X-API-Key` (nothing parsed or dispatched). `400`
/// naming the missing field for a malformed payload (nothing dispatched).
/// Otherwise `202 {run_id, event_id, envelope_id}` — `event_id` always
/// equals `run_id`, matching `POST /events/`'s contract.
pub async fn inbound_email(
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !crate::http::check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let envelope = match engine_core::nodes::email::parse_inbound_email(&body) {
        Ok(envelope) => envelope,
        Err(message) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "malformed inbound email",
                "message": message,
            }));
        }
    };

    let envelope_id = envelope.envelope_id.clone();
    let event = json!({ "envelope": envelope });

    match dispatch_and_spawn(&state, INBOUND_WORKFLOW_TYPE, event) {
        Err(response) => response,
        Ok(run_id) => HttpResponse::Accepted().json(json!({
            "run_id": run_id,
            "event_id": run_id,
            "envelope_id": envelope_id,
        })),
    }
}

/// `POST /webhooks/email/events` — map a Resend delivery/bounce webhook
/// payload ([`engine_core::nodes::email::map_delivery_event`]) onto `EN.7.B`'s
/// `OPPORTUNITY_ADD_ACTION` workflow, correlated solely via the
/// `opportunity_slug` tag echoed back on send.
///
/// `401` without a valid `X-API-Key` (nothing mapped or dispatched). `400`
/// for a structurally malformed payload. An untagged or unrecognized event
/// is accepted and explicitly skipped — `202 {skipped: true, reason}` — with
/// nothing dispatched, rather than erroring or guessing a slug. A correlated
/// event dispatches exactly one `OPPORTUNITY_ADD_ACTION` run and returns
/// `202 {run_id, event_id}`.
pub async fn email_delivery_events(
    req: HttpRequest,
    body: web::Json<serde_json::Value>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !crate::http::check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    match engine_core::nodes::email::map_delivery_event(&body) {
        Err(message) => HttpResponse::BadRequest().json(json!({
            "error": "malformed email webhook",
            "message": message,
        })),
        Ok(engine_core::nodes::email::DeliveryMapping::Skipped(reason)) => HttpResponse::Accepted()
            .json(json!({
                "skipped": true,
                "reason": reason.as_str(),
            })),
        Ok(engine_core::nodes::email::DeliveryMapping::Action(action)) => {
            let event = match serde_json::to_value(&action) {
                Ok(event) => event,
                Err(err) => {
                    return HttpResponse::InternalServerError().json(json!({
                        "error": "failed to serialize add-action event",
                        "message": err.to_string(),
                    }));
                }
            };

            match dispatch_and_spawn(
                &state,
                engine_core::workflows::opportunity_edit::ADD_ACTION_WORKFLOW_TYPE,
                event,
            ) {
                Err(response) => response,
                Ok(run_id) => HttpResponse::Accepted().json(json!({
                    "run_id": run_id,
                    "event_id": run_id,
                })),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, web, App};
    use engine_contract::TaskContext;
    use engine_core::dispatch::Dispatcher;
    use engine_core::workflows::content_pipeline::schema::ContentPipelineInput;
    use engine_core::workflows::opportunity_edit::schema::AddOpportunityActionEvent;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;
    use std::sync::{Arc, Mutex};

    /// A node that records the event it was constructed against into a
    /// shared sink and does nothing else — the observation point for
    /// "exactly one run was dispatched carrying this event", following
    /// `http.rs`'s `post_events_forwards_data_to_the_factory_for_policy_resolution`
    /// precedent (the factory closure is where the event is visible).
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

    /// `AppState` whose dispatcher registers `CONTENT_PIPELINE` and
    /// `OPPORTUNITY_ADD_ACTION` against fixture single-node graphs, recording
    /// every dispatched event into `recorded` for assertion.
    fn test_app_state() -> (AppState, Arc<Mutex<Vec<serde_json::Value>>>) {
        let recorded: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

        let mut dispatcher = Dispatcher::new();
        for workflow_type in [INBOUND_WORKFLOW_TYPE, "OPPORTUNITY_ADD_ACTION"] {
            let recorded_for_factory = recorded.clone();
            let workflow_type_owned = workflow_type.to_string();
            dispatcher.register(
                fixture_schema(workflow_type),
                Box::new(move |event: &serde_json::Value| {
                    recorded_for_factory
                        .lock()
                        .expect("recorded lock poisoned")
                        .push(event.clone());
                    let mut registry = NodeRegistry::new();
                    registry.register(Box::new(MarkerNode));
                    Ok(Workflow::new(
                        registry,
                        fixture_schema(&workflow_type_owned),
                    ))
                }),
            );
        }

        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: crate::live_state::LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: crate::abort::RunRegistry::new(),
            api_key: "test-key".to_string(),
        };
        (state, recorded)
    }

    fn configure(cfg: &mut web::ServiceConfig) {
        cfg.route("/webhooks/email/inbound", web::post().to(inbound_email))
            .route(
                "/webhooks/email/events",
                web::post().to(email_delivery_events),
            );
    }

    fn inbound_payload() -> serde_json::Value {
        json!({
            "message_id": "msg-abc-123",
            "from": "prospect@example.com",
            "subject": "Re: proposal",
            "text": "Sounds great, let's talk.",
            "created_at": "2026-07-30T12:00:00Z",
        })
    }

    fn tagged_bounce_payload() -> serde_json::Value {
        json!({
            "type": "email.bounced",
            "created_at": "2026-07-30T12:00:00Z",
            "data": {
                "to": ["prospect@example.com"],
                "tags": { "opportunity_slug": "acme-corp" },
            },
        })
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
            .uri("/webhooks/email/inbound")
            .set_json(inbound_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn inbound_valid_payload_dispatches_one_content_pipeline_run() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/email/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(inbound_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["run_id"].as_str().is_some());
        assert!(body["event_id"].as_str().is_some());
        assert_eq!(body["run_id"], body["event_id"]);
        assert!(body["envelope_id"].as_str().is_some());

        let events = recorded.lock().expect("recorded lock poisoned").clone();
        assert_eq!(events.len(), 1, "exactly one run should be dispatched");
        let input: ContentPipelineInput = serde_json::from_value(events[0].clone())
            .expect("dispatched event should deserialize as ContentPipelineInput");
        assert_eq!(
            input.envelope.channel_type,
            engine_contract::envelope::ChannelType::Email
        );
    }

    #[actix_web::test]
    async fn inbound_malformed_payload_is_400_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/email/inbound")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(json!({}))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 400);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn events_without_the_api_key_is_401_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/email/events")
            .set_json(tagged_bounce_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn events_tagged_bounce_dispatches_one_add_action_run() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/email/events")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(tagged_bounce_payload())
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["run_id"].as_str().is_some());
        assert_eq!(body["run_id"], body["event_id"]);

        let events = recorded.lock().expect("recorded lock poisoned").clone();
        assert_eq!(events.len(), 1, "exactly one run should be dispatched");
        let action: AddOpportunityActionEvent = serde_json::from_value(events[0].clone())
            .expect("dispatched event should deserialize as AddOpportunityActionEvent");
        assert_eq!(action.slug, "acme-corp");
        assert_eq!(action.kind, "email-bounced");
    }

    #[actix_web::test]
    async fn events_untagged_bounce_is_202_skipped_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let untagged = json!({
            "type": "email.bounced",
            "created_at": "2026-07-30T12:00:00Z",
            "data": { "to": ["prospect@example.com"] },
        });

        let req = test::TestRequest::post()
            .uri("/webhooks/email/events")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(untagged)
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["skipped"], json!(true));
        assert_eq!(body["reason"], json!("no_opportunity_slug_tag"));
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn events_unknown_event_type_is_202_skipped_and_dispatches_nothing() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let unknown = json!({
            "type": "email.opened",
            "created_at": "2026-07-30T12:00:00Z",
            "data": {
                "to": ["prospect@example.com"],
                "tags": { "opportunity_slug": "acme-corp" },
            },
        });

        let req = test::TestRequest::post()
            .uri("/webhooks/email/events")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(unknown)
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["skipped"], json!(true));
        assert_eq!(body["reason"], json!("unhandled_event_type"));
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }

    #[actix_web::test]
    async fn events_malformed_payload_is_400() {
        let (state, recorded) = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/webhooks/email/events")
            .insert_header(("X-API-Key", "test-key"))
            .set_json(json!({ "type": 12345 }))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 400);
        assert!(recorded.lock().expect("recorded lock poisoned").is_empty());
    }
}
