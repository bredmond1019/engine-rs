//! The channel-agnostic `IngressEnvelope` contract (D51, EN.5.A).
//!
//! Every ingress channel the engine knows about — web articles, YouTube
//! transcripts, human channels (Slack/Telegram/WhatsApp/Email), agent/workflow
//! sources, and every *surface* (bastion-web, the bastion TUI, the scheduler,
//! a raw API call) — arrives as one `IngressEnvelope`. `CONTENT_PIPELINE`
//! (and, later, any envelope-consuming workflow) routes purely on the
//! internally-tagged `SourcePayload` kind, not on `ChannelType`, so adding a
//! new channel later costs zero graph edits.
//!
//! Source of truth: `planning/EN.5.A-content-pipeline/architecture.md` §3.1.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Every ingress and egress channel the engine knows about. Unsupported
/// channels are routable *types* today; their adapters land in EN.6.*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    WebArticle,
    #[serde(rename = "youtube_transcript")]
    YouTubeTranscript,
    Slack,
    Telegram,
    WhatsApp,
    Email,
    ResearchAgent,
    WorkflowTrigger,
    /// bastion-web surface.
    Web,
    /// The bastion terminal UI surface.
    Tui,
    /// The engine's own scheduler (cron-triggered runs).
    Schedule,
    /// A raw API call with no richer channel semantics.
    Api,
}

/// Where a reply should go. Opaque to the pipeline; only the owning
/// `ChannelTransport` (EN.6.*) interprets these fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplyContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Channel-scoped routing token (Slack channel id, chat id, mailbox…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_token: Option<String>,
}

/// The typed content reference inside an envelope. Internally tagged so
/// stored `TaskContext`s stay self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourcePayload {
    Url {
        url: String,
    },
    VideoId {
        video_id: String,
    },
    ChannelMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Value>,
    },
    /// A prior run's output as content (EN.6.E). `inline` carries the
    /// payload when present; otherwise `event_id` names the source run.
    TaskContextRef {
        workflow_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inline: Option<Value>,
    },
    WorkflowTrigger {
        workflow_type: String,
        event: Value,
    },
}

/// The channel-agnostic ingress contract (D51). Everything entering
/// `CONTENT_PIPELINE` — and, later, any envelope-consuming workflow —
/// arrives in this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngressEnvelope {
    pub envelope_id: String,
    pub channel_type: ChannelType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_context: Option<ReplyContext>,
    /// RFC 3339. Kept a `String` to preserve byte-stability across the
    /// contract seam.
    pub timestamp: String,
    pub source: SourcePayload,
    /// The unmodified channel payload, for audit/replay. Adapters fill it;
    /// pipeline nodes never parse it.
    #[serde(default)]
    pub raw_payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope_with(channel_type: ChannelType, source: SourcePayload) -> IngressEnvelope {
        IngressEnvelope {
            envelope_id: "env-1".to_string(),
            channel_type,
            sender_id: Some("user-1".to_string()),
            reply_context: Some(ReplyContext {
                thread_id: Some("t-1".to_string()),
                conversation_id: None,
                channel_token: Some("token-1".to_string()),
            }),
            timestamp: "2026-07-25T00:00:00Z".to_string(),
            source,
            raw_payload: json!({"raw": true}),
        }
    }

    fn all_channel_types() -> Vec<ChannelType> {
        vec![
            ChannelType::WebArticle,
            ChannelType::YouTubeTranscript,
            ChannelType::Slack,
            ChannelType::Telegram,
            ChannelType::WhatsApp,
            ChannelType::Email,
            ChannelType::ResearchAgent,
            ChannelType::WorkflowTrigger,
            ChannelType::Web,
            ChannelType::Tui,
            ChannelType::Schedule,
            ChannelType::Api,
        ]
    }

    #[test]
    fn channel_type_round_trips_for_every_variant() {
        for ct in all_channel_types() {
            let json = serde_json::to_string(&ct).expect("serialize");
            let back: ChannelType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(ct, back);
        }
    }

    #[test]
    fn youtube_transcript_serializes_as_youtube_transcript_not_you_tube_transcript() {
        let json = serde_json::to_string(&ChannelType::YouTubeTranscript).unwrap();
        assert_eq!(json, "\"youtube_transcript\"");
    }

    #[test]
    fn plain_snake_case_variants_serialize_as_expected() {
        assert_eq!(
            serde_json::to_string(&ChannelType::WebArticle).unwrap(),
            "\"web_article\""
        );
        assert_eq!(
            serde_json::to_string(&ChannelType::ResearchAgent).unwrap(),
            "\"research_agent\""
        );
        assert_eq!(
            serde_json::to_string(&ChannelType::WorkflowTrigger).unwrap(),
            "\"workflow_trigger\""
        );
        assert_eq!(serde_json::to_string(&ChannelType::Web).unwrap(), "\"web\"");
        assert_eq!(serde_json::to_string(&ChannelType::Tui).unwrap(), "\"tui\"");
        assert_eq!(
            serde_json::to_string(&ChannelType::Schedule).unwrap(),
            "\"schedule\""
        );
        assert_eq!(serde_json::to_string(&ChannelType::Api).unwrap(), "\"api\"");
    }

    #[test]
    fn unknown_channel_type_string_fails_deserialization() {
        let err = serde_json::from_str::<ChannelType>("\"carrier_pigeon\"");
        assert!(err.is_err());
    }

    #[test]
    fn source_payload_url_round_trips() {
        let payload = SourcePayload::Url {
            url: "https://example.com/article".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "url");
        let back: SourcePayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn source_payload_video_id_round_trips() {
        let payload = SourcePayload::VideoId {
            video_id: "abc123".to_string(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "video_id");
        let back: SourcePayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn source_payload_channel_message_round_trips_with_default_attachments() {
        let payload = SourcePayload::ChannelMessage {
            text: "hello".to_string(),
            attachments: Vec::new(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "channel_message");
        assert!(
            json.get("attachments").is_none(),
            "empty attachments should be skipped"
        );
        let back: SourcePayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload, back);

        // Missing `attachments` key entirely also deserializes cleanly (default).
        let minimal = json!({"kind": "channel_message", "text": "hi"});
        let back: SourcePayload = serde_json::from_value(minimal).unwrap();
        assert_eq!(
            back,
            SourcePayload::ChannelMessage {
                text: "hi".to_string(),
                attachments: Vec::new(),
            }
        );
    }

    #[test]
    fn source_payload_task_context_ref_round_trips_with_optional_fields() {
        let payload = SourcePayload::TaskContextRef {
            workflow_type: "SDLC_FLOW".to_string(),
            event_id: Some("evt-1".to_string()),
            inline: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "task_context_ref");
        assert!(json.get("inline").is_none());
        let back: SourcePayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload, back);

        // Minimal form with only the required field defaults the optionals cleanly.
        let minimal = json!({"kind": "task_context_ref", "workflow_type": "SDLC_FLOW"});
        let back: SourcePayload = serde_json::from_value(minimal).unwrap();
        assert_eq!(
            back,
            SourcePayload::TaskContextRef {
                workflow_type: "SDLC_FLOW".to_string(),
                event_id: None,
                inline: None,
            }
        );
    }

    #[test]
    fn source_payload_workflow_trigger_round_trips() {
        let payload = SourcePayload::WorkflowTrigger {
            workflow_type: "PROPOSAL_GENERATOR".to_string(),
            event: json!({"trigger": "cron"}),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["kind"], "workflow_trigger");
        let back: SourcePayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn ingress_envelope_round_trips_for_every_channel_type() {
        for ct in all_channel_types() {
            let envelope = envelope_with(
                ct,
                SourcePayload::Url {
                    url: "https://example.com".to_string(),
                },
            );
            let json = serde_json::to_value(&envelope).unwrap();
            let back: IngressEnvelope = serde_json::from_value(json).unwrap();
            assert_eq!(envelope, back);
        }
    }

    #[test]
    fn ingress_envelope_optional_fields_default_cleanly() {
        let minimal = json!({
            "envelope_id": "env-2",
            "channel_type": "web_article",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": {"kind": "url", "url": "https://example.com"}
        });
        let envelope: IngressEnvelope = serde_json::from_value(minimal).unwrap();
        assert_eq!(envelope.sender_id, None);
        assert_eq!(envelope.reply_context, None);
        assert_eq!(envelope.raw_payload, Value::Null);
    }

    #[test]
    fn reply_context_optional_fields_default_cleanly() {
        let minimal: ReplyContext = serde_json::from_value(json!({})).unwrap();
        assert_eq!(minimal, ReplyContext::default());
    }
}
