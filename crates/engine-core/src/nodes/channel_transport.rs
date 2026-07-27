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

use super::http_post::HttpPost;

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

/// Live default until the human-channel adapters land: `TriggerWorkflow`
/// bodies are posted to engine-serve's existing `POST /events/` endpoint
/// via the `HttpPost` seam (no engine-core -> engine-serve dependency — the
/// trigger goes over HTTP); every other body kind (human-channel replies)
/// errors naming the owning block, since no adapter exists yet.
pub struct WorkflowTriggerDispatch {
    #[allow(dead_code)]
    http: Arc<dyn HttpPost>,
    #[allow(dead_code)]
    events_url: String,
}

impl WorkflowTriggerDispatch {
    /// Build a `WorkflowTriggerDispatch` targeting `events_url` (e.g.
    /// `"http://localhost:8080/events/"`) with the default live `HttpPost`.
    #[must_use]
    pub fn new(events_url: impl Into<String>) -> Self {
        Self {
            http: super::http_post::http_post_live(),
            events_url: events_url.into(),
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
            .with_http_post(Arc::new(super::super::http_post::StubHttpPost::succeeding(
                json!({"ok": true}),
            )));
    }
}
