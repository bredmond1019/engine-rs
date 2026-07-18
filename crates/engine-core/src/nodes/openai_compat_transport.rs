//! `openai_compat_transport` — the EN.3.C `local` model tier's transport.
//!
//! Satisfies the existing `ModelTransport`/D4 `Transport` signature
//! (`Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>>`)
//! by POSTing to an OpenAI-compatible `/v1/chat/completions` endpoint (e.g. a
//! local Ollama server) instead of spawning the `claude` CLI subprocess.
//! **Zero changes to `ClaudeCodeStep`**: this module only builds a closure
//! of the same shape that `ClaudeCodeStep::with_transport` (and each
//! task-loop node's own `with_transport`) already accepts — the seam is the
//! integration point, per `planning/local-llm-tier-investigation/notes.md`.
//!
//! Scope: the `local` tier is for single-shot judgment stages (triage,
//! review-gate classification, JSON-repair) — never the agentic `implement`
//! stage. This module does not itself decide which stages opt in; that's
//! `graph.rs`'s job (per-stage transport injection from the resolved
//! `SdlcPolicy`).
//!
//! Fail-fast + fallback: any local-endpoint error (connection refused,
//! non-2xx status, malformed body) is never surfaced as an `Err` from the
//! transport this module builds — it silently falls back to the supplied
//! `cloud_fallback` transport for that same call, so a stage that opts into
//! `local` never hard-fails a run just because the local server isn't
//! reachable.

use std::collections::BTreeMap;
use std::sync::Arc;

use claude_code_rs::parse::{ModelUsage, Usage};
use claude_code_rs::{Config, Outcome};
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::workflows::sdlc_flow::policy::LocalConfig;
use crate::workflows::sdlc_flow::ModelTransport;

/// The injectable HTTP-POST seam this transport calls to reach the local
/// endpoint. Defaults to a real `reqwest` POST via
/// [`default_local_http_post`]; tests substitute a stub so the gated
/// `cargo test` suite never needs a live Ollama server — mirrors
/// `ClaudeCodeStep`'s own transport seam (`EN.2.A`) and
/// `sdlc_flow::CommandRunner` (`EN.3.A`).
pub type LocalHttpPost =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

/// The real HTTP POST: `reqwest::Client::post(url).json(&body).send()`,
/// collapsed into this seam's `Result<Value, String>` shape. Any transport
/// error or non-2xx status becomes an `Err` string, which
/// [`openai_compat_transport`] treats as "local endpoint unavailable" and
/// routes to the cloud fallback.
#[must_use]
pub fn default_local_http_post() -> LocalHttpPost {
    Arc::new(|url, body| {
        Box::pin(async move {
            let response = reqwest::Client::new()
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|err| format!("local endpoint request failed: {err}"))?;

            if !response.status().is_success() {
                return Err(format!(
                    "local endpoint returned HTTP {}",
                    response.status()
                ));
            }

            response
                .json::<Value>()
                .await
                .map_err(|err| format!("local endpoint returned unparsable JSON: {err}"))
        })
    })
}

/// The subset of an OpenAI-compatible `/v1/chat/completions` response this
/// transport reads. Unknown fields are ignored (not `deny_unknown_fields`)
/// since different OpenAI-compatible servers (Ollama, vLLM, ...) vary in
/// what else they include on the envelope.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: ChatUsage,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// Synthesize a `claude_code_rs::Outcome` from a parsed chat-completion
/// response: `text` <- the first choice's message content, token counts <-
/// `usage.prompt_tokens`/`usage.completion_tokens`, `cost_usd` forced to
/// `0.0` (local inference has no per-call cloud billing), and a single
/// `model_usage` entry keyed `local/<model>` so `Outcome::primary_model()`
/// reports the local model's name rather than a cloud one.
fn outcome_from_chat_completion(model: &str, response: &Value) -> Result<Outcome, String> {
    let parsed: ChatCompletionResponse = serde_json::from_value(response.clone())
        .map_err(|err| format!("malformed chat-completion response: {err}"))?;

    let text = parsed
        .choices
        .first()
        .map(|choice| choice.message.content.clone())
        .ok_or_else(|| "chat-completion response has no choices".to_string())?;

    let mut model_usage = BTreeMap::new();
    model_usage.insert(
        format!("local/{model}"),
        ModelUsage {
            input_tokens: parsed.usage.prompt_tokens,
            output_tokens: parsed.usage.completion_tokens,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cost_usd: 0.0,
        },
    );

    Ok(Outcome {
        cost_usd: 0.0,
        usage: Usage {
            input_tokens: parsed.usage.prompt_tokens,
            output_tokens: parsed.usage.completion_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage,
        text,
        is_error: false,
        api_error_status: None,
    })
}

/// Build the `/v1/chat/completions` request body. When
/// `local.constrained_json` is set, adds a `response_format: {"type":
/// "json_object"}` hint so the server constrains decoding to valid JSON —
/// the caller (the stage consuming this transport) is expected to skip its
/// own JSON-repair retry in that case, per the spec's Context Pointers.
fn build_request_body(local: &LocalConfig, prompt: &str) -> Value {
    let mut body = json!({
        "model": local.model,
        "messages": [{ "role": "user", "content": prompt }],
    });
    if local.constrained_json {
        body["response_format"] = json!({ "type": "json_object" });
    }
    body
}

/// Build an `openai_compat_transport` [`ModelTransport`] for the `local`
/// model tier: POSTs to `local.endpoint`'s `/v1/chat/completions` via
/// `http_post` and synthesizes an `Outcome`. On any local-side failure
/// (connection error, non-2xx status, malformed body), fails fast and falls
/// back to `cloud_fallback` for that same `(config, prompt)` call — the
/// local failure is never surfaced as an `Err` to the caller.
#[must_use]
pub fn openai_compat_transport(
    local: LocalConfig,
    http_post: LocalHttpPost,
    cloud_fallback: ModelTransport,
) -> ModelTransport {
    Arc::new(move |config: Config, prompt: String| {
        let local = local.clone();
        let http_post = Arc::clone(&http_post);
        let cloud_fallback = Arc::clone(&cloud_fallback);

        Box::pin(async move {
            let url = format!(
                "{}/v1/chat/completions",
                local.endpoint.trim_end_matches('/')
            );
            let body = build_request_body(&local, &prompt);

            let local_result = match (http_post)(url, body).await {
                Ok(response) => outcome_from_chat_completion(&local.model, &response),
                Err(err) => Err(err),
            };

            match local_result {
                Ok(outcome) => Ok(outcome),
                Err(_local_err) => (cloud_fallback)(config, prompt).await,
            }
        })
    })
}

/// Convenience: [`openai_compat_transport`] wired to the real `reqwest` HTTP
/// POST ([`default_local_http_post`]). Production callers (`graph.rs`) reach
/// for this; tests build the transport directly with a stubbed `http_post`
/// instead, so the gated suite never contacts a live Ollama server.
#[must_use]
pub fn openai_compat_transport_live(
    local: LocalConfig,
    cloud_fallback: ModelTransport,
) -> ModelTransport {
    openai_compat_transport(local, default_local_http_post(), cloud_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_local_config() -> LocalConfig {
        LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        }
    }

    fn ok_stub_response(content: &str) -> LocalHttpPost {
        let content = content.to_string();
        Arc::new(move |_url, _body| {
            let content = content.clone();
            Box::pin(async move {
                Ok(json!({
                    "choices": [{ "message": { "content": content } }],
                    "usage": { "prompt_tokens": 12, "completion_tokens": 34 },
                }))
            })
        })
    }

    fn down_stub_response() -> LocalHttpPost {
        Arc::new(|_url, _body| Box::pin(async { Err("connection refused".to_string()) }))
    }

    fn cloud_stub(text: &str) -> ModelTransport {
        let text = text.to_string();
        Arc::new(move |_config, _prompt| {
            let text = text.clone();
            Box::pin(async move {
                Ok(Outcome {
                    cost_usd: 0.02,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 20,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        ModelUsage {
                            input_tokens: 10,
                            output_tokens: 20,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.02,
                        },
                    )]),
                    text,
                    is_error: false,
                    api_error_status: None,
                })
            })
        })
    }

    fn panicking_cloud_fallback() -> ModelTransport {
        Arc::new(|_config, _prompt| {
            Box::pin(async { panic!("cloud fallback must not be called when local succeeds") })
        })
    }

    #[tokio::test]
    async fn outcome_synthesis_from_stubbed_chat_completion() {
        let transport = openai_compat_transport(
            test_local_config(),
            ok_stub_response("{\"verdict\": \"PASS\"}"),
            panicking_cloud_fallback(),
        );

        let outcome = transport(Config::default(), "hello".to_string())
            .await
            .expect("stubbed local call should succeed");

        assert_eq!(outcome.text, "{\"verdict\": \"PASS\"}");
        assert_eq!(outcome.cost_usd, 0.0);
        assert_eq!(outcome.usage.input_tokens, 12);
        assert_eq!(outcome.usage.output_tokens, 34);
        assert_eq!(
            outcome.primary_model(),
            Some("local/qwen2.5-coder:7b"),
            "modelUsage must carry a single local/<model> entry"
        );
        assert_eq!(outcome.model_usage["local/qwen2.5-coder:7b"].cost_usd, 0.0);
    }

    #[tokio::test]
    async fn falls_back_to_cloud_when_local_endpoint_is_down() {
        let transport = openai_compat_transport(
            test_local_config(),
            down_stub_response(),
            cloud_stub("cloud reply"),
        );

        let outcome = transport(Config::default(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed");

        assert_eq!(outcome.text, "cloud reply");
        assert_eq!(
            outcome.primary_model(),
            Some("claude-sonnet-4-5"),
            "fallback outcome must carry the cloud model's usage, not a local/ entry"
        );
    }

    #[tokio::test]
    async fn falls_back_to_cloud_when_local_response_is_malformed() {
        let malformed: LocalHttpPost =
            Arc::new(|_url, _body| Box::pin(async { Ok(json!({ "unexpected": "shape" })) }));

        let transport =
            openai_compat_transport(test_local_config(), malformed, cloud_stub("cloud reply"));

        let outcome = transport(Config::default(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed on malformed local response");

        assert_eq!(outcome.text, "cloud reply");
    }

    #[tokio::test]
    async fn constrained_json_adds_response_format_to_request_body() {
        let seen_body = Arc::new(std::sync::Mutex::new(None));
        let seen_body_clone = Arc::clone(&seen_body);
        let capturing: LocalHttpPost = Arc::new(move |_url, body| {
            *seen_body_clone.lock().unwrap() = Some(body);
            Box::pin(async {
                Ok(json!({
                    "choices": [{ "message": { "content": "{}" } }],
                }))
            })
        });

        let local = LocalConfig {
            constrained_json: true,
            ..test_local_config()
        };
        let transport = openai_compat_transport(local, capturing, panicking_cloud_fallback());

        let _ = transport(Config::default(), "hello".to_string()).await;

        let body = seen_body
            .lock()
            .unwrap()
            .clone()
            .expect("http_post was called");
        assert_eq!(
            body["response_format"],
            json!({ "type": "json_object" }),
            "constrained_json must set response_format on the request body"
        );
    }

    #[tokio::test]
    async fn request_url_targets_v1_chat_completions() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);
        let seen_url = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_url_clone = Arc::clone(&seen_url);
        let capturing: LocalHttpPost = Arc::new(move |url, _body| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            *seen_url_clone.lock().unwrap() = url;
            Box::pin(async { Ok(json!({ "choices": [{ "message": { "content": "ok" } }] })) })
        });

        let transport =
            openai_compat_transport(test_local_config(), capturing, panicking_cloud_fallback());
        let _ = transport(Config::default(), "hello".to_string()).await;

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            *seen_url.lock().unwrap(),
            "http://localhost:11434/v1/chat/completions"
        );
    }
}
