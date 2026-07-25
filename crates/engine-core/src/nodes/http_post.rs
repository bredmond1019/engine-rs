//! `http_post` — the injectable HTTP-POST seam `PersistToBrainNode`
//! (`EN.4.C` task 9) calls to push the finished `AutomationRoadmap` to the
//! brain ingest endpoint (Synapse's `POST /ingest/*`, `OR.Q`).
//!
//! Patterned on `crate::nodes::openai_compat_transport`'s `LocalHttpPost`
//! seam: a trait so production code reaches for a real `reqwest`-backed POST
//! ([`http_post_live`]) while tests inject a stub that records the last
//! payload it was handed — no live network call in the gated `cargo test`
//! suite. Per THE BOUNDARY TEST (`CLAUDE.md`), this seam only POSTs; it never
//! embeds, never touches pgvector, and never writes to the corpus itself —
//! that all stays behind the brain-side endpoint.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

/// The result of a successful `HttpPost::post` call: the response status
/// code and the parsed JSON body (or `Value::Null` if the response had no
/// body / wasn't JSON — callers that only care about success can ignore it).
#[derive(Debug, Clone, PartialEq)]
pub struct HttpPostResponse {
    pub status: u16,
    pub body: Value,
}

/// The injectable HTTP-POST seam: `post(url, json_body)` -> a
/// [`HttpPostResponse`] on success, or an error string describing the
/// transport/status failure. Async trait so both the live `reqwest`
/// implementation and test stubs can be swapped in behind a single
/// `Arc<dyn HttpPost>` — mirrors `LocalHttpPost`'s closure seam but as a
/// trait object so `PersistToBrainNode::with_http_post` can hold either
/// interchangeably.
#[async_trait]
pub trait HttpPost: Send + Sync {
    async fn post(&self, url: &str, json_body: Value) -> Result<HttpPostResponse, String>;
}

/// The real HTTP POST: `reqwest::Client::post(url).json(&body).send()`,
/// collapsed into the [`HttpPost`] seam's `Result<HttpPostResponse, String>`
/// shape. Any transport error becomes an `Err` string; non-2xx statuses are
/// still surfaced as an `Err` (unlike `openai_compat_transport`'s
/// fail-fast-and-fallback local tier, there is no fallback for a brain-push
/// failure — the caller decides how to handle it).
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestHttpPost;

#[async_trait]
impl HttpPost for ReqwestHttpPost {
    async fn post(&self, url: &str, json_body: Value) -> Result<HttpPostResponse, String> {
        let response = reqwest::Client::new()
            .post(url)
            .json(&json_body)
            .send()
            .await
            .map_err(|err| format!("brain ingest request failed: {err}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("brain ingest endpoint returned HTTP {status}"));
        }

        let body = response.json::<Value>().await.unwrap_or(Value::Null);

        Ok(HttpPostResponse {
            status: status.as_u16(),
            body,
        })
    }
}

/// Convenience constructor: an `Arc<dyn HttpPost>` wrapping [`ReqwestHttpPost`].
/// Production callers (`persist_to_brain.rs`'s `new()`) reach for this; tests
/// build a [`StubHttpPost`] instead, so the gated suite never contacts a live
/// brain endpoint.
#[must_use]
pub fn http_post_live() -> Arc<dyn HttpPost> {
    Arc::new(ReqwestHttpPost)
}

/// Test-stub `HttpPost`: records the last `(url, json_body)` pair it was
/// POSTed and returns a configurable success/failure response, so unit tests
/// (this module, `persist_to_brain.rs`, the e2e test) can assert on both the
/// outbound payload and how the node handles a failure.
#[derive(Clone)]
pub struct StubHttpPost {
    last_call: Arc<Mutex<Option<(String, Value)>>>,
    result: Arc<Mutex<Result<HttpPostResponse, String>>>,
}

impl StubHttpPost {
    /// A stub that always succeeds with `status: 200` and the given body.
    #[must_use]
    pub fn succeeding(body: Value) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(HttpPostResponse { status: 200, body }))),
        }
    }

    /// A stub that always fails with the given error message.
    #[must_use]
    pub fn failing(error: impl Into<String>) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Err(error.into()))),
        }
    }

    /// The `(url, json_body)` pair passed to the most recent `post` call, if
    /// any.
    #[must_use]
    pub fn last_call(&self) -> Option<(String, Value)> {
        self.last_call.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpPost for StubHttpPost {
    async fn post(&self, url: &str, json_body: Value) -> Result<HttpPostResponse, String> {
        *self.last_call.lock().unwrap() = Some((url.to_string(), json_body));
        self.result.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn stub_records_the_payload_it_is_posted() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));

        let response = stub
            .post(
                "https://brain.example/ingest/proposal",
                json!({"artifact_id": "abc123", "company_name": "Acme"}),
            )
            .await
            .expect("succeeding stub should return Ok");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, json!({"ok": true}));

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/proposal");
        assert_eq!(
            body,
            json!({"artifact_id": "abc123", "company_name": "Acme"})
        );
    }

    #[tokio::test]
    async fn stub_returns_a_configurable_failure() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");

        let result = stub
            .post("https://brain.example/ingest/proposal", json!({}))
            .await;

        assert_eq!(result, Err("brain endpoint unreachable".to_string()));
        assert!(
            stub.last_call().is_some(),
            "a failing post is still recorded as attempted"
        );
    }

    #[tokio::test]
    async fn stub_returns_null_body_by_default_success_variant_still_records_call() {
        let stub = StubHttpPost::succeeding(Value::Null);

        let response = stub
            .post("https://brain.example/ingest/proposal", json!({"x": 1}))
            .await
            .expect("succeeding stub should return Ok");

        assert_eq!(response.body, Value::Null);
    }

    #[test]
    fn live_impl_constructs_without_panicking() {
        let _live: Arc<dyn HttpPost> = http_post_live();
    }
}
