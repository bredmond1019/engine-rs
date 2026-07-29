//! `channel_transport` — the injectable egress seam `ActionDispatchNode`
//! (`EN.6.A` task 3) calls to deliver a `CONTENT_PIPELINE` run's outbound
//! actions (a digest reply, a fire-and-forget no-op, or a workflow-trigger
//! chain) to the channel that originated the run.
//!
//! Patterned on `crate::nodes::http_post`'s `HttpPost` seam: a trait so
//! production code reaches for a real implementation while tests inject a
//! stub that records every `OutboundAction` it was handed — no live network
//! call, no real channel API, in the gated `cargo test` suite. Per THE
//! BOUNDARY TEST (`CLAUDE.md`), this seam only sends; the real channel
//! adapters (Slack/Telegram/WhatsApp/Email) land in `EN.6.B`–`EN.6.D`.
//!
//! Source of truth: `planning/EN.5.A-content-pipeline/architecture.md` §3.3.

use std::sync::{Arc, Mutex};

use engine_contract::envelope::{ChannelType, ReplyContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dispatch::Dispatcher;

use super::http_post::HttpPost;

/// Env var `channel_transport_live` reads for the `X-API-Key` header the
/// workflow-trigger self-POST to `/events/` must carry (the endpoint 401s
/// without it — `engine-serve`'s `check_api_key`). Empty/unset resolves to
/// `""`; a caller that needs a different key (e.g. `EN.6.A` task 5's serve
/// wiring, matching `AppState::api_key`) overrides it via
/// [`WorkflowTriggerDispatch::with_api_key`].
const EVENTS_API_KEY_ENV: &str = "ENGINE_EVENTS_API_KEY";

/// A `TriggerWorkflow` chain at or beyond this many hops is refused rather
/// than sent, so an A -> B -> A trigger cycle terminates instead of
/// recursing forever. Each hop's outgoing `data.chain_depth` is the
/// previous hop's `chain_depth` (default `0` if absent) plus one.
const MAX_CHAIN_DEPTH: u64 = 8;

/// The typed outbound payload an `OutboundAction` carries. Internally
/// tagged so stored actions/receipts stay self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboundBody {
    Message {
        text: String,
    },
    Digest {
        markdown: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        html: Option<String>,
    },
    TriggerWorkflow {
        workflow_type: String,
        event: Value,
    },
}

/// One egress send request: where it's going (`channel_type` /
/// `reply_context`, opaque to the pipeline) and what to send (`body`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboundAction {
    pub channel_type: ChannelType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_context: Option<ReplyContext>,
    pub body: OutboundBody,
}

/// The result of attempting to send an `OutboundAction`. A failed send is
/// recorded as `delivered: false` — it never fails the run (the Brain write
/// already happened by the time `ActionDispatchNode` runs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelSendReceipt {
    pub delivered: bool,
    pub detail: String,
}

/// Injectable egress seam — the outbound mirror of `HttpPost`. One impl per
/// channel; adapters land in `EN.6.B`–`EN.6.D`. `send` returns `Err` only
/// for seam-level failures the caller should treat as unsendable (the
/// `ActionDispatchNode` still turns that into a `delivered: false` receipt
/// rather than failing the run).
#[async_trait::async_trait]
pub trait ChannelTransport: Send + Sync {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String>;
}

/// The local-dev placeholder `/events/` URL every `ChannelTransport`-backed
/// dispatch node (`ActionDispatchNode`, `ResearchIngressDispatchNode`)
/// defaults to until its deployment wires `ENGINE_EVENTS_URL`
/// (`engine-serve::workflows::events_url_from_env`). Centralized here so
/// those nodes share one literal instead of each declaring its own private
/// copy.
pub const DEFAULT_EVENTS_URL: &str = "http://localhost:8080/events/";

/// Fold a [`ChannelTransport::send`] result into a [`ChannelSendReceipt`],
/// turning a seam-level `Err` into a `delivered: false` receipt instead of
/// propagating it — the shared "never fail the run on a transport error"
/// behavior every dispatch node built on this seam relies on.
#[must_use]
pub fn receipt_from_send_result(result: Result<ChannelSendReceipt, String>) -> ChannelSendReceipt {
    match result {
        Ok(receipt) => receipt,
        Err(detail) => ChannelSendReceipt {
            delivered: false,
            detail,
        },
    }
}

/// Live default until the human-channel adapters land: `TriggerWorkflow`
/// bodies are posted to engine-serve's existing `POST /events/` endpoint
/// via the `HttpPost` seam (no engine-core -> engine-serve dependency — the
/// trigger goes over HTTP); every other body kind (human-channel replies)
/// errors naming the owning block, since no adapter exists yet.
///
/// When an in-process [`Dispatcher`] is injected via [`Self::with_dispatcher`]
/// and it has the target `workflow_type` registered, `send` prefers running
/// it directly over the loopback HTTP POST — no network hop, no worker held
/// on the far side. `EN.5.F` already makes `/events/` return before the
/// child run finishes, so the HTTP path is a correct (if slightly costlier)
/// fallback for any `workflow_type` the injected `Dispatcher` doesn't know
/// about (an out-of-process target, or no `Dispatcher` injected at all).
pub struct WorkflowTriggerDispatch {
    http: Arc<dyn HttpPost>,
    events_url: String,
    /// `X-API-Key` header value sent with the loopback POST. Defaults to
    /// [`EVENTS_API_KEY_ENV`]'s value at construction time.
    api_key: String,
    /// In-process dispatch preferred over the loopback HTTP POST when
    /// present and the `workflow_type` is registered on it.
    dispatcher: Option<Arc<Dispatcher>>,
}

impl WorkflowTriggerDispatch {
    /// Build a `WorkflowTriggerDispatch` targeting `events_url` (e.g.
    /// `"http://localhost:8080/events/"`) with the default live `HttpPost`
    /// and the `X-API-Key` read from [`EVENTS_API_KEY_ENV`] (empty if unset
    /// — override with [`Self::with_api_key`]).
    #[must_use]
    pub fn new(events_url: impl Into<String>) -> Self {
        Self {
            http: super::http_post::http_post_live(),
            events_url: events_url.into(),
            api_key: std::env::var(EVENTS_API_KEY_ENV).unwrap_or_default(),
            dispatcher: None,
        }
    }

    /// Override the `HttpPost` seam (tests inject a `StubHttpPost`).
    #[must_use]
    pub fn with_http_post(mut self, http: Arc<dyn HttpPost>) -> Self {
        self.http = http;
        self
    }

    /// Override the target `/events/` URL.
    #[must_use]
    pub fn with_url(mut self, events_url: impl Into<String>) -> Self {
        self.events_url = events_url.into();
        self
    }

    /// Override the `X-API-Key` header value sent with the loopback POST.
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Inject an in-process `Dispatcher` to prefer over the loopback HTTP
    /// POST for any `workflow_type` it has registered.
    #[must_use]
    pub fn with_dispatcher(mut self, dispatcher: Arc<Dispatcher>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }
}

#[async_trait::async_trait]
impl ChannelTransport for WorkflowTriggerDispatch {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String> {
        let (workflow_type, event) = match &action.body {
            OutboundBody::TriggerWorkflow {
                workflow_type,
                event,
            } => (workflow_type.clone(), event.clone()),
            other => {
                return Err(format!(
                    "WorkflowTriggerDispatch only handles OutboundBody::TriggerWorkflow, \
                     got {other:?}"
                ));
            }
        };

        let chain_depth = event
            .get("chain_depth")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if chain_depth >= MAX_CHAIN_DEPTH {
            return Err(format!(
                "workflow-trigger chain depth {chain_depth} is at or beyond the cap of \
                 {MAX_CHAIN_DEPTH}; refusing to dispatch '{workflow_type}' to avoid an \
                 unbounded chain (e.g. an A -> B -> A cycle)"
            ));
        }

        // Carry the event through untouched (any `parent_run_id` /
        // `envelope_id` the caller already stamped travels with it as-is —
        // this seam never fabricates an id it wasn't given), bumping only
        // `chain_depth` so the next hop's cap check sees this hop counted.
        let mut data = event;
        if !data.is_object() {
            data = serde_json::json!({ "event": data });
        }
        data["chain_depth"] = serde_json::json!(chain_depth + 1);

        if let Some(dispatcher) = &self.dispatcher {
            if let Ok(workflow) = dispatcher.dispatch_with_event(&workflow_type, &data) {
                // Fire-and-forget, mirroring `/events/`'s own `EN.5.F`
                // contract: the caller gets a receipt for the handoff, not
                // for the child run's outcome. `Workflow::run` takes an
                // `OnProgress<'_> = Box<dyn FnMut(&TaskContext)>` with no
                // `Send` bound (`workflow.rs`), so its future is `!Send` and
                // cannot be held across an await point inside this
                // `Send`-bound `async_trait` method (or handed to
                // `tokio::spawn`, which requires `Send`). `spawn_blocking`'s
                // closure only needs `workflow`/`run_data` to cross the
                // thread boundary (both are `Send`); the `!Send`
                // `OnProgress` box and the run's future are built and driven
                // entirely on that one blocking thread via a fresh
                // current-thread runtime, so the `Send` requirement never
                // touches them.
                let run_data = data.clone();
                let run_workflow_type = workflow_type.clone();
                tokio::task::spawn_blocking(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(_err) => return,
                    };
                    runtime.block_on(async move {
                        let on_progress: crate::OnProgress<'_> = Box::new(|_ctx| {});
                        let _ = workflow.run(run_data, on_progress).await;
                    });
                });

                return Ok(ChannelSendReceipt {
                    delivered: true,
                    detail: format!(
                        "dispatched '{run_workflow_type}' in-process (fire-and-forget)"
                    ),
                });
            }
            // Not registered on the injected `Dispatcher` (an out-of-process
            // target) — fall through to the loopback HTTP path below.
        }

        let payload = serde_json::json!({
            "workflow_type": workflow_type,
            "data": data,
        });

        let response = self
            .http
            .post_with_headers(
                &self.events_url,
                payload,
                &[("X-API-Key", self.api_key.as_str())],
            )
            .await?;

        Ok(ChannelSendReceipt {
            delivered: true,
            detail: format!(
                "triggered '{workflow_type}' at {} (status {})",
                self.events_url, response.status
            ),
        })
    }
}

/// Returns an error naming the `EN.6.x` block that will own the adapter for
/// `channel_type`, so an unwired human channel fails legibly instead of
/// silently no-oping.
fn unwired_channel_error(channel_type: ChannelType) -> String {
    let owner = match channel_type {
        ChannelType::Slack => "EN.6.B",
        ChannelType::Telegram | ChannelType::WhatsApp => "EN.6.C",
        ChannelType::Email => "EN.6.D",
        _ => "a future EN.6.x block",
    };
    format!(
        "no ChannelTransport adapter wired for channel_type={channel_type:?} yet \
         (owning block: {owner})"
    )
}

/// Test seam: records every `OutboundAction` it is handed and returns a
/// configurable success/failure receipt, so unit tests (this module,
/// `action_dispatch.rs`, the e2e test) can assert on both the outbound
/// action and how the node handles a failure.
#[derive(Default)]
pub struct StubChannelTransport {
    pub succeeding: bool,
    pub calls: Mutex<Vec<OutboundAction>>,
}

impl StubChannelTransport {
    /// A stub that always succeeds.
    #[must_use]
    pub fn succeeding() -> Self {
        Self {
            succeeding: true,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub that always fails (transport error, not a run failure).
    #[must_use]
    pub fn failing() -> Self {
        Self {
            succeeding: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Every `OutboundAction` recorded so far, in send order.
    #[must_use]
    pub fn calls(&self) -> Vec<OutboundAction> {
        self.calls.lock().unwrap().clone()
    }

    /// The most recently recorded `OutboundAction`, if any.
    #[must_use]
    pub fn last_call(&self) -> Option<OutboundAction> {
        self.calls.lock().unwrap().last().cloned()
    }
}

#[async_trait::async_trait]
impl ChannelTransport for StubChannelTransport {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String> {
        self.calls.lock().unwrap().push(action.clone());
        if self.succeeding {
            Ok(ChannelSendReceipt {
                delivered: true,
                detail: "stub delivered".to_string(),
            })
        } else {
            Err("stub configured to fail".to_string())
        }
    }
}

/// A `ChannelTransport` that always errors, naming the `EN.6.x` block that
/// will own a real adapter for each human channel. Used by
/// `channel_transport_live` for every channel that isn't `WorkflowTrigger`
/// (there is no adapter yet).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnwiredChannelTransport;

#[async_trait::async_trait]
impl ChannelTransport for UnwiredChannelTransport {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String> {
        Err(unwired_channel_error(action.channel_type))
    }
}

/// The live default `ChannelTransport`: routes on `action.channel_type`,
/// sending `WorkflowTrigger` actions through `WorkflowTriggerDispatch` and
/// every other (human) channel through `UnwiredChannelTransport`, since no
/// real adapter exists for those yet. Built by [`channel_transport_live`].
pub struct LiveChannelTransport {
    trigger: WorkflowTriggerDispatch,
    unwired: UnwiredChannelTransport,
}

#[async_trait::async_trait]
impl ChannelTransport for LiveChannelTransport {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String> {
        match action.channel_type {
            ChannelType::WorkflowTrigger => self.trigger.send(action).await,
            _ => self.unwired.send(action).await,
        }
    }
}

/// Build the live default `ChannelTransport` (`ActionDispatchNode`'s
/// default, `EN.6.A` task 4): `TriggerWorkflow` bodies are posted to
/// `events_url` via `WorkflowTriggerDispatch` over the real `ReqwestHttpPost`
/// seam (no engine-core -> engine-serve dependency); every other
/// `channel_type` routes to `UnwiredChannelTransport` until its `EN.6.B`–`D`
/// adapter lands.
#[must_use]
pub fn channel_transport_live(events_url: impl Into<String>) -> Arc<dyn ChannelTransport> {
    Arc::new(LiveChannelTransport {
        trigger: WorkflowTriggerDispatch::new(events_url),
        unwired: UnwiredChannelTransport,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn digest_action(channel_type: ChannelType) -> OutboundAction {
        OutboundAction {
            channel_type,
            reply_context: Some(ReplyContext {
                thread_id: Some("t-1".to_string()),
                conversation_id: None,
                channel_token: Some("c-1".to_string()),
            }),
            body: OutboundBody::Digest {
                markdown: "# Digest".to_string(),
                html: None,
            },
        }
    }

    #[test]
    fn outbound_action_round_trips_through_serde_for_every_body_kind() {
        let actions = vec![
            OutboundAction {
                channel_type: ChannelType::Slack,
                reply_context: None,
                body: OutboundBody::Message {
                    text: "hello".to_string(),
                },
            },
            digest_action(ChannelType::Email),
            OutboundAction {
                channel_type: ChannelType::WorkflowTrigger,
                reply_context: None,
                body: OutboundBody::TriggerWorkflow {
                    workflow_type: "CONTENT_PIPELINE".to_string(),
                    event: json!({"envelope": {"envelope_id": "e-1"}}),
                },
            },
        ];

        for action in actions {
            let serialized = serde_json::to_string(&action).expect("serialize OutboundAction");
            let deserialized: OutboundAction =
                serde_json::from_str(&serialized).expect("deserialize OutboundAction");
            assert_eq!(deserialized, action);
        }
    }

    #[test]
    fn outbound_body_tags_are_internally_tagged_by_kind() {
        let body = OutboundBody::Digest {
            markdown: "# hi".to_string(),
            html: Some("<h1>hi</h1>".to_string()),
        };
        let value = serde_json::to_value(&body).expect("serialize OutboundBody");
        assert_eq!(value["kind"], json!("digest"));
        assert_eq!(value["markdown"], json!("# hi"));
        assert_eq!(value["html"], json!("<h1>hi</h1>"));
    }

    #[tokio::test]
    async fn stub_records_every_action_it_is_sent() {
        let stub = StubChannelTransport::succeeding();
        let action = digest_action(ChannelType::Slack);

        let receipt = stub.send(&action).await.expect("succeeding stub sends");

        assert!(receipt.delivered);
        assert_eq!(stub.calls(), vec![action.clone()]);
        assert_eq!(stub.last_call(), Some(action));
    }

    #[tokio::test]
    async fn stub_records_calls_even_when_configured_to_fail() {
        let stub = StubChannelTransport::failing();
        let action = digest_action(ChannelType::Telegram);

        let result = stub.send(&action).await;

        assert_eq!(result, Err("stub configured to fail".to_string()));
        assert_eq!(stub.calls().len(), 1);
    }

    #[tokio::test]
    async fn unwired_transport_errors_naming_the_owning_block_per_channel() {
        let transport = UnwiredChannelTransport;

        let slack = transport
            .send(&digest_action(ChannelType::Slack))
            .await
            .unwrap_err();
        assert!(slack.contains("EN.6.B"), "slack error was: {slack}");

        let telegram = transport
            .send(&digest_action(ChannelType::Telegram))
            .await
            .unwrap_err();
        assert!(
            telegram.contains("EN.6.C"),
            "telegram error was: {telegram}"
        );

        let whatsapp = transport
            .send(&digest_action(ChannelType::WhatsApp))
            .await
            .unwrap_err();
        assert!(
            whatsapp.contains("EN.6.C"),
            "whatsapp error was: {whatsapp}"
        );

        let email = transport
            .send(&digest_action(ChannelType::Email))
            .await
            .unwrap_err();
        assert!(email.contains("EN.6.D"), "email error was: {email}");
    }

    #[test]
    fn workflow_trigger_dispatch_builders_do_not_panic() {
        let _dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_url("http://localhost:9090/events/")
            .with_api_key("test-key")
            .with_http_post(Arc::new(super::super::http_post::StubHttpPost::succeeding(
                json!({"ok": true}),
            )));
    }

    fn trigger_action(workflow_type: &str, event: Value) -> OutboundAction {
        OutboundAction {
            channel_type: ChannelType::WorkflowTrigger,
            reply_context: None,
            body: OutboundBody::TriggerWorkflow {
                workflow_type: workflow_type.to_string(),
                event,
            },
        }
    }

    #[tokio::test]
    async fn trigger_dispatch_posts_url_and_payload_to_the_stub() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_api_key("test-key")
            .with_http_post(stub.clone());

        let receipt = dispatch
            .send(&trigger_action(
                "CONTENT_PIPELINE",
                json!({"envelope_id": "env-1"}),
            ))
            .await
            .expect("trigger dispatch should succeed");

        assert!(receipt.delivered);

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "http://localhost:8080/events/");
        assert_eq!(
            body,
            json!({
                "workflow_type": "CONTENT_PIPELINE",
                "data": {"envelope_id": "env-1", "chain_depth": 1},
            })
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_sends_the_x_api_key_header() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_api_key("super-secret")
            .with_http_post(stub.clone());

        dispatch
            .send(&trigger_action("CONTENT_PIPELINE", json!({})))
            .await
            .expect("trigger dispatch should succeed");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(
            headers.contains(&("X-API-Key".to_string(), "super-secret".to_string())),
            "headers were: {headers:?}"
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_http_failure_maps_to_err() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::failing(
            "connection refused",
        ));
        let dispatch =
            WorkflowTriggerDispatch::new("http://localhost:8080/events/").with_http_post(stub);

        let result = dispatch
            .send(&trigger_action("CONTENT_PIPELINE", json!({})))
            .await;

        assert_eq!(result, Err("connection refused".to_string()));
    }

    #[tokio::test]
    async fn trigger_dispatch_rejects_non_trigger_bodies() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatch =
            WorkflowTriggerDispatch::new("http://localhost:8080/events/").with_http_post(stub);

        let result = dispatch
            .send(&digest_action(ChannelType::WorkflowTrigger))
            .await;

        assert!(result.is_err(), "a Digest body should not be handled");
    }

    #[tokio::test]
    async fn trigger_dispatch_refuses_a_chain_at_the_depth_cap() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_http_post(stub.clone());

        let result = dispatch
            .send(&trigger_action(
                "CONTENT_PIPELINE",
                json!({"chain_depth": MAX_CHAIN_DEPTH}),
            ))
            .await;

        assert!(result.is_err(), "a chain at the cap should be refused");
        assert!(
            stub.last_call().is_none(),
            "a refused chain should never reach the HTTP seam"
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_increments_chain_depth_below_the_cap() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_http_post(stub.clone());

        dispatch
            .send(&trigger_action(
                "CONTENT_PIPELINE",
                json!({"chain_depth": MAX_CHAIN_DEPTH - 1}),
            ))
            .await
            .expect("a chain one hop below the cap should be sent");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["data"]["chain_depth"], json!(MAX_CHAIN_DEPTH));
    }

    #[tokio::test]
    async fn channel_transport_live_routes_trigger_actions_to_the_events_url() {
        // The live constructor wires a real `ReqwestHttpPost`, so we only
        // assert on routing here (WorkflowTrigger -> attempted network call
        // that fails fast against an unroutable host) rather than a
        // successful send, keeping this test hermetic.
        let transport = channel_transport_live("http://127.0.0.1:0/events/");

        let result = transport
            .send(&trigger_action("CONTENT_PIPELINE", json!({})))
            .await;

        assert!(
            result.is_err(),
            "an unroutable events_url should surface as a transport error, \
             not a silent no-op"
        );
    }

    #[tokio::test]
    async fn channel_transport_live_routes_human_channels_to_unwired() {
        let transport = channel_transport_live("http://127.0.0.1:0/events/");

        let result = transport.send(&digest_action(ChannelType::Slack)).await;

        let err = result.unwrap_err();
        assert!(err.contains("EN.6.B"), "error was: {err}");
    }

    struct MarkerNode;

    #[async_trait::async_trait]
    impl crate::Node for MarkerNode {
        async fn process(
            &self,
            mut ctx: engine_contract::TaskContext,
        ) -> Result<engine_contract::TaskContext, crate::NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    fn marker_schema(workflow_type: &str) -> crate::WorkflowSchema {
        let nodes = std::collections::HashMap::from([(
            "MarkerNode".to_string(),
            crate::NodeConfig::new("MarkerNode", vec![]),
        )]);
        crate::WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
    }

    fn dispatcher_with_marker_workflow(workflow_type: &str) -> Dispatcher {
        let mut dispatcher = Dispatcher::new();
        let schema = marker_schema(workflow_type);
        dispatcher.register(
            schema.clone(),
            Box::new(move |_event: &Value| {
                let mut registry = crate::NodeRegistry::new();
                registry.register(Box::new(MarkerNode));
                Ok(crate::Workflow::new(registry, schema.clone()))
            }),
        );
        dispatcher
    }

    #[tokio::test]
    async fn trigger_dispatch_prefers_the_injected_dispatcher_over_http() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        let dispatcher = Arc::new(dispatcher_with_marker_workflow("MARKER_WORKFLOW"));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_http_post(stub.clone())
            .with_dispatcher(dispatcher);

        let receipt = dispatch
            .send(&trigger_action("MARKER_WORKFLOW", json!({})))
            .await
            .expect("in-process dispatch should succeed");

        assert!(receipt.delivered);
        assert!(
            stub.last_call().is_none(),
            "an in-process dispatch should never fall through to HTTP"
        );
    }

    #[tokio::test]
    async fn trigger_dispatch_falls_back_to_http_when_dispatcher_lacks_the_workflow_type() {
        let stub = Arc::new(super::super::http_post::StubHttpPost::succeeding(
            json!({"ok": true}),
        ));
        // A Dispatcher is injected, but it only knows a different
        // workflow_type — the target here is an out-of-process type, so the
        // HTTP fallback must still fire.
        let dispatcher = Arc::new(dispatcher_with_marker_workflow("SOME_OTHER_WORKFLOW"));
        let dispatch = WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_http_post(stub.clone())
            .with_dispatcher(dispatcher);

        let receipt = dispatch
            .send(&trigger_action("CONTENT_PIPELINE", json!({})))
            .await
            .expect("HTTP fallback should succeed");

        assert!(receipt.delivered);
        assert!(
            stub.last_call().is_some(),
            "an unregistered workflow_type should fall through to HTTP"
        );
    }
}
