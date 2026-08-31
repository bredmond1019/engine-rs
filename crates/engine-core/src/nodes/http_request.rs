//! `http_request` — `HttpRequestNode` (`EN.ticket.generic-http-request-node`
//! task 1), a general-purpose HTTP node: a workflow graph configures it with
//! a URL, JSON body and headers and it stores `{status, body}` under its own
//! node identity, exactly like every other node.
//!
//! REUSES the existing `crate::nodes::http_post::HttpPost` seam rather than
//! adding a second `reqwest` path — the same injectable seam
//! `PersistToBrainNode` (`workflows::content_pipeline::persist_to_brain`)
//! already calls via `post_with_headers`, production-proven since `EN.6.K`
//! moved every production caller onto that path. Builder shape
//! (`with_http_post`/`with_url`/`with_body`/`with_headers`) mirrors
//! `PersistToBrainNode`'s constructor so tests can swap in the existing
//! `StubHttpPost` — no new test infrastructure needed.
//!
//! Only `POST` is wired today: the seam's only outbound method is
//! `HttpPost::post`/`post_with_headers`, and the first consumer
//! (`price-scout:PS.9.D`, a nightly price-watch trigger) needs POST. A
//! `method` field is still exposed on the builder so a workflow's config can
//! name the verb explicitly and a future non-POST caller has a seam to land
//! on; anything other than `"POST"` (case-insensitive) surfaces as a
//! `NodeError` rather than being silently coerced or ignored — extending the
//! `HttpPost` trait to a real second verb is out of scope for this ticket
//! (see `planning/blocks/EN.ticket.generic-http-request-node.json`'s `what`).
//!
//! A non-2xx response or a transport failure both surface as a `NodeError`
//! (task 2) — the first consumer is a scheduled, unattended trigger, so a
//! node that swallowed a non-2xx would make a dead nightly job look healthy.

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::put_result;

/// The `Node::name()` identity `HttpRequestNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "HttpRequestNode";

/// The general-purpose HTTP node: POSTs a configured URL/body/headers over
/// the injectable [`HttpPost`] seam and stores `{status, body}` under
/// [`NODE_NAME`]. No brain-ingest intent, no gate, no payload shaping — a
/// workflow builds whatever JSON body it needs and hands it to
/// [`Self::with_body`] directly.
pub struct HttpRequestNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    url: Option<String>,
    method: String,
    body: Value,
    headers: Vec<(String, String)>,
}

impl HttpRequestNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl, no URL
    /// (configuring one is required — [`Self::process`] errors without it),
    /// `method = "POST"`, a `null` body, and no headers.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            url: None,
            method: "POST".to_string(),
            body: Value::Null,
            headers: Vec::new(),
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never contacts a live endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: std::sync::Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Set the target URL. Required — [`Self::process`] returns a
    /// `NodeError` if this is never called.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the HTTP method, case-insensitively. Only `"POST"` is currently
    /// supported (the underlying `HttpPost` seam has no other verb); any
    /// other value fails [`Self::process`] with a `NodeError` naming the
    /// unsupported method rather than silently sending a POST anyway.
    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Set the JSON body to send. Defaults to `Value::Null`.
    #[must_use]
    pub fn with_body(mut self, body: Value) -> Self {
        self.body = body;
        self
    }

    /// Set the headers (name/value pairs) to attach to the outbound
    /// request. Defaults to none.
    #[must_use]
    pub fn with_headers(
        mut self,
        headers: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.headers = headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        self
    }
}

impl Default for HttpRequestNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for HttpRequestNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let url = self.url.clone().ok_or_else(|| {
            NodeError::new(format!("{NODE_NAME}: no URL configured (call with_url)"))
        })?;

        if !self.method.eq_ignore_ascii_case("POST") {
            return Err(NodeError::new(format!(
                "{NODE_NAME}: unsupported HTTP method \"{}\" - only POST is currently supported \
                 over the HttpPost seam",
                self.method
            )));
        }

        let header_refs: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let response = self
            .http_post
            .post_with_headers(&url, self.body.clone(), &header_refs)
            .await
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: request failed: {err}")))?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "status": response.status,
                "body": response.body,
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

    use crate::nodes::http_post::StubHttpPost;

    use super::*;

    fn empty_ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn process_posts_the_configured_url_body_and_headers() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HttpRequestNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("http://mini:8010/api/jobs/watch-run")
            .with_body(json!({"trigger": "nightly"}))
            .with_headers([("X-API-Key", "secret")]);

        let ctx = node
            .process(empty_ctx())
            .await
            .expect("process should succeed");

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "http://mini:8010/api/jobs/watch-run");
        assert_eq!(body, json!({"trigger": "nightly"}));

        let headers = stub
            .last_headers()
            .expect("post_with_headers should have been used");
        assert_eq!(
            headers,
            vec![("X-API-Key".to_string(), "secret".to_string())]
        );

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["body"], json!({"ok": true}));
    }

    #[tokio::test]
    async fn process_works_with_no_headers_configured() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HttpRequestNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("http://mini:8010/api/jobs/watch-run");

        node.process(empty_ctx())
            .await
            .expect("process should succeed");

        assert_eq!(stub.last_headers(), Some(Vec::new()));
    }

    #[tokio::test]
    async fn process_errors_without_a_configured_url() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HttpRequestNode::new().with_http_post(std::sync::Arc::new(stub));

        let err = node
            .process(empty_ctx())
            .await
            .expect_err("should fail without a URL");
        assert!(err.message.contains("no URL configured"));
    }

    #[tokio::test]
    async fn process_errors_on_an_unsupported_method() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = HttpRequestNode::new()
            .with_http_post(std::sync::Arc::new(stub))
            .with_url("http://mini:8010/api/jobs/watch-run")
            .with_method("DELETE");

        let err = node
            .process(empty_ctx())
            .await
            .expect_err("should fail on an unsupported method");
        assert!(err.message.contains("unsupported HTTP method"));
        assert!(err.message.contains("DELETE"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = HttpRequestNode::default();
    }
}
