//! `IntakeIdeaNode` — normalizes `PRE_PLAN`'s dispatched event into
//! `TaskContext` (`EN.19.A` task 1).
//!
//! No model call. Validates that the dispatched event carries the two
//! required fields (`idea`, `slug`) as non-empty strings, failing loudly and
//! naming the missing field otherwise — the same validation-error shape
//! `crate::nodes::email::inbound::parse_inbound_email` uses for
//! `email_webhooks.rs`'s inbound route. Optional channel metadata
//! (`channel`, `sender`) passes through unchanged when present.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::workflows::put_result;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its normalized payload is stamped onto. This is also
/// `check_existing::CONTINUE_ROUTE`'s target identity.
pub const NODE_NAME: &str = "IntakeIdeaNode";

/// Read a required, non-empty string field from the dispatched event,
/// failing with a `NodeError` naming `field` when it is missing, empty, or
/// not a string — mirrors
/// `crate::nodes::email::inbound::parse_inbound_email`'s validation shape.
fn require_field<'a>(event: &'a serde_json::Value, field: &str) -> Result<&'a str, NodeError> {
    event
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| NodeError::new(format!("pre_plan intake: missing or empty '{field}'")))
}

/// Normalizes the inbound payload (idea text, slug, optional channel
/// metadata) into `TaskContext` — the first real pipeline step once
/// `CheckExistingNotesNode` has confirmed no existing `notes.md` blocks this
/// run.
pub struct IntakeIdeaNode;

impl IntakeIdeaNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for IntakeIdeaNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for IntakeIdeaNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let idea = require_field(&ctx.event, "idea")?.to_string();
        let slug = require_field(&ctx.event, "slug")?.to_string();

        // Optional channel metadata — carried through when present, absent
        // otherwise, since PRE_PLAN's HTTP dispatch today and a future
        // Telegram webhook both reuse this same node without either being
        // required to supply it.
        let channel = ctx
            .event
            .get("channel")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let sender = ctx
            .event
            .get("sender")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                "idea": idea,
                "slug": slug,
                "channel": channel,
                "sender": sender,
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
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn process_normalizes_idea_and_slug() {
        let node = IntakeIdeaNode::new();
        let ctx = context(json!({"idea": "build a widget", "slug": "widget-idea"}));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("idea").and_then(|v| v.as_str()),
            Some("build a widget")
        );
        assert_eq!(
            stored.get("slug").and_then(|v| v.as_str()),
            Some("widget-idea")
        );
        assert!(stored
            .get("channel")
            .map(serde_json::Value::is_null)
            .unwrap_or(true));
    }

    #[tokio::test]
    async fn process_carries_optional_channel_metadata() {
        let node = IntakeIdeaNode::new();
        let ctx = context(json!({
            "idea": "build a widget",
            "slug": "widget-idea",
            "channel": "telegram",
            "sender": "operator",
        }));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("channel").and_then(|v| v.as_str()),
            Some("telegram")
        );
        assert_eq!(
            stored.get("sender").and_then(|v| v.as_str()),
            Some("operator")
        );
    }

    #[tokio::test]
    async fn process_errors_naming_missing_idea() {
        let node = IntakeIdeaNode::new();
        let ctx = context(json!({"slug": "widget-idea"}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without idea");
        assert!(err.message.contains("idea"));
    }

    #[tokio::test]
    async fn process_errors_naming_missing_slug() {
        let node = IntakeIdeaNode::new();
        let ctx = context(json!({"idea": "build a widget"}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without slug");
        assert!(err.message.contains("slug"));
    }

    #[tokio::test]
    async fn process_errors_on_empty_idea() {
        let node = IntakeIdeaNode::new();
        let ctx = context(json!({"idea": "   ", "slug": "widget-idea"}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail on blank idea");
        assert!(err.message.contains("idea"));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(IntakeIdeaNode::new().name(), NODE_NAME);
    }
}
