//! `FetchTranscriptNode` — fetches `SourcePayload::VideoId` content behind
//! an injectable fetch seam (EN.5.A task 5).
//!
//! Same shape as `fetch_article.rs`'s `ArticleFetch` seam, over a video id
//! instead of a URL. Reads the `SourceRouterNode`-stored envelope, extracts
//! `SourcePayload::VideoId { video_id }`, fetches the transcript, and
//! stores the result under its own identity as `{title, text, source_ref}`
//! — the same shape `FetchArticleNode` and `NormalizeChannelContentNode`
//! converge on for `SummarizeNode`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use engine_contract::envelope::{IngressEnvelope, SourcePayload};
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::source_router;

/// The `Node::name()` identity `FetchTranscriptNode` runs and stores its
/// result under (bound to from `SourceRouterNode::route` via
/// `InputBinding::bound("FetchTranscriptNode")`).
pub const NODE_NAME: &str = "FetchTranscriptNode";

/// Fetched transcript content: an optional video title and the transcript
/// text.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedTranscript {
    pub title: Option<String>,
    pub text: String,
}

/// The injectable fetch seam: `fetch(video_id)` -> a [`FetchedTranscript`]
/// on success, or an error string describing the transport failure. Mirrors
/// `ArticleFetch` so `FetchTranscriptNode::with_fetch` can hold either the
/// live or stub implementation interchangeably.
#[async_trait]
pub trait TranscriptFetch: Send + Sync {
    async fn fetch(&self, video_id: &str) -> Result<FetchedTranscript, String>;
}

/// Base URL a `video_id` is resolved against to build `source_ref`.
const YOUTUBE_WATCH_BASE: &str = "https://www.youtube.com/watch?v=";

/// The real transcript fetch: no first-party transcript API is wired yet
/// (this is the seam later channel work fills), so the live implementation
/// fails closed with a descriptive error rather than silently returning
/// empty content. `FetchTranscriptNode` accepts a seam override precisely
/// so production wiring can swap this out without touching the node.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnimplementedTranscriptFetch;

#[async_trait]
impl TranscriptFetch for UnimplementedTranscriptFetch {
    async fn fetch(&self, video_id: &str) -> Result<FetchedTranscript, String> {
        Err(format!(
            "no transcript source configured for video_id={video_id}"
        ))
    }
}

/// Convenience constructor: an `Arc<dyn TranscriptFetch>` wrapping
/// [`UnimplementedTranscriptFetch`]. Production callers swap this for a
/// real transcript-fetching implementation via `with_fetch`; tests build a
/// [`StubTranscriptFetch`] instead, so the gated suite never contacts the
/// network.
#[must_use]
pub fn transcript_fetch_live() -> Arc<dyn TranscriptFetch> {
    Arc::new(UnimplementedTranscriptFetch)
}

/// Test-stub `TranscriptFetch`: records the last `video_id` it was asked to
/// fetch and returns a configurable success/failure result.
#[derive(Clone)]
pub struct StubTranscriptFetch {
    last_call: Arc<Mutex<Option<String>>>,
    result: Arc<Mutex<Result<FetchedTranscript, String>>>,
}

impl StubTranscriptFetch {
    #[must_use]
    pub fn succeeding(content: FetchedTranscript) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(content))),
        }
    }

    #[must_use]
    pub fn failing(error: impl Into<String>) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Err(error.into()))),
        }
    }

    #[must_use]
    pub fn last_call(&self) -> Option<String> {
        self.last_call.lock().unwrap().clone()
    }
}

#[async_trait]
impl TranscriptFetch for StubTranscriptFetch {
    async fn fetch(&self, video_id: &str) -> Result<FetchedTranscript, String> {
        *self.last_call.lock().unwrap() = Some(video_id.to_string());
        self.result.lock().unwrap().clone()
    }
}

/// Fetches `SourcePayload::VideoId` content and stores `{title, text,
/// source_ref}` for `SummarizeNode`.
pub struct FetchTranscriptNode {
    fetch: Arc<dyn TranscriptFetch>,
}

impl FetchTranscriptNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fetch: transcript_fetch_live(),
        }
    }

    /// Override the fetch seam. Tests use this to inject a
    /// [`StubTranscriptFetch`] so the gated suite never contacts the
    /// network.
    #[must_use]
    pub fn with_fetch(mut self, fetch: Arc<dyn TranscriptFetch>) -> Self {
        self.fetch = fetch;
        self
    }
}

impl Default for FetchTranscriptNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Node for FetchTranscriptNode {
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

        let video_id = match &envelope.source {
            SourcePayload::VideoId { video_id } => video_id.clone(),
            other => {
                return Err(NodeError::new(format!(
                    "{NODE_NAME}: expected SourcePayload::VideoId, got {other:?}"
                )))
            }
        };

        let content = self
            .fetch
            .fetch(&video_id)
            .await
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: fetch failed: {err}")))?;

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "title": content.title,
                "text": content.text,
                "source_ref": format!("{YOUTUBE_WATCH_BASE}{video_id}"),
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

    fn ctx_with_envelope(source: serde_json::Value) -> TaskContext {
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
                    "channel_type": "youtube_transcript",
                    "timestamp": "2026-07-25T00:00:00Z",
                    "source": source,
                },
                "policy": { "max_critic_iterations": 3, "critic_confidence_threshold": 0.8 },
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn happy_path_stores_title_text_source_ref() {
        let node = FetchTranscriptNode::new().with_fetch(Arc::new(
            StubTranscriptFetch::succeeding(FetchedTranscript {
                title: Some("A Talk".to_string()),
                text: "transcript body".to_string(),
            }),
        ));
        let ctx = ctx_with_envelope(json!({ "kind": "video_id", "video_id": "abc123" }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(stored.get("title").and_then(|v| v.as_str()), Some("A Talk"));
        assert_eq!(
            stored.get("text").and_then(|v| v.as_str()),
            Some("transcript body")
        );
        assert_eq!(
            stored.get("source_ref").and_then(|v| v.as_str()),
            Some("https://www.youtube.com/watch?v=abc123")
        );
    }

    #[tokio::test]
    async fn fetch_failure_surfaces_node_error() {
        let node = FetchTranscriptNode::new().with_fetch(Arc::new(StubTranscriptFetch::failing(
            "transcript unavailable",
        )));
        let ctx = ctx_with_envelope(json!({ "kind": "video_id", "video_id": "abc123" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("transcript unavailable"));
    }

    #[tokio::test]
    async fn wrong_payload_kind_surfaces_node_error() {
        let node = FetchTranscriptNode::new().with_fetch(Arc::new(
            StubTranscriptFetch::succeeding(FetchedTranscript {
                title: None,
                text: String::new(),
            }),
        ));
        let ctx = ctx_with_envelope(json!({ "kind": "url", "url": "https://example.com/a" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("SourcePayload::VideoId"));
    }

    #[tokio::test]
    async fn stub_records_the_video_id_it_was_fetched_with() {
        let stub = StubTranscriptFetch::succeeding(FetchedTranscript {
            title: None,
            text: "body".to_string(),
        });
        let node = FetchTranscriptNode::new().with_fetch(Arc::new(stub.clone()));
        let ctx = ctx_with_envelope(json!({ "kind": "video_id", "video_id": "abc123" }));

        node.process(ctx).await.expect("process should succeed");

        assert_eq!(stub.last_call(), Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn live_seam_fails_closed_with_no_transcript_source_configured() {
        let live = transcript_fetch_live();
        let err = live.fetch("abc123").await.expect_err("should fail");
        assert!(err.contains("no transcript source configured"));
    }
}
