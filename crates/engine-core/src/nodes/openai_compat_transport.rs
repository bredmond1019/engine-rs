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
//! reachable. The `Config` handed to the fallback has its `model` cleared
//! first ([`clear_local_model`]) — it holds the LOCAL model name, which the
//! cloud `claude` CLI would reject with a 404.

use std::collections::BTreeMap;
use std::sync::Arc;

use claude_code_rs::parse::{ModelUsage, Usage};
use claude_code_rs::{Config, Outcome};
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::nodes::claude_code_step::{MetaTransport, TransportInfo};
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
        structured_output: None,
    })
}

/// The `name` field Ollama's OpenAI-compat `response_format.json_schema`
/// wrapper requires. `Config.json_schema` (`claude-code-rs`) carries only the
/// bare JSON Schema value with no name of its own, so this transport supplies
/// a single stable, run-invariant name for every schema-constrained call —
/// nothing downstream reads this string back, it only needs to satisfy the
/// wire format's required field.
const JSON_SCHEMA_RESPONSE_NAME: &str = "structured_response";

/// Build the `/v1/chat/completions` request body. When
/// `local.constrained_json` is set:
/// - if `json_schema` is `Some`, sets `response_format` to the OpenAI/Ollama
///   `json_schema` structured-output shape (`{"type": "json_schema",
///   "json_schema": {"name": ..., "schema": <json_schema>}}`), confirmed
///   live against Ollama in this ticket's Task 1 (see the Amendment Log in
///   `planning/ticket-local-schema-constrained-json/tasks.md`) — this
///   enforces field *types*, not just syntactic JSON validity.
/// - if `json_schema` is `None`, falls back to the original generic
///   `{"type": "json_object"}` hint (valid-JSON-only, no type enforcement) —
///   unchanged pre-ticket behavior for callers with no schema to offer.
fn build_request_body(local: &LocalConfig, prompt: &str, json_schema: Option<&Value>) -> Value {
    let mut body = json!({
        "model": local.model,
        "messages": [{ "role": "user", "content": prompt }],
    });
    if local.constrained_json {
        body["response_format"] = match json_schema {
            Some(schema) => json!({
                "type": "json_schema",
                "json_schema": {
                    "name": JSON_SCHEMA_RESPONSE_NAME,
                    "schema": schema,
                },
            }),
            None => json!({ "type": "json_object" }),
        };
    }
    body
}

/// Strip the local model name off a `Config` before handing it to the cloud
/// fallback.
///
/// **Why this is necessary.** `policy::shaping::apply_model_tier(config,
/// ModelTier::Local, local_model)` sets `config.model =
/// Some("<local model>")` (e.g. `"qwen2.5:3b"`). The cloud fallback is the
/// `claude` CLI, which has never heard of that model, so forwarding the
/// `Config` unchanged made the fallback fail with
/// `claude API error (HTTP 404): There's an issue with the selected model
/// (qwen2.5:3b)` — i.e. the fallback was useless for the single most likely
/// local-side failure (the model isn't pulled / the endpoint is unusable).
/// Setting `model` to `None` lets `claude-code-rs` apply the CLI's own
/// default model.
///
/// **Why clearing is safe.** `openai_compat_transport` /
/// `openai_compat_meta_transport` are only ever wired when the resolved tier
/// for that stage is `ModelTier::Local` (see `graph.rs`'s
/// `registry_for_policy`), so `config.model` here is ALWAYS the local model
/// string — clearing it cannot affect any non-local path.
///
/// **Two deliberate non-choices**, recorded here rather than implemented:
/// - No `fallback_model` field on [`LocalConfig`]. A configurable fallback
///   model would be a standing-rule-6 knob, requiring an explicit setting in
///   every named profile across four workflows — disproportionate for an
///   error path. `None` (the CLI default) is the honest choice: the stage
///   declared `local`, so no cloud tier was ever specified for it.
/// - The fallback stays quiet, not loud/fatal — it is deliberately a
///   fallback. It is already ATTRIBUTABLE rather than silent:
///   [`openai_compat_meta_transport`] stamps `{"tier": "cloud", "model":
///   <the fallback's primary model>, "endpoint": None}`, and
///   `ClaudeCodeStep`'s plain-transport branch stamps a generic `"cloud"`
///   tier — so telemetry records what actually ran, not what policy intended.
fn clear_local_model(mut config: Config) -> Config {
    config.model = None;
    config
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
            let body = build_request_body(&local, &prompt, config.json_schema.as_ref());

            let local_result = match (http_post)(url, body).await {
                Ok(response) => outcome_from_chat_completion(&local.model, &response),
                Err(err) => Err(err),
            };

            match local_result {
                Ok(outcome) => Ok(outcome),
                Err(_local_err) => (cloud_fallback)(clear_local_model(config), prompt).await,
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

/// [`openai_compat_transport`]'s tier-aware sibling: builds a
/// [`MetaTransport`] (`EN.5.D` task 9) instead of a plain [`ModelTransport`],
/// so the caller's `ClaudeCodeStep` (via `with_meta_transport`) can stamp
/// what actually ran rather than what the resolved policy intended. Local
/// success stamps `{"tier": "local", "model": local.model, "endpoint":
/// Some(local.endpoint)}`; the cloud fallback — reached on any local-side
/// error — stamps `{"tier": "cloud", "model": <the fallback's primary
/// model>, "endpoint": None}`, which is exactly the case intent-derived
/// telemetry gets wrong: the resolved policy said `local`, but this call
/// silently fell back to cloud.
#[must_use]
pub fn openai_compat_meta_transport(
    local: LocalConfig,
    http_post: LocalHttpPost,
    cloud_fallback: ModelTransport,
) -> MetaTransport {
    Arc::new(move |config: Config, prompt: String| {
        let local = local.clone();
        let http_post = Arc::clone(&http_post);
        let cloud_fallback = Arc::clone(&cloud_fallback);

        Box::pin(async move {
            let url = format!(
                "{}/v1/chat/completions",
                local.endpoint.trim_end_matches('/')
            );
            let body = build_request_body(&local, &prompt, config.json_schema.as_ref());

            let local_result = match (http_post)(url, body).await {
                Ok(response) => outcome_from_chat_completion(&local.model, &response),
                Err(err) => Err(err),
            };

            match local_result {
                Ok(outcome) => {
                    let info = TransportInfo {
                        tier: "local".to_string(),
                        model: local.model.clone(),
                        endpoint: Some(local.endpoint.clone()),
                    };
                    Ok((outcome, info))
                }
                Err(_local_err) => {
                    let outcome = (cloud_fallback)(clear_local_model(config), prompt).await?;
                    let model = outcome.primary_model().unwrap_or("unknown").to_string();
                    let info = TransportInfo {
                        tier: "cloud".to_string(),
                        model,
                        endpoint: None,
                    };
                    Ok((outcome, info))
                }
            }
        })
    })
}

/// Convenience: [`openai_compat_meta_transport`] wired to the real
/// `reqwest` HTTP POST ([`default_local_http_post`]). Production callers
/// reach for this once `graph.rs` migrates to `ClaudeCodeStep`'s
/// `with_meta_transport` seam; tests build the transport directly with a
/// stubbed `http_post` instead.
#[must_use]
pub fn openai_compat_meta_transport_live(
    local: LocalConfig,
    cloud_fallback: ModelTransport,
) -> MetaTransport {
    openai_compat_meta_transport(local, default_local_http_post(), cloud_fallback)
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
                    structured_output: None,
                })
            })
        })
    }

    /// A cloud fallback that RECORDS the `Config` it was handed (the
    /// `panicking_cloud_fallback` below can only assert the fallback is
    /// *not* reached; this one asserts what it *receives*).
    fn capturing_cloud_stub(text: &str) -> (ModelTransport, Arc<std::sync::Mutex<Option<Config>>>) {
        let seen: Arc<std::sync::Mutex<Option<Config>>> = Arc::new(std::sync::Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        let inner = cloud_stub(text);
        let transport: ModelTransport = Arc::new(move |config: Config, prompt: String| {
            *seen_clone.lock().unwrap() = Some(config.clone());
            (inner)(config, prompt)
        });
        (transport, seen)
    }

    /// A `Config` shaped the way `policy::shaping::apply_model_tier(..,
    /// ModelTier::Local, ..)` shapes it: `model` holds the LOCAL model name.
    fn local_tier_config() -> Config {
        Config {
            model: Some(test_local_config().model),
            ..Config::default()
        }
    }

    /// Local endpoint returns HTTP-level success but a body that fails
    /// `outcome_from_chat_completion` parsing — the second local-failure
    /// shape the code distinguishes.
    fn malformed_stub_response() -> LocalHttpPost {
        Arc::new(|_url, _body| Box::pin(async { Ok(json!({ "unexpected": "shape" })) }))
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

    fn capturing_local_http_post() -> (LocalHttpPost, Arc<std::sync::Mutex<Option<Value>>>) {
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
        (capturing, seen_body)
    }

    /// `constrained_json: true` + no `Config.json_schema` must keep the
    /// pre-ticket generic `{"type": "json_object"}` behavior unchanged
    /// (regression coverage — task 3, AC 2).
    #[tokio::test]
    async fn constrained_json_with_no_schema_falls_back_to_generic_json_object() {
        let (capturing, seen_body) = capturing_local_http_post();

        let local = LocalConfig {
            constrained_json: true,
            ..test_local_config()
        };
        let transport = openai_compat_transport(local, capturing, panicking_cloud_fallback());

        // `Config::default()` carries no `json_schema` — the no-schema path.
        let _ = transport(Config::default(), "hello".to_string()).await;

        let body = seen_body
            .lock()
            .unwrap()
            .clone()
            .expect("http_post was called");
        assert_eq!(
            body["response_format"],
            json!({ "type": "json_object" }),
            "constrained_json with no schema must fall back to generic json_object mode, unchanged"
        );
    }

    /// `constrained_json: true` + a `Config.json_schema` must send that
    /// schema through in `response_format` (the OpenAI/Ollama `json_schema`
    /// structured-output shape), not the generic `json_object` mode —
    /// task 3, AC 1. Asserts on the full structure/content, not just
    /// presence, per the ticket's testing strategy.
    #[tokio::test]
    async fn constrained_json_with_schema_sends_schema_constrained_response_format() {
        let (capturing, seen_body) = capturing_local_http_post();

        let local = LocalConfig {
            constrained_json: true,
            ..test_local_config()
        };
        let transport = openai_compat_transport(local, capturing, panicking_cloud_fallback());

        let schema = json!({
            "type": "object",
            "properties": {
                "team_size": { "type": "integer" },
                "company_name": { "type": "string" },
            },
            "required": ["team_size", "company_name"],
        });
        let config = Config {
            json_schema: Some(schema.clone()),
            ..Config::default()
        };

        let _ = transport(config, "hello".to_string()).await;

        let body = seen_body
            .lock()
            .unwrap()
            .clone()
            .expect("http_post was called");
        assert_eq!(
            body["response_format"],
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": JSON_SCHEMA_RESPONSE_NAME,
                    "schema": schema,
                },
            }),
            "constrained_json with a schema must send the schema-constrained \
             response_format shape, not generic json_object mode"
        );
        // The specific regression this ticket exists to fix: field types
        // (e.g. team_size: integer) must be present in the schema sent
        // through, not collapsed into a generic "any valid JSON" hint.
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["properties"]["team_size"]["type"],
            json!("integer")
        );
    }

    // -- `openai_compat_meta_transport` (task 9) --

    #[tokio::test]
    async fn meta_transport_stamps_local_tier_and_endpoint_on_success() {
        let transport = openai_compat_meta_transport(
            test_local_config(),
            ok_stub_response("local reply"),
            panicking_cloud_fallback(),
        );

        let (outcome, info) = transport(Config::default(), "hello".to_string())
            .await
            .expect("stubbed local call should succeed");

        assert_eq!(outcome.text, "local reply");
        assert_eq!(info.tier, "local");
        assert_eq!(info.model, "qwen2.5-coder:7b");
        assert_eq!(info.endpoint.as_deref(), Some("http://localhost:11434"));
    }

    #[tokio::test]
    async fn meta_transport_stamps_cloud_tier_with_no_endpoint_when_local_is_down() {
        let transport = openai_compat_meta_transport(
            test_local_config(),
            down_stub_response(),
            cloud_stub("cloud reply"),
        );

        let (outcome, info) = transport(Config::default(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed");

        assert_eq!(outcome.text, "cloud reply");
        assert_eq!(
            info.tier, "cloud",
            "a down local endpoint must stamp the cloud fallback's tier, \
             not the resolved policy's `local` tier"
        );
        assert_eq!(info.model, "claude-sonnet-4-5");
        assert_eq!(
            info.endpoint, None,
            "the cloud fallback has no single endpoint to stamp"
        );
    }

    #[tokio::test]
    async fn meta_transport_stamps_cloud_tier_when_local_response_is_malformed() {
        let malformed: LocalHttpPost =
            Arc::new(|_url, _body| Box::pin(async { Ok(json!({ "unexpected": "shape" })) }));

        let transport =
            openai_compat_meta_transport(test_local_config(), malformed, cloud_stub("cloud reply"));

        let (_outcome, info) = transport(Config::default(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed on malformed local response");

        assert_eq!(info.tier, "cloud");
        assert_eq!(info.endpoint, None);
    }

    // -- regression: the cloud fallback must not be handed the LOCAL model --
    //
    // `apply_model_tier(.., ModelTier::Local, ..)` puts the local model name
    // in `config.model`. Forwarding that to the `claude` CLI produced
    // `HTTP 404: There's an issue with the selected model (qwen2.5:3b)`
    // (observed live, run `bd034156`), making the fallback useless for the
    // most likely local-side failure. All four tests below drive the failure
    // through the stubbed `http_post` seam, so they never contact a live
    // Ollama.

    #[tokio::test]
    async fn fallback_clears_local_model_when_local_endpoint_errors() {
        let (cloud, seen) = capturing_cloud_stub("cloud reply");
        let transport = openai_compat_transport(test_local_config(), down_stub_response(), cloud);

        let outcome = transport(local_tier_config(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed");
        assert_eq!(outcome.text, "cloud reply");

        let config = seen.lock().unwrap().clone().expect("fallback was called");
        assert_eq!(
            config.model,
            None,
            "the cloud fallback must not be handed the local model name \
             (`{}`) — the `claude` CLI 404s on it",
            test_local_config().model
        );
    }

    #[tokio::test]
    async fn fallback_clears_local_model_when_local_response_is_malformed() {
        let (cloud, seen) = capturing_cloud_stub("cloud reply");
        let transport =
            openai_compat_transport(test_local_config(), malformed_stub_response(), cloud);

        let _ = transport(local_tier_config(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed on malformed local response");

        let config = seen.lock().unwrap().clone().expect("fallback was called");
        assert_eq!(
            config.model, None,
            "a parse failure on the local body must reach the fallback with \
             the local model cleared, exactly like a transport error"
        );
    }

    #[tokio::test]
    async fn meta_transport_fallback_clears_local_model_when_local_endpoint_errors() {
        let (cloud, seen) = capturing_cloud_stub("cloud reply");
        let transport =
            openai_compat_meta_transport(test_local_config(), down_stub_response(), cloud);

        let (outcome, info) = transport(local_tier_config(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed");
        assert_eq!(outcome.text, "cloud reply");
        assert_eq!(
            info.tier, "cloud",
            "clearing the model must not regress the fallback's tier stamp"
        );

        let config = seen.lock().unwrap().clone().expect("fallback was called");
        assert_eq!(config.model, None);
    }

    #[tokio::test]
    async fn meta_transport_fallback_clears_local_model_when_local_response_is_malformed() {
        let (cloud, seen) = capturing_cloud_stub("cloud reply");
        let transport =
            openai_compat_meta_transport(test_local_config(), malformed_stub_response(), cloud);

        let (_outcome, info) = transport(local_tier_config(), "hello".to_string())
            .await
            .expect("cloud fallback should succeed on malformed local response");
        assert_eq!(info.tier, "cloud");

        let config = seen.lock().unwrap().clone().expect("fallback was called");
        assert_eq!(config.model, None);
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
