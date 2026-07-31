//! Inbound-mail parsing (`EN.6.B` task 4): a Resend inbound payload ->
//! `IngressEnvelope{channel_type: Email, source: ChannelMessage}`.
//!
//! Pure and total: no network, no filesystem, no clock beyond the
//! documented fallback below. The `envelope_id` is derived deterministically
//! from the message id, following the `research_agent::ingress_dispatch::
//! resolve_envelope_id` precedent (a stable string prefix, never
//! `Uuid::new_v4()`), so the same payload always yields the same id.

use engine_contract::envelope::{ChannelType, IngressEnvelope, ReplyContext, SourcePayload};
use serde_json::Value;

/// Parse a Resend inbound-mail webhook payload into an [`IngressEnvelope`].
///
/// Field mapping:
/// - `channel_type`: always `ChannelType::Email`.
/// - `sender_id`: `payload["from"]` as a string; absence is an `Err`.
/// - `source`: `SourcePayload::ChannelMessage { text, attachments }` — `text`
///   prefers `payload["text"]`, falling back to `payload["html"]`;
///   `attachments` from `payload["attachments"]` as an array, else empty.
///   Neither `text` nor `html` present is an `Err`.
/// - `reply_context`: `thread_id` from the message id (see
///   [`derive_envelope_id`]'s resolution order), `conversation_id` from
///   `payload["subject"]`, `channel_token` from the `from` address (so a
///   reply routes back to the sender).
/// - `timestamp`: `payload["created_at"]` when a string, else `Utc::now()`
///   in RFC 3339 (documented fallback — the only clock read on this path).
/// - `raw_payload`: `payload.clone()`, untouched.
/// - `envelope_id`: deterministic — see [`derive_envelope_id`].
pub fn parse_inbound_email(payload: &Value) -> Result<IngressEnvelope, String> {
    let from = payload
        .get("from")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "inbound email: missing 'from'".to_string())?;

    let text = payload.get("text").and_then(Value::as_str);
    let html = payload.get("html").and_then(Value::as_str);
    let body_text = text
        .or(html)
        .ok_or_else(|| "inbound email: no usable body (neither 'text' nor 'html')".to_string())?;

    let attachments = payload
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let message_id = resolve_message_id(payload);
    let subject = payload.get("subject").and_then(Value::as_str);

    let reply_context = ReplyContext {
        thread_id: message_id.clone(),
        conversation_id: subject.map(str::to_string),
        channel_token: Some(from.to_string()),
    };

    let timestamp = payload
        .get("created_at")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let envelope_id = derive_envelope_id(
        message_id.as_deref(),
        from,
        subject.unwrap_or(""),
        body_text,
    );

    Ok(IngressEnvelope {
        envelope_id,
        channel_type: ChannelType::Email,
        sender_id: Some(from.to_string()),
        reply_context: Some(reply_context),
        timestamp,
        source: SourcePayload::ChannelMessage {
            text: body_text.to_string(),
            attachments,
        },
        raw_payload: payload.clone(),
    })
}

/// Resolve the message id, first string wins: `payload["message_id"]`, then
/// `payload["headers"]["Message-ID"]`, then `payload["in_reply_to"]`.
fn resolve_message_id(payload: &Value) -> Option<String> {
    payload
        .get("message_id")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("headers")
                .and_then(|headers| headers.get("Message-ID"))
                .and_then(Value::as_str)
        })
        .or_else(|| payload.get("in_reply_to").and_then(Value::as_str))
        .map(str::to_string)
}

/// Deterministic correlation key — never `Uuid::new_v4()`. Prefers the
/// message id (`format!("email:{message_id}")`); with none, derives a
/// stable v5 UUID from the `(from, subject, body)` triple so a redelivered
/// payload with no message id still maps to one id.
fn derive_envelope_id(message_id: Option<&str>, from: &str, subject: &str, body: &str) -> String {
    match message_id {
        Some(id) => format!("email:{id}"),
        None => {
            let seed = format!("{from}|{subject}|{body}");
            let uuid = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, seed.as_bytes());
            format!("email:{uuid}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({
            "from": "someone@example.com",
            "to": ["bastion@mail.bastiel.com.br"],
            "subject": "Re: proposal follow-up",
            "text": "Sounds great, let's proceed.",
            "html": "<p>Sounds great, let's proceed.</p>",
            "message_id": "msg-123@resend",
            "created_at": "2026-07-30T12:00:00Z",
            "attachments": [{"filename": "contract.pdf", "url": "https://example.com/contract.pdf"}],
        })
    }

    #[test]
    fn parses_every_field_from_a_representative_payload() {
        let envelope = parse_inbound_email(&payload()).expect("should parse");

        assert_eq!(envelope.channel_type, ChannelType::Email);
        assert_eq!(envelope.sender_id, Some("someone@example.com".to_string()));

        match &envelope.source {
            SourcePayload::ChannelMessage { text, .. } => {
                assert_eq!(text, "Sounds great, let's proceed.");
            }
            other => panic!("expected ChannelMessage, got {other:?}"),
        }

        let reply = envelope.reply_context.expect("reply_context present");
        assert_eq!(reply.thread_id, Some("msg-123@resend".to_string()));
        assert_eq!(
            reply.conversation_id,
            Some("Re: proposal follow-up".to_string())
        );
        assert_eq!(reply.channel_token, Some("someone@example.com".to_string()));

        chrono::DateTime::parse_from_rfc3339(&envelope.timestamp)
            .expect("timestamp should be RFC 3339");
    }

    #[test]
    fn raw_payload_is_the_untouched_input() {
        let envelope = parse_inbound_email(&payload()).expect("should parse");
        assert_eq!(envelope.raw_payload, payload());
    }

    #[test]
    fn envelope_id_is_stable_across_two_parses() {
        let first = parse_inbound_email(&payload()).expect("should parse");
        let second = parse_inbound_email(&payload()).expect("should parse");
        assert_eq!(first.envelope_id, second.envelope_id);
    }

    #[test]
    fn envelope_id_is_stable_without_a_message_id() {
        let mut p = payload();
        p.as_object_mut().unwrap().remove("message_id");
        p.as_object_mut().unwrap().remove("headers");

        let first = parse_inbound_email(&p).expect("should parse");
        let second = parse_inbound_email(&p).expect("should parse");

        assert_eq!(first.envelope_id, second.envelope_id);
        assert!(!first.envelope_id.is_empty());
        assert_ne!(first.envelope_id, "email:msg-123@resend");
    }

    #[test]
    fn html_only_payload_falls_back_to_the_html_body() {
        let mut p = payload();
        p.as_object_mut().unwrap().remove("text");

        let envelope = parse_inbound_email(&p).expect("should parse");
        match &envelope.source {
            SourcePayload::ChannelMessage { text, .. } => {
                assert_eq!(text, "<p>Sounds great, let's proceed.</p>");
            }
            other => panic!("expected ChannelMessage, got {other:?}"),
        }
    }

    #[test]
    fn attachments_round_trip_into_the_channel_message() {
        let envelope = parse_inbound_email(&payload()).expect("should parse");
        match &envelope.source {
            SourcePayload::ChannelMessage { attachments, .. } => {
                assert_eq!(attachments.len(), 1);
                assert_eq!(attachments[0]["filename"], json!("contract.pdf"));
            }
            other => panic!("expected ChannelMessage, got {other:?}"),
        }
    }

    #[test]
    fn missing_from_errors() {
        let mut p = payload();
        p.as_object_mut().unwrap().remove("from");

        let err = parse_inbound_email(&p).unwrap_err();
        assert!(err.contains("from"), "error should name 'from': {err}");
    }

    #[test]
    fn missing_body_errors() {
        let mut p = payload();
        p.as_object_mut().unwrap().remove("text");
        p.as_object_mut().unwrap().remove("html");

        let err = parse_inbound_email(&p).unwrap_err();
        assert!(
            err.contains("text") || err.contains("html") || err.contains("body"),
            "error should name the missing body: {err}"
        );
    }

    #[test]
    fn message_id_falls_back_to_headers_then_in_reply_to() {
        let mut p = payload();
        p.as_object_mut().unwrap().remove("message_id");
        p["headers"] = json!({"Message-ID": "header-id@resend"});

        let envelope = parse_inbound_email(&p).expect("should parse");
        assert_eq!(
            envelope.reply_context.unwrap().thread_id,
            Some("header-id@resend".to_string())
        );

        let mut p2 = payload();
        p2.as_object_mut().unwrap().remove("message_id");
        p2["in_reply_to"] = json!("in-reply-to-id@resend");

        let envelope2 = parse_inbound_email(&p2).expect("should parse");
        assert_eq!(
            envelope2.reply_context.unwrap().thread_id,
            Some("in-reply-to-id@resend".to_string())
        );
    }
}
