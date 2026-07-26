//! `NormalizeChannelContentNode` — deterministic passthrough for
//! `SourcePayload::ChannelMessage` (and the `TaskContextRef`/
//! `WorkflowTrigger` payloads `SourceRouterNode` also routes here), the
//! Phase 6 adapter landing pad (EN.5.A task 5).
//!
//! No network, no model — reads the `SourceRouterNode`-stored envelope and
//! normalizes `{text, sender_id, channel_type}` into the same `{title,
//! text, source_ref}` shape `FetchArticleNode`/`FetchTranscriptNode` emit,
//! so `SummarizeNode` sees one converged content shape regardless of
//! source.

use engine_contract::envelope::{IngressEnvelope, SourcePayload};
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::source_router;

/// The `Node::name()` identity `NormalizeChannelContentNode` runs and
/// stores its result under (bound to from `SourceRouterNode::route` via
/// `InputBinding::bound("NormalizeChannelContentNode")`).
pub const NODE_NAME: &str = "NormalizeChannelContentNode";

/// Deterministic passthrough normalizer. Requires no seam at all — every
/// input it needs is already on `ctx` by the time it runs.
pub struct NormalizeChannelContentNode;

/// Builds the `source_ref` for a normalized envelope: `channel_type` plus
/// `sender_id` when present, so downstream stages can attribute the
/// content without re-parsing the raw envelope.
fn source_ref(envelope: &IngressEnvelope) -> String {
    let channel_type = serde_json::to_value(envelope.channel_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());

    match &envelope.sender_id {
        Some(sender_id) => format!("{channel_type}:{sender_id}"),
        None => channel_type,
    }
}

/// Extracts the normalized `text` from any of the three payload kinds this
/// node handles.
fn extract_text(source: &SourcePayload) -> Result<String, NodeError> {
    match source {
        SourcePayload::ChannelMessage { text, .. } => Ok(text.clone()),
        SourcePayload::TaskContextRef {
            inline, event_id, ..
        } => Ok(inline
            .as_ref()
            .and_then(|value| value.as_str().map(str::to_string))
            .or_else(|| inline.as_ref().map(|value| value.to_string()))
            .or_else(|| event_id.clone())
            .unwrap_or_default()),
        SourcePayload::WorkflowTrigger { event, .. } => Ok(event.to_string()),
        other => Err(NodeError::new(format!(
            "{NODE_NAME}: unsupported SourcePayload kind: {other:?}"
        ))),
    }
}

#[async_trait::async_trait]
impl Node for NormalizeChannelContentNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let stored = get_result(&ctx, source_router::NODE_NAME).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no envelope stored by {}",
                source_router::NODE_NAME
            ))
        })?;
        let envelope: IngressEnvelope = serde_json::from_value(
            stored
                .get("envelope")
                .cloned()
                .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: envelope field missing")))?,
        )
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid envelope: {err}")))?;

        let text = extract_text(&envelope.source)?;
        let source_ref = source_ref(&envelope);

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "title": serde_json::Value::Null,
                "text": text,
                "source_ref": source_ref,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ctx_with_envelope(
        channel_type: &str,
        sender_id: Option<&str>,
        source: serde_json::Value,
    ) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        };
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({
                "envelope": {
                    "envelope_id": "env-1",
                    "channel_type": channel_type,
                    "sender_id": sender_id,
                    "timestamp": "2026-07-25T00:00:00Z",
                    "source": source,
                },
                "policy": { "max_critic_iterations": 3, "critic_confidence_threshold": 0.8 },
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn channel_message_normalizes_to_content_shape() {
        let node = NormalizeChannelContentNode;
        let ctx = ctx_with_envelope(
            "slack",
            Some("U123"),
            json!({ "kind": "channel_message", "text": "hello world", "attachments": [] }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("title"), Some(&serde_json::Value::Null));
        assert_eq!(
            stored.get("text").and_then(|v| v.as_str()),
            Some("hello world")
        );
        assert_eq!(
            stored.get("source_ref").and_then(|v| v.as_str()),
            Some("slack:U123")
        );
    }

    #[tokio::test]
    async fn channel_message_without_sender_id_falls_back_to_channel_type_only() {
        let node = NormalizeChannelContentNode;
        let ctx = ctx_with_envelope(
            "email",
            None,
            json!({ "kind": "channel_message", "text": "body", "attachments": [] }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("source_ref").and_then(|v| v.as_str()),
            Some("email")
        );
    }

    #[tokio::test]
    async fn task_context_ref_with_inline_string_normalizes() {
        let node = NormalizeChannelContentNode;
        let ctx = ctx_with_envelope(
            "research_agent",
            None,
            json!({
                "kind": "task_context_ref",
                "workflow_type": "RESEARCH_AGENT",
                "inline": "researched summary text",
            }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("text").and_then(|v| v.as_str()),
            Some("researched summary text")
        );
    }

    #[tokio::test]
    async fn workflow_trigger_normalizes_event_to_text() {
        let node = NormalizeChannelContentNode;
        let ctx = ctx_with_envelope(
            "workflow_trigger",
            None,
            json!({
                "kind": "workflow_trigger",
                "workflow_type": "SDLC_FLOW",
                "event": { "task": "do-thing" },
            }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert!(stored
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("do-thing"));
    }

    #[tokio::test]
    async fn wrong_payload_kind_surfaces_node_error() {
        let node = NormalizeChannelContentNode;
        let ctx = ctx_with_envelope(
            "web_article",
            None,
            json!({ "kind": "url", "url": "https://example.com/a" }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("unsupported SourcePayload kind"));
    }

    #[test]
    fn node_requires_no_seam_to_construct() {
        let _node = NormalizeChannelContentNode;
    }
}
