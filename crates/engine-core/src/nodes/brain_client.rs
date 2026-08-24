//! `brain_client` — the injectable HTTP-GET seam engine-rs uses to read back
//! from Synapse's `GET /recall` (`EN.6.K` task 1), plus `BrainConfig`, the
//! shared env-driven base URL / API key config both the read seam and (per
//! `EN.6.K` task 3) the outbound `HttpPost` ingest nodes read from.
//!
//! Authorized by [D23](../../../../../planning/decisions/D23-brain-read-seam.md) — the operator
//! ruling that closed the `d9-read-seam-decision` gate [D9](../../../../../planning/decisions/D9-engine-brain-boundary.md)
//! left open. D23 lands as a separate transport beside `http_post`'s `HttpPost`
//! (never a generalized "HTTP client" merging the two directions), a
//! typed consumer of whatever shape Synapse pins for `GET /recall` (never a
//! second implementation of ranking/fusion/retrieval — that stays wholly
//! Synapse's per brain D51/D53), and read-only: this seam never embeds,
//! never opens `pgvector`, and never writes a corpus row (`CLAUDE.md`'s
//! BOUNDARY TEST, hard rule).
//!
//! Patterned directly on [`super::http_post`]: a trait so production code
//! reaches for a real `reqwest`-backed GET ([`ReqwestHttpGet`]) while tests
//! inject a [`StubHttpGet`] that records the last call it was handed — no
//! live network call in the gated `cargo nextest` suite. Unlike
//! `HttpPost::post_with_headers` (a default-impl method that silently
//! discards `headers` unless a caller specifically overrides it — the trap
//! called out in this module's originating task), [`HttpGet::fetch`] puts
//! `headers` in its one and only method signature, so there is no default
//! path that quietly drops them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

/// Env var `BrainConfig::from_env` reads for the Brain (Synapse) base URL —
/// e.g. `"http://localhost:8000"`. Required: construction fails with a clear
/// error if unset, rather than a node discovering the misconfiguration mid-run.
pub const BRAIN_API_URL_ENV: &str = "BRAIN_API_URL";

/// Env var `BrainConfig::from_env` reads for the `X-API-Key` header value.
/// Optional — a locally-run Brain with `require_api_key` disabled has no
/// key to send; `BrainConfig::from_env` logs a warning (not an error) when
/// it is absent, since a request sent with no key is a legitimate outcome
/// the server enforces (or does not), not an engine-side failure.
pub const BRAIN_API_KEY_ENV: &str = "BRAIN_API_KEY";

/// The injectable HTTP-GET seam: `fetch(url, query, headers)` -> a parsed
/// JSON body on success, or an error string describing the transport/status
/// failure. `headers` lives in the primary method signature (not a second,
/// default-impl method the way `HttpPost::post_with_headers` is) so no live
/// or stub implementation can silently drop them the way that trap allows.
#[async_trait]
pub trait HttpGet: Send + Sync {
    async fn fetch(
        &self,
        url: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<Value, String>;
}

/// The real HTTP GET: `reqwest::Client::get(url).query(..).header(..).send()`,
/// collapsed into the [`HttpGet`] seam's `Result<Value, String>` shape. Any
/// transport error becomes an `Err` string; a non-2xx status is also an
/// `Err`, carrying the status code and a short body snippet so a caller (or
/// a test asserting on the message) can tell a 401 from a 500 without a
/// second round trip.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestHttpGet;

/// How much of a non-2xx response body to fold into the error message —
/// enough to see a JSON error's `detail` field, not so much that a large
/// HTML error page floods a log line.
const ERROR_BODY_SNIPPET_LEN: usize = 500;

#[async_trait]
impl HttpGet for ReqwestHttpGet {
    async fn fetch(
        &self,
        url: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<Value, String> {
        let mut request = reqwest::Client::new().get(url).query(query);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request
            .send()
            .await
            .map_err(|err| format!("brain read request failed: {err}"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(ERROR_BODY_SNIPPET_LEN).collect();
            return Err(format!(
                "brain read endpoint returned HTTP {status}: {snippet}"
            ));
        }

        response
            .json::<Value>()
            .await
            .map_err(|err| format!("brain read response was not valid JSON: {err}"))
    }
}

/// Convenience constructor: an `Arc<dyn HttpGet>` wrapping [`ReqwestHttpGet`].
/// Production callers (e.g. `RecallNode::new`) reach for this; tests build a
/// [`StubHttpGet`] instead, so the gated suite never contacts a live Brain.
#[must_use]
pub fn http_get_live() -> Arc<dyn HttpGet> {
    Arc::new(ReqwestHttpGet)
}

/// A `fetch` call's recorded query params / headers, as owned `(name,
/// value)` pairs.
type RecordedPairs = Vec<(String, String)>;

/// Test-stub `HttpGet`: records the last `(url, query, headers)` call it
/// received and returns a configurable success/failure response, so unit
/// tests (this module, `RecallNode`'s) can assert on both the outbound
/// request shape and how a node handles a failure — mirrors
/// [`super::http_post::StubHttpPost`].
#[derive(Clone)]
pub struct StubHttpGet {
    last_call: Arc<Mutex<Option<(String, RecordedPairs, RecordedPairs)>>>,
    result: Arc<Mutex<Result<Value, String>>>,
}

impl StubHttpGet {
    /// A stub that always succeeds with the given JSON body.
    #[must_use]
    pub fn succeeding(body: Value) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(body))),
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

    /// The `(url, query, headers)` triple passed to the most recent `fetch`
    /// call, if any — query and headers as owned `(name, value)` pairs.
    #[must_use]
    pub fn last_call(&self) -> Option<(String, RecordedPairs, RecordedPairs)> {
        self.last_call.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpGet for StubHttpGet {
    async fn fetch(
        &self,
        url: &str,
        query: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Result<Value, String> {
        *self.last_call.lock().unwrap() = Some((
            url.to_string(),
            query
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        ));
        self.result.lock().unwrap().clone()
    }
}

/// Error returned by [`BrainConfig::from_env`] when [`BRAIN_API_URL_ENV`] is
/// unset — a construction-time error with a clear message, per this block's
/// acceptance criteria, rather than a runtime surprise mid-workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainConfigError(pub String);

impl std::fmt::Display for BrainConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BrainConfigError {}

/// Deployment-topology config shared by every Brain client node: the read
/// seam ([`super::brain_client`]'s future `RecallNode`) and, per this
/// block's task 3, the outbound `HttpPost` ingest nodes that currently carry
/// their own hardcoded `localhost:8000` consts. Modeled on
/// `WorkflowTriggerDispatch::new`'s env-read shape
/// (`channel_transport.rs:183-192`). Per standing rule 6, the base URL and
/// API key are deployment config (env), not a per-run `Policy` knob — same
/// treatment `BRAIN_INGEST_URL` got under D9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainConfig {
    /// The Brain (Synapse) base URL, e.g. `"http://localhost:8000"` — no
    /// trailing slash assumed either way; callers join it with a
    /// leading-slash path (`format!("{base_url}/recall")`).
    pub base_url: String,
    /// The `X-API-Key` header value, if configured. `None` means "send no
    /// auth header" — a legitimate outcome for a locally-run Brain with
    /// `require_api_key` disabled; the server enforces whether that is
    /// actually acceptable, not this config.
    pub api_key: Option<String>,
}

impl BrainConfig {
    /// Build a `BrainConfig` from [`BRAIN_API_URL_ENV`] (required) and
    /// [`BRAIN_API_KEY_ENV`] (optional, `tracing::warn!` when absent).
    ///
    /// # Errors
    ///
    /// Returns [`BrainConfigError`] if `BRAIN_API_URL` is unset or empty —
    /// a construction-time failure with a clear message, not a runtime
    /// surprise the first time a workflow tries to reach the Brain.
    pub fn from_env() -> Result<Self, BrainConfigError> {
        let base_url = std::env::var(BRAIN_API_URL_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                BrainConfigError(format!(
                    "{BRAIN_API_URL_ENV} is not set — the Brain read/ingest seams need a base \
                     URL (e.g. \"http://localhost:8000\") to reach Synapse"
                ))
            })?;

        let api_key = std::env::var(BRAIN_API_KEY_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty());
        if api_key.is_none() {
            tracing::warn!(
                "{BRAIN_API_KEY_ENV} is not set — Brain requests will be sent with no \
                 X-API-Key header"
            );
        }

        Ok(Self { base_url, api_key })
    }

    /// Build a `BrainConfig` directly, bypassing the environment — the
    /// constructor tests and node builders (`with_config`) reach for.
    #[must_use]
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
        }
    }

    /// The `[("X-API-Key", key)]` header pair to attach to a request, or an
    /// empty slice-equivalent `Vec` when no key is configured. Returns an
    /// owned `Vec` (rather than borrowing) so callers can freely combine it
    /// with other headers before handing a `&[(&str, &str)]` slice to
    /// [`HttpGet::fetch`] / `HttpPost::post_with_headers`.
    #[must_use]
    pub fn auth_headers(&self) -> Vec<(String, String)> {
        match &self.api_key {
            Some(key) => vec![("X-API-Key".to_string(), key.clone())],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    // Serializes the `from_env` tests since they mutate process-global env
    // vars — run under `cargo nextest`'s per-test process isolation this
    // would be unnecessary, but keeps the module correct under plain
    // `cargo test` too.
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn clear_env() {
        std::env::remove_var(BRAIN_API_URL_ENV);
        std::env::remove_var(BRAIN_API_KEY_ENV);
    }

    #[test]
    fn from_env_reads_both_vars_when_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(BRAIN_API_URL_ENV, "http://localhost:8000");
        std::env::set_var(BRAIN_API_KEY_ENV, "secret-key");

        let config = BrainConfig::from_env().expect("both vars set should succeed");

        assert_eq!(config.base_url, "http://localhost:8000");
        assert_eq!(config.api_key, Some("secret-key".to_string()));
        clear_env();
    }

    #[test]
    fn from_env_allows_a_missing_api_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(BRAIN_API_URL_ENV, "http://localhost:8000");

        let config = BrainConfig::from_env().expect("missing key alone should still succeed");

        assert_eq!(config.base_url, "http://localhost:8000");
        assert_eq!(config.api_key, None);
        clear_env();
    }

    #[test]
    fn from_env_errors_clearly_when_url_is_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();

        let error = BrainConfig::from_env().expect_err("missing URL should be a clear error");

        assert!(
            error.0.contains(BRAIN_API_URL_ENV),
            "error message should name the missing env var: {error}"
        );
        clear_env();
    }

    #[test]
    fn from_env_errors_when_url_is_blank() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(BRAIN_API_URL_ENV, "   ");

        let error = BrainConfig::from_env().expect_err("blank URL should be treated as unset");

        assert!(error.0.contains(BRAIN_API_URL_ENV));
        clear_env();
    }

    #[test]
    fn auth_headers_carries_the_key_when_present() {
        let config = BrainConfig::new("http://localhost:8000", Some("k-123".to_string()));
        assert_eq!(
            config.auth_headers(),
            vec![("X-API-Key".to_string(), "k-123".to_string())]
        );
    }

    #[test]
    fn auth_headers_is_empty_when_key_absent() {
        let config = BrainConfig::new("http://localhost:8000", None);
        assert!(config.auth_headers().is_empty());
    }

    #[tokio::test]
    async fn stub_http_get_records_url_query_and_headers() {
        let stub = StubHttpGet::succeeding(json!({"results": []}));

        let response = stub
            .fetch(
                "http://localhost:8000/recall",
                &[("q", "roadmap"), ("limit", "5")],
                &[("X-API-Key", "k-123")],
            )
            .await
            .expect("succeeding stub should return Ok");

        assert_eq!(response, json!({"results": []}));

        let (url, query, headers) = stub.last_call().expect("fetch should have been recorded");
        assert_eq!(url, "http://localhost:8000/recall");
        assert_eq!(
            query,
            vec![
                ("q".to_string(), "roadmap".to_string()),
                ("limit".to_string(), "5".to_string())
            ]
        );
        assert_eq!(
            headers,
            vec![("X-API-Key".to_string(), "k-123".to_string())]
        );
    }

    #[tokio::test]
    async fn stub_http_get_returns_a_configurable_failure() {
        let stub = StubHttpGet::failing("brain endpoint unreachable");

        let result = stub.fetch("http://localhost:8000/recall", &[], &[]).await;

        assert_eq!(result, Err("brain endpoint unreachable".to_string()));
        assert!(
            stub.last_call().is_some(),
            "a failing fetch is still recorded as attempted"
        );
    }

    #[tokio::test]
    async fn stub_http_get_records_a_call_with_no_headers() {
        let stub = StubHttpGet::succeeding(json!({}));

        stub.fetch("http://localhost:8000/recall", &[("q", "x")], &[])
            .await
            .expect("succeeding stub should return Ok");

        let (_, _, headers) = stub.last_call().expect("fetch should have been recorded");
        assert!(headers.is_empty());
    }
}
