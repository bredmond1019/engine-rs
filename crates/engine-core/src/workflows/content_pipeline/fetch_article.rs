//! `FetchArticleNode` — fetches `SourcePayload::Url` content behind an
//! injectable fetch seam (EN.5.A task 5).
//!
//! Patterned on `crate::nodes::http_post::HttpPost`: a trait so production
//! code reaches for a real `reqwest`-backed GET ([`article_fetch_live`])
//! while tests inject a stub that returns a canned [`FetchedContent`] — no
//! live network call in the gated `cargo test` suite.
//!
//! Reads the [`super::source_router::SourceRouterNode`]-stored envelope,
//! extracts the `SourcePayload::Url { url }`, fetches it, and stores the
//! result under its own identity as `{title, text, source_ref}` — the same
//! shape `FetchTranscriptNode` and `NormalizeChannelContentNode` converge
//! on for `SummarizeNode`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use engine_contract::envelope::{IngressEnvelope, SourcePayload};
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::source_router;

/// The `Node::name()` identity `FetchArticleNode` runs and stores its
/// result under (bound to from `SourceRouterNode::route` via
/// `InputBinding::bound("FetchArticleNode")`).
pub const NODE_NAME: &str = "FetchArticleNode";

/// Fetched article content: an optional page title and the extracted text.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedContent {
    pub title: Option<String>,
    pub text: String,
}

/// The injectable fetch seam: `fetch(url)` -> a [`FetchedContent`] on
/// success, or an error string describing the transport failure. Mirrors
/// `HttpPost`'s async-trait-object shape so `FetchArticleNode::with_fetch`
/// can hold either the live or stub implementation interchangeably.
#[async_trait]
pub trait ArticleFetch: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchedContent, String>;
}

/// The real HTTP GET: fetches `url`, naively lifts a `<title>…</title>` if
/// present, and treats the response body as the article text.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestArticleFetch;

#[async_trait]
impl ArticleFetch for ReqwestArticleFetch {
    async fn fetch(&self, url: &str) -> Result<FetchedContent, String> {
        let response = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .map_err(|err| format!("article fetch request failed: {err}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("article fetch returned HTTP {status}"));
        }

        let body = response
            .text()
            .await
            .map_err(|err| format!("article fetch body read failed: {err}"))?;

        let title = extract_title(&body);

        Ok(FetchedContent { title, text: body })
    }
}

/// Naive `<title>` extraction — good enough for a first pass; no full HTML
/// parser dependency is warranted for this seam.
fn extract_title(html: &str) -> Option<String> {
    let start = html.to_ascii_lowercase().find("<title>")? + "<title>".len();
    let end = html[start..].to_ascii_lowercase().find("</title>")? + start;
    let title = html[start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Convenience constructor: an `Arc<dyn ArticleFetch>` wrapping
/// [`ReqwestArticleFetch`]. Production callers reach for this; tests build a
/// [`StubArticleFetch`] instead, so the gated suite never contacts the
/// network.
#[must_use]
pub fn article_fetch_live() -> Arc<dyn ArticleFetch> {
    Arc::new(ReqwestArticleFetch)
}

/// Test-stub `ArticleFetch`: records the last `url` it was asked to fetch
/// and returns a configurable success/failure result.
#[derive(Clone)]
pub struct StubArticleFetch {
    last_call: Arc<Mutex<Option<String>>>,
    result: Arc<Mutex<Result<FetchedContent, String>>>,
}

impl StubArticleFetch {
    #[must_use]
    pub fn succeeding(content: FetchedContent) -> Self {
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
impl ArticleFetch for StubArticleFetch {
    async fn fetch(&self, url: &str) -> Result<FetchedContent, String> {
        *self.last_call.lock().unwrap() = Some(url.to_string());
        self.result.lock().unwrap().clone()
    }
}

/// Fetches `SourcePayload::Url` content and stores `{title, text,
/// source_ref}` for `SummarizeNode`.
pub struct FetchArticleNode {
    fetch: Arc<dyn ArticleFetch>,
}

impl FetchArticleNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fetch: article_fetch_live(),
        }
    }

    /// Override the fetch seam. Tests use this to inject a [`StubArticleFetch`]
    /// so the gated suite never contacts the network.
    #[must_use]
    pub fn with_fetch(mut self, fetch: Arc<dyn ArticleFetch>) -> Self {
        self.fetch = fetch;
        self
    }
}

impl Default for FetchArticleNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Node for FetchArticleNode {
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

        let url = match &envelope.source {
            SourcePayload::Url { url } => url.clone(),
            other => {
                return Err(NodeError::new(format!(
                    "{NODE_NAME}: expected SourcePayload::Url, got {other:?}"
                )))
            }
        };

        let content = self
            .fetch
            .fetch(&url)
            .await
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: fetch failed: {err}")))?;

        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "title": content.title,
                "text": content.text,
                "source_ref": url,
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
                    "channel_type": "web_article",
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
        let node = FetchArticleNode::new().with_fetch(Arc::new(StubArticleFetch::succeeding(
            FetchedContent {
                title: Some("Example Title".to_string()),
                text: "example body".to_string(),
            },
        )));
        let ctx = ctx_with_envelope(json!({ "kind": "url", "url": "https://example.com/a" }));

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get("title").and_then(|v| v.as_str()),
            Some("Example Title")
        );
        assert_eq!(
            stored.get("text").and_then(|v| v.as_str()),
            Some("example body")
        );
        assert_eq!(
            stored.get("source_ref").and_then(|v| v.as_str()),
            Some("https://example.com/a")
        );
    }

    #[tokio::test]
    async fn fetch_failure_surfaces_node_error() {
        let node = FetchArticleNode::new()
            .with_fetch(Arc::new(StubArticleFetch::failing("connection refused")));
        let ctx = ctx_with_envelope(json!({ "kind": "url", "url": "https://example.com/a" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("connection refused"));
    }

    #[tokio::test]
    async fn wrong_payload_kind_surfaces_node_error() {
        let node = FetchArticleNode::new().with_fetch(Arc::new(StubArticleFetch::succeeding(
            FetchedContent {
                title: None,
                text: String::new(),
            },
        )));
        let ctx = ctx_with_envelope(json!({ "kind": "video_id", "video_id": "abc123" }));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("SourcePayload::Url"));
    }

    #[tokio::test]
    async fn stub_records_the_url_it_was_fetched_with() {
        let stub = StubArticleFetch::succeeding(FetchedContent {
            title: None,
            text: "body".to_string(),
        });
        let node = FetchArticleNode::new().with_fetch(Arc::new(stub.clone()));
        let ctx = ctx_with_envelope(json!({ "kind": "url", "url": "https://example.com/a" }));

        node.process(ctx).await.expect("process should succeed");

        assert_eq!(stub.last_call(), Some("https://example.com/a".to_string()));
    }

    #[test]
    fn live_seam_constructs_without_panicking() {
        let _live: Arc<dyn ArticleFetch> = article_fetch_live();
    }

    #[test]
    fn extract_title_finds_a_title_tag() {
        assert_eq!(
            extract_title("<html><head><title>Hi There</title></head></html>"),
            Some("Hi There".to_string())
        );
        assert_eq!(extract_title("<html><body>no title</body></html>"), None);
    }
}
