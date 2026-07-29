//! `HarvestApproveNode` (`EN.7.C` task 6) — the generic node that completes a
//! deferred harvest: reads the `{artifact_id, url, payload, doc_paths}`
//! pending-harvest record (`crate::nodes::harvest_gate::pending_harvest_record`,
//! task 1) off `ctx.event` and POSTs `payload` to `url` over the injectable
//! `HttpPost` seam.
//!
//! This is the completion half of `HarvestMode::Approval`
//! (`crate::workflows::content_pipeline::persist_to_brain::PersistToBrainNode`,
//! task 4): the deferring node stamps a pending record whose `payload` is
//! the exact JSON body an `in_process` push would have sent; this node POSTs
//! that payload verbatim to the same `url`, so the eventual push is
//! byte-identical to what the in-process path would have produced — no
//! second indexing path, no divergence between what got written and what
//! got indexed.
//!
//! Model-free, like `MaterializeDocNode`/`OpportunityEditNode`: no
//! `ClaudeCodeStep`, no `ModelTier`, nothing for a policy layer to resolve.
//! Stamps under `self.name()` rather than the bare [`NODE_NAME`] const,
//! mirroring `MaterializeDocNode`, so an identity override
//! (`crate::node::NodeExt::with_identity`) never collides with another
//! instance's result in a shared `ctx.nodes` map.
//!
//! An approved-but-failed harvest is a loud failure, never a silent drop: a
//! malformed/absent pending record is a `NodeError` naming the missing
//! field, and a non-2xx / transport failure from the `HttpPost` seam is a
//! `NodeError` too.

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::put_result;

/// The `Node::name()` identity `HarvestApproveNode` runs under by default,
/// and the `ctx.nodes` key its result is stamped onto when no identity
/// override is applied.
pub const NODE_NAME: &str = "HarvestApproveNode";

/// The node that completes a deferred harvest by POSTing a pending-harvest
/// record's `payload` to its `url` over the injectable `HttpPost` seam.
pub struct HarvestApproveNode {
    http_post: std::sync::Arc<dyn HttpPost>,
}

impl HarvestApproveNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never contacts a live endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: std::sync::Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Read a required string field off the pending record in `ctx.event`,
    /// erroring with a message naming the missing field.
    fn required_str<'a>(
        &self,
        event: &'a serde_json::Value,
        field: &str,
    ) -> Result<&'a str, NodeError> {
        event.get(field).and_then(|v| v.as_str()).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: pending-harvest record missing required field `{field}`"
            ))
        })
    }
}

impl Default for HarvestApproveNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for HarvestApproveNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = ctx.event.clone();

        let artifact_id = self.required_str(&event, "artifact_id")?.to_string();
        let url = self.required_str(&event, "url")?.to_string();
        let payload = event.get("payload").cloned().ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: pending-harvest record missing required field `payload`"
            ))
        })?;

        let response = self.http_post.post(&url, payload).await.map_err(|err| {
            NodeError::new(format!("{NODE_NAME}: harvest approval push failed: {err}"))
        })?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            self.name(),
            json!({
                "approved": true,
                "posted": true,
                "status": response.status,
                "artifact_id": artifact_id,
                "response": response.body,
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

    use crate::nodes::http_post::StubHttpPost;

    use super::*;

    fn pending_event() -> serde_json::Value {
        json!({
            "artifact_id": "artifact-1",
            "url": "https://brain.example/ingest/learning",
            "payload": {"artifact_id": "artifact-1", "summary": "A digest."},
            "doc_paths": ["brain/content/learning/artifact-1.md"],
        })
    }

    fn ctx_with_event(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn well_formed_record_posts_the_exact_url_and_payload() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let ctx = ctx_with_event(pending_event());
        let ctx = node.process(ctx).await.expect("process should succeed");

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/learning");
        assert_eq!(
            body,
            json!({"artifact_id": "artifact-1", "summary": "A digest."})
        );

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["approved"], json!(true));
        assert_eq!(result["posted"], json!(true));
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["artifact_id"], json!("artifact-1"));
    }

    #[tokio::test]
    async fn missing_url_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("url");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains('`'));
        assert!(err.message.contains("url"));
    }

    #[tokio::test]
    async fn missing_payload_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("payload");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("payload"));
    }

    #[tokio::test]
    async fn missing_artifact_id_errors_naming_the_field() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut event = pending_event();
        event.as_object_mut().unwrap().remove("artifact_id");
        let ctx = ctx_with_event(event);

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("artifact_id"));
    }

    #[tokio::test]
    async fn a_stub_failure_surfaces_as_a_node_error() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");
        let node = HarvestApproveNode::new().with_http_post(std::sync::Arc::new(stub));

        let ctx = ctx_with_event(pending_event());
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("brain endpoint unreachable"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = HarvestApproveNode::default();
    }
}
