//! `EmailChannelTransport` — the `ChannelTransport` impl (`EN.6.B` task 2)
//! that sends outbound mail through the [Resend](https://resend.com) HTTP
//! API, modeled directly on `channel_transport::WorkflowTriggerDispatch`'s
//! construction/builder shape and sending over the same injectable
//! [`HttpPost`] seam via `post_with_headers`.
//!
//! Credentials come only from the environment ([`RESEND_API_KEY_ENV`]) —
//! never a literal in source, tests, or fixtures — read at construction
//! time and validated (non-empty) at send time, so an unset/empty key
//! produces a descriptive `Err` naming the env var rather than a request
//! with an empty `Authorization` header.
//!
//! The sender address is deployment topology, not a policy knob (see
//! `planning/EN.6.B-email-adapter/tasks.md`'s Notes → *Why there is no
//! policy surface here*): it defaults to [`DEFAULT_EMAIL_FROM`], overridable
//! via [`EmailChannelTransport::with_sender`] or [`EMAIL_FROM_ENV`].
//!
//! Bounce/delivery correlation (`EN.6.B` design decision 1): any
//! `metadata["opportunity_slug"]` on the `OutboundAction` (`EN.6.B` task 1)
//! is echoed onto the Resend payload as a `tags` entry; the webhook handler
//! (task 6) reads it back off the delivery/bounce event to correlate
//! without an address lookup or corpus scan.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::nodes::channel_transport::{
    ChannelSendReceipt, ChannelTransport, OutboundAction, OutboundBody,
};
use crate::nodes::http_post::HttpPost;

/// Env var `EmailChannelTransport::new` reads the Resend API key from at
/// construction time. Never committed anywhere as a literal.
pub const RESEND_API_KEY_ENV: &str = "RESEND_API_KEY";

/// Env var overriding the default `from` sender address ([`DEFAULT_EMAIL_FROM`]).
pub const EMAIL_FROM_ENV: &str = "ENGINE_EMAIL_FROM";

/// The default `from` sender address — the verified `mail.bastiel.com.br`
/// domain (`HQ.3.B`). Deployment topology, not a policy knob; override with
/// [`EmailChannelTransport::with_sender`] or [`EMAIL_FROM_ENV`].
pub const DEFAULT_EMAIL_FROM: &str = "bastion@mail.bastiel.com.br";

/// The Resend "send an email" endpoint.
const RESEND_SEND_URL: &str = "https://api.resend.com/emails";

/// The metadata key `EN.6.B` gives meaning to: echoed onto the Resend
/// payload as a `tags` entry so the delivery/bounce webhook can correlate
/// the event back to an opportunity. Populated by `EN.6.H2`; this module
/// only plumbs it.
const OPPORTUNITY_SLUG_METADATA_KEY: &str = "opportunity_slug";

/// The `ChannelTransport` adapter for `ChannelType::Email`: sends through
/// the Resend HTTP API over the injectable [`HttpPost`] seam
/// (`post_with_headers`), threading replies via `ReplyContext` and echoing
/// `metadata["opportunity_slug"]` onto Resend `tags`.
pub struct EmailChannelTransport {
    http: Arc<dyn HttpPost>,
    /// The Resend API key, read from [`RESEND_API_KEY_ENV`] at construction
    /// (empty if unset — `send` rejects an empty key rather than issuing a
    /// request with a blank `Authorization` header).
    api_key: String,
    /// The `from` sender address.
    sender: String,
}

impl EmailChannelTransport {
    /// Build an `EmailChannelTransport` with the default live `HttpPost`,
    /// the API key read from [`RESEND_API_KEY_ENV`] (empty if unset —
    /// `send` then errors rather than silently no-oping), and the sender
    /// read from [`EMAIL_FROM_ENV`] (falling back to [`DEFAULT_EMAIL_FROM`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: super::super::http_post::http_post_live(),
            api_key: std::env::var(RESEND_API_KEY_ENV).unwrap_or_default(),
            sender: std::env::var(EMAIL_FROM_ENV)
                .unwrap_or_else(|_| DEFAULT_EMAIL_FROM.to_string()),
        }
    }

    /// Override the `HttpPost` seam (tests inject a `StubHttpPost`).
    #[must_use]
    pub fn with_http_post(mut self, http: Arc<dyn HttpPost>) -> Self {
        self.http = http;
        self
    }

    /// Override the Resend API key (tests inject a fake key — never a real
    /// one — so `Authorization` header behavior is assertable without
    /// reading the environment).
    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Override the `from` sender address.
    #[must_use]
    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = sender.into();
        self
    }
}

impl Default for EmailChannelTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelTransport for EmailChannelTransport {
    async fn send(&self, action: &OutboundAction) -> Result<ChannelSendReceipt, String> {
        if self.api_key.trim().is_empty() {
            return Err(format!(
                "EmailChannelTransport: {RESEND_API_KEY_ENV} is unset/empty; \
                 refusing to send with no Authorization credential"
            ));
        }

        let to = action
            .reply_context
            .as_ref()
            .and_then(|reply| {
                reply
                    .channel_token
                    .clone()
                    .or_else(|| reply.conversation_id.clone())
            })
            .ok_or_else(|| {
                "EmailChannelTransport: no recipient — reply_context.channel_token and \
                 reply_context.conversation_id are both absent"
                    .to_string()
            })?;

        let (text, html) = match &action.body {
            OutboundBody::Message { text } => (Some(text.clone()), None),
            OutboundBody::Digest { markdown, html } => (Some(markdown.clone()), html.clone()),
            OutboundBody::TriggerWorkflow { .. } => {
                return Err(format!(
                    "EmailChannelTransport only handles OutboundBody::Message and \
                     OutboundBody::Digest, got {:?}",
                    action.body
                ));
            }
        };

        let mut payload = json!({
            "from": self.sender,
            "to": [to],
        });

        if let Some(html) = &html {
            payload["html"] = json!(html);
        }
        if let Some(text) = &text {
            payload["text"] = json!(text);
        }

        if let Some(thread_id) = action
            .reply_context
            .as_ref()
            .and_then(|reply| reply.thread_id.as_ref())
        {
            payload["headers"] = json!({
                "In-Reply-To": thread_id,
                "References": thread_id,
            });
        }

        if let Some(slug) = action.metadata_value(OPPORTUNITY_SLUG_METADATA_KEY) {
            payload["tags"] = json!([{ "name": OPPORTUNITY_SLUG_METADATA_KEY, "value": slug }]);
        }

        let auth_header = format!("Bearer {}", self.api_key);
        let response = self
            .http
            .post_with_headers(RESEND_SEND_URL, payload, &[("Authorization", &auth_header)])
            .await?;

        Ok(ChannelSendReceipt {
            delivered: true,
            detail: format!("sent via Resend (status {})", response.status),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::channel_transport::OutboundAction;
    use crate::nodes::http_post::StubHttpPost;
    use engine_contract::envelope::{ChannelType, ReplyContext};

    fn reply(thread_id: Option<&str>, channel_token: Option<&str>) -> Option<ReplyContext> {
        Some(ReplyContext {
            thread_id: thread_id.map(str::to_string),
            conversation_id: None,
            channel_token: channel_token.map(str::to_string),
        })
    }

    fn digest_action(reply_context: Option<ReplyContext>) -> OutboundAction {
        OutboundAction::new(
            ChannelType::Email,
            reply_context,
            OutboundBody::Digest {
                markdown: "# Weekly digest".to_string(),
                html: None,
            },
        )
    }

    fn transport_with_stub(stub: Arc<StubHttpPost>) -> EmailChannelTransport {
        EmailChannelTransport::new()
            .with_api_key("test-key")
            .with_http_post(stub)
    }

    #[tokio::test]
    async fn send_carries_the_mail_bastiel_sender() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&digest_action(reply(None, Some("someone@example.com"))))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["from"], json!(DEFAULT_EMAIL_FROM));
        assert!(
            DEFAULT_EMAIL_FROM.ends_with("mail.bastiel.com.br"),
            "default sender must be on mail.bastiel.com.br: {DEFAULT_EMAIL_FROM}"
        );
    }

    #[tokio::test]
    async fn send_sets_the_authorization_header_from_the_api_key() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = EmailChannelTransport::new()
            .with_api_key("super-secret")
            .with_http_post(stub.clone());

        transport
            .send(&digest_action(reply(None, Some("someone@example.com"))))
            .await
            .expect("send should succeed");

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert!(
            headers.contains(&(
                "Authorization".to_string(),
                "Bearer super-secret".to_string()
            )),
            "headers were: {headers:?}"
        );
    }

    #[tokio::test]
    async fn send_with_empty_api_key_errors_naming_the_env_var_and_issues_no_request() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = EmailChannelTransport::new()
            .with_api_key("")
            .with_http_post(stub.clone());

        let result = transport
            .send(&digest_action(reply(None, Some("someone@example.com"))))
            .await;

        let err = result.unwrap_err();
        assert!(
            err.contains(RESEND_API_KEY_ENV),
            "error should name the env var: {err}"
        );
        assert!(
            stub.last_call().is_none(),
            "no request should be issued with an empty key"
        );
    }

    #[tokio::test]
    async fn thread_id_produces_in_reply_to_and_references_headers() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&digest_action(reply(
                Some("msg-42@resend"),
                Some("someone@example.com"),
            )))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["headers"]["In-Reply-To"], json!("msg-42@resend"));
        assert_eq!(body["headers"]["References"], json!("msg-42@resend"));
    }

    #[tokio::test]
    async fn message_body_maps_to_plain_text() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&OutboundAction::new(
                ChannelType::Email,
                reply(None, Some("someone@example.com")),
                OutboundBody::Message {
                    text: "hello there".to_string(),
                },
            ))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["text"], json!("hello there"));
        assert!(body.get("html").is_none());
    }

    #[tokio::test]
    async fn digest_with_html_uses_html_body() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&OutboundAction::new(
                ChannelType::Email,
                reply(None, Some("someone@example.com")),
                OutboundBody::Digest {
                    markdown: "# hi".to_string(),
                    html: Some("<h1>hi</h1>".to_string()),
                },
            ))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["html"], json!("<h1>hi</h1>"));
    }

    #[tokio::test]
    async fn digest_without_html_falls_back_to_markdown_as_text() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&OutboundAction::new(
                ChannelType::Email,
                reply(None, Some("someone@example.com")),
                OutboundBody::Digest {
                    markdown: "# hi".to_string(),
                    html: None,
                },
            ))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["text"], json!("# hi"));
        assert!(body.get("html").is_none());
    }

    #[tokio::test]
    async fn trigger_workflow_body_is_rejected() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        let result = transport
            .send(&OutboundAction::new(
                ChannelType::Email,
                reply(None, Some("someone@example.com")),
                OutboundBody::TriggerWorkflow {
                    workflow_type: "CONTENT_PIPELINE".to_string(),
                    event: json!({}),
                },
            ))
            .await;

        assert!(result.is_err(), "a TriggerWorkflow body should be rejected");
        assert!(
            stub.last_call().is_none(),
            "a rejected body should never reach the HTTP seam"
        );
    }

    #[tokio::test]
    async fn opportunity_slug_metadata_becomes_a_resend_tag() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        let action = digest_action(reply(None, Some("someone@example.com")))
            .with_metadata("opportunity_slug", "acme-corp");

        transport.send(&action).await.expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(
            body["tags"],
            json!([{ "name": "opportunity_slug", "value": "acme-corp" }])
        );
    }

    #[tokio::test]
    async fn no_opportunity_slug_metadata_means_no_tags_key() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        transport
            .send(&digest_action(reply(None, Some("someone@example.com"))))
            .await
            .expect("send should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert!(body.get("tags").is_none());
    }

    #[tokio::test]
    async fn missing_recipient_errors_descriptively() {
        let stub = Arc::new(StubHttpPost::succeeding(json!({"id": "email-1"})));
        let transport = transport_with_stub(stub.clone());

        let result = transport.send(&digest_action(None)).await;

        assert!(result.is_err(), "a missing recipient should error");
        assert!(
            stub.last_call().is_none(),
            "no request should be issued with no resolvable recipient"
        );
    }

    #[tokio::test]
    async fn transport_failure_surfaces_as_err() {
        let stub = Arc::new(StubHttpPost::failing("connection refused"));
        let transport = transport_with_stub(stub);

        let result = transport
            .send(&digest_action(reply(None, Some("someone@example.com"))))
            .await;

        assert_eq!(result, Err("connection refused".to_string()));
    }

    #[test]
    fn builders_do_not_panic() {
        let _transport = EmailChannelTransport::new()
            .with_api_key("test-key")
            .with_sender("test@mail.bastiel.com.br")
            .with_http_post(Arc::new(StubHttpPost::succeeding(json!({"id": "e-1"}))));
    }

    #[tokio::test]
    #[ignore = "live send: needs RESEND_API_KEY + EMAIL_TEST_TO"]
    async fn live_send() {
        let to = std::env::var("EMAIL_TEST_TO")
            .expect("EMAIL_TEST_TO must be set for the live-send test");
        let transport = EmailChannelTransport::new();

        let receipt = transport
            .send(&OutboundAction::new(
                ChannelType::Email,
                reply(None, Some(&to)),
                OutboundBody::Message {
                    text: "EN.6.B live-send verification".to_string(),
                },
            ))
            .await
            .expect("live send should succeed with a real RESEND_API_KEY");

        assert!(receipt.delivered);
    }
}
