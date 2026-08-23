//! `ClaudeCodeStep` — a reusable `Node` that spawns a Claude Code session via
//! `claude_code_rs::execute` and maps its `Outcome` into `NodeRun`/`TaskContext`
//! (`EN.2.A`).
//!
//! Per `D4-claude-code-transport-choice`, this module owns none of the
//! subprocess/argv/parse logic — that surface belongs entirely to
//! `core/claude-code-rs`. This node's only job is: build a prompt from the
//! `TaskContext`, await the SDK's `execute()`, and stamp the result onto the
//! node's own `TaskContext::nodes` entry and `NodeRun.usage`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRun, NodeRunStatus, TaskContext, Usage};
use futures::future::BoxFuture;

use crate::cancellation::CancellationToken;
use crate::node::{Node, NodeError};
use crate::workflows::sdlc_flow::policy::TransportRetry;

/// Stand-in for `Usage::model` when the SDK reports no model.
///
/// The CLI names models only as keys in its `modelUsage` map, which is empty on
/// the error envelope — so `Outcome::primary_model()` is an `Option`. The
/// orchestrator data contract (v1.0.1 §6) requires `usage.model` to be a
/// non-null string, so absence has to be spelled *somehow*; it is spelled here,
/// at the seam, rather than by loosening the contract type for a vendor quirk.
const UNKNOWN_MODEL: &str = "unknown";

/// Ceiling on the exponential backoff between retried transport attempts, in
/// milliseconds — caps [`TransportRetry::initial_backoff_ms`]'s doubling so a
/// misconfigured policy (or a large `max_attempts`) cannot turn a bounded
/// retry into a de-facto unbounded wait.
const MAX_TRANSPORT_BACKOFF_MS: u64 = 5_000;

/// Whether a `claude_code_rs::Error` is worth retrying, per
/// `ticket-implement-node-transport-retry` Task 1's Amendment Log
/// investigation of `claude_code_rs::Error`'s six variants:
/// `Spawn`/`Timeout`/`Cli`/`Api` are transient or cheap-to-retry;
/// `BinaryNotFound`/`Parse`/`Isolation` are local/deterministic failures that
/// would fail identically on a retried attempt.
fn is_retryable_transport_error(err: &claude_code_rs::Error) -> bool {
    matches!(
        err,
        claude_code_rs::Error::Spawn(_)
            | claude_code_rs::Error::Timeout
            | claude_code_rs::Error::Cli { .. }
            | claude_code_rs::Error::Api { .. }
    )
}

/// Result of [`call_with_retry`]: either the transport eventually succeeded,
/// a cancellation won a race against some attempt (never retried after), or
/// the retry budget was exhausted / the error was non-retryable (converted
/// to a `NodeError` surfacing the underlying transport message).
enum RetriedCall<T> {
    Ok(T),
    Cancelled,
    Err(NodeError),
}

/// Await one transport attempt, racing it against `token` (if present)
/// exactly as the pre-retry code did — a cancel win drops the in-flight
/// future (dropped by `tokio::select!`) and is reported distinctly from a
/// transport error so the caller never retries past a cancel.
async fn race_one_attempt<T>(
    fut: BoxFuture<'static, claude_code_rs::Result<T>>,
    token: Option<&CancellationToken>,
) -> Option<claude_code_rs::Result<T>> {
    match token {
        Some(token) => {
            tokio::select! {
                _ = token.cancelled() => None,
                result = fut => Some(result),
            }
        }
        None => Some(fut.await),
    }
}

/// Drive `make_attempt` (which builds a fresh transport future each call —
/// required because a `BoxFuture` cannot be polled twice) through a bounded
/// retry-with-backoff loop.
///
/// - A cancelled token wins immediately, on the first attempt or any later
///   one, and is never retried past — matches
///   [`ClaudeCodeStep::with_cancellation_token`]'s documented semantics.
/// - A transient error (per [`is_retryable_transport_error`]) is retried,
///   with exponential backoff (capped at [`MAX_TRANSPORT_BACKOFF_MS`]), until
///   `retry.max_attempts` is exhausted. The backoff wait itself re-checks the
///   token so a cancel during backoff also short-circuits to `Cancelled`
///   rather than firing one more attempt.
/// - A persistent error (non-retryable, or the budget exhausted) becomes a
///   [`NodeError`] carrying the underlying transport message — never a
///   generic "retries exhausted" string.
/// - `max_attempts <= 1` behaves exactly as the pre-retry code did: one
///   attempt, no backoff, first error is fatal.
async fn call_with_retry<T, F>(
    mut make_attempt: F,
    retry: TransportRetry,
    token: Option<&CancellationToken>,
) -> RetriedCall<T>
where
    F: FnMut() -> BoxFuture<'static, claude_code_rs::Result<T>>,
{
    let total_attempts = retry.max_attempts.max(1);
    let mut backoff_ms = retry.initial_backoff_ms;
    let mut attempt = 1u32;

    loop {
        match race_one_attempt(make_attempt(), token).await {
            None => return RetriedCall::Cancelled,
            Some(Ok(value)) => return RetriedCall::Ok(value),
            Some(Err(err)) => {
                let budget_exhausted = attempt >= total_attempts;
                if budget_exhausted || !is_retryable_transport_error(&err) {
                    return RetriedCall::Err(NodeError::new(err.to_string()));
                }
            }
        }

        // Backoff before the next attempt, re-checking cancellation so a
        // cancel that lands during the wait is never followed by one more
        // transport call.
        match token {
            Some(token) => {
                tokio::select! {
                    _ = token.cancelled() => return RetriedCall::Cancelled,
                    () = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                }
            }
            None => tokio::time::sleep(Duration::from_millis(backoff_ms)).await,
        }

        backoff_ms = backoff_ms.saturating_mul(2).min(MAX_TRANSPORT_BACKOFF_MS);
        attempt += 1;
    }
}

/// The injectable transport signature: takes an owned `Config` + prompt and
/// returns a boxed future resolving to a `claude_code_rs::Result<Outcome>`.
/// Defaults to `claude_code_rs::execute`; tests substitute a stub via
/// [`ClaudeCodeStep::with_transport`] so the gated suite never spawns a real
/// subprocess.
type Transport = Arc<
    dyn Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> + Send + Sync,
>;

/// The tier/model/endpoint a transport actually called for one invocation —
/// stamped onto `ctx.nodes[name]["transport"]` (`EN.5.D` task 9) so
/// downstream telemetry harvesting reads what happened rather than what the
/// resolved policy *intended* (the distinction that matters for
/// `openai_compat_transport`'s silent cloud fallback: the resolved policy
/// may say `local` while the call that actually ran was `cloud`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportInfo {
    /// The model tier actually called, e.g. `"cloud"` or `"local"`.
    pub tier: String,
    /// The model name actually called.
    pub model: String,
    /// The HTTP endpoint actually called, when the transport is an
    /// HTTP-based one (e.g. `openai_compat_transport`'s local endpoint).
    /// `None` for subprocess-based (cloud CLI) calls, and for any cloud
    /// fallback — there is no single "endpoint" for the `claude` CLI.
    pub endpoint: Option<String>,
}

/// The injectable transport signature for transports that know their own
/// [`TransportInfo`] (e.g. `openai_compat_transport`, which alone knows
/// whether a given call actually hit the local endpoint or silently fell
/// back to cloud). Additive alongside [`Transport`]: a `ClaudeCodeStep`
/// with no [`MetaTransport`] set falls back to a generic `"cloud"`-tier
/// `TransportInfo` derived from the plain `Transport`'s `Outcome`, so every
/// existing `with_transport` caller keeps compiling and behaving unchanged.
pub type MetaTransport = Arc<
    dyn Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<(Outcome, TransportInfo)>>
        + Send
        + Sync,
>;

/// Where a `ClaudeCodeStep`'s prompt text comes from: either a fixed string
/// decided at construction, or a closure built fresh from the live
/// `TaskContext` on each `process` call.
#[derive(Clone)]
enum PromptSource {
    Fixed(String),
    Builder(Arc<dyn Fn(&TaskContext) -> String + Send + Sync>),
}

impl fmt::Debug for PromptSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromptSource::Fixed(prompt) => f.debug_tuple("Fixed").field(prompt).finish(),
            PromptSource::Builder(_) => f.debug_tuple("Builder").field(&"<closure>").finish(),
        }
    }
}

/// A `Node` that runs a single Claude Code session (via `core/claude-code-rs`)
/// and maps its result into `TaskContext`.
///
/// Constructed with an instance name (its `Node::name()` identity — Phase 3
/// registers multiple instances under distinct identities for
/// implement/test/triage/review), a `claude_code_rs::Config`, and a prompt
/// source (fixed string or a builder closure over `&TaskContext`).
#[derive(Clone)]
pub struct ClaudeCodeStep {
    name: String,
    config: Config,
    prompt: PromptSource,
    transport: Transport,
    meta_transport: Option<MetaTransport>,
    cancellation_token: Option<CancellationToken>,
    /// Bounded retry-with-backoff budget for the transport call
    /// (`ticket-implement-node-transport-retry`). Defaults to
    /// [`TransportRetry::default`] — behavior-stable on the success path
    /// (see that type's docs), so every existing caller keeps compiling and
    /// behaving unchanged unless it opts into a different budget via
    /// [`ClaudeCodeStep::with_retry_policy`].
    retry: TransportRetry,
}

impl fmt::Debug for ClaudeCodeStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCodeStep")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("prompt", &self.prompt)
            .finish_non_exhaustive()
    }
}

/// The default transport: delegates straight to `claude_code_rs::execute`,
/// owning its own clones of `config`/`prompt` so the returned future is
/// `'static`.
fn default_transport(
    config: Config,
    prompt: String,
) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> {
    Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
}

impl ClaudeCodeStep {
    /// Construct a step with a fixed prompt string.
    pub fn new(name: impl Into<String>, config: Config, prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config,
            prompt: PromptSource::Fixed(prompt.into()),
            transport: Arc::new(default_transport),
            meta_transport: None,
            cancellation_token: None,
            retry: TransportRetry::default(),
        }
    }

    /// Construct a step whose prompt is built fresh from the live
    /// `TaskContext` on each `process` call.
    pub fn with_prompt_builder(
        name: impl Into<String>,
        config: Config,
        builder: impl Fn(&TaskContext) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            config,
            prompt: PromptSource::Builder(Arc::new(builder)),
            transport: Arc::new(default_transport),
            meta_transport: None,
            cancellation_token: None,
            retry: TransportRetry::default(),
        }
    }

    /// Override the transport used to run the Claude Code session. Tests use
    /// this to stub a real subprocess call with a canned `Outcome`/`Error`, so
    /// the gated `cargo test` suite stays hermetic (no real `claude` spawn).
    ///
    /// A plain `Transport` doesn't know its own tier/endpoint, so `process`
    /// stamps a generic `"cloud"`-tier `TransportInfo` (model derived from
    /// the returned `Outcome`, no endpoint) unless [`with_meta_transport`]
    /// is also set.
    ///
    /// [`with_meta_transport`]: Self::with_meta_transport
    #[must_use]
    pub fn with_transport(
        mut self,
        transport: impl Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.transport = Arc::new(transport);
        self
    }

    /// Override the transport with one that reports its own [`TransportInfo`]
    /// (tier/model/endpoint actually called) alongside its `Outcome` — the
    /// seam `openai_compat_transport` uses so a silent cloud fallback stamps
    /// `"cloud"` even though the resolved policy said `"local"`. Takes
    /// precedence over [`with_transport`] when both are set.
    ///
    /// [`with_transport`]: Self::with_transport
    #[must_use]
    pub fn with_meta_transport(
        mut self,
        meta_transport: impl Fn(
                Config,
                String,
            ) -> BoxFuture<'static, claude_code_rs::Result<(Outcome, TransportInfo)>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.meta_transport = Some(Arc::new(meta_transport));
        self
    }

    /// Attach a `CancellationToken` (EN.2.B task 1) that is raced against the
    /// awaited transport future in `process` (task 4). Absent a token,
    /// `process` behaves exactly as before — the transport future is simply
    /// awaited to completion.
    ///
    /// A cancel win drops the in-flight transport future (per D4 the SDK owns
    /// kill-on-drop — this is not subprocess signalling) and returns `Ok(ctx)`
    /// with the context untouched. `process` never stamps its own `NodeRun`
    /// (that envelope is framework-owned, see `crate::node::Node`), so this
    /// deliberately does *not* return a `NodeError` for a cancel: task 3's
    /// `Workflow::run_with` re-checks the token at the very next node
    /// boundary and stamps the run cancelled there — an `Err` here would
    /// instead be read as FAILED by `workflow::node_context`.
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Override the bounded transport retry-with-backoff budget (defaults to
    /// [`TransportRetry::default`]). `SdlcPolicy::transport_retry`
    /// (`policy.rs`) is the resolved four-layer value a caller would
    /// normally hand in here once wired at the call site.
    #[must_use]
    pub fn with_retry_policy(mut self, retry: TransportRetry) -> Self {
        self.retry = retry;
        self
    }
}

#[async_trait::async_trait]
impl Node for ClaudeCodeStep {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let prompt = match &self.prompt {
            PromptSource::Fixed(prompt) => prompt.clone(),
            PromptSource::Builder(builder) => builder(&ctx),
        };

        let (outcome, transport_info) = match &self.meta_transport {
            Some(meta_transport) => {
                let meta_transport = Arc::clone(meta_transport);
                let config = self.config.clone();
                let call_result = call_with_retry(
                    move || meta_transport(config.clone(), prompt.clone()),
                    self.retry,
                    self.cancellation_token.as_ref(),
                )
                .await;

                match call_result {
                    // A cancel win returns `Ok(ctx)` rather than `Err` — see
                    // `with_cancellation_token`'s doc comment. `call_with_retry`
                    // never retries past a cancellation.
                    RetriedCall::Cancelled => return Ok(ctx),
                    RetriedCall::Err(err) => return Err(err),
                    RetriedCall::Ok(value) => value,
                }
            }
            None => {
                let transport = Arc::clone(&self.transport);
                let config = self.config.clone();
                let call_result = call_with_retry(
                    move || transport(config.clone(), prompt.clone()),
                    self.retry,
                    self.cancellation_token.as_ref(),
                )
                .await;

                let outcome = match call_result {
                    // A cancel win drops the in-flight future (`tokio::select!`
                    // drops the losing branch) rather than awaiting it to
                    // completion, and is never retried. Returning `Ok(ctx)`
                    // unchanged here — not `Err` — is deliberate: see
                    // `with_cancellation_token`'s doc comment.
                    RetriedCall::Cancelled => return Ok(ctx),
                    RetriedCall::Err(err) => return Err(err),
                    RetriedCall::Ok(value) => value,
                };

                // A plain `Transport` doesn't know its own tier/endpoint — stamp a
                // generic `"cloud"`-tier `TransportInfo` derived from the
                // `Outcome`'s own model attribution, with no endpoint.
                let model = outcome.primary_model().unwrap_or(UNKNOWN_MODEL).to_string();
                let info = TransportInfo {
                    tier: "cloud".to_string(),
                    model,
                    endpoint: None,
                };
                (outcome, info)
            }
        };

        // `primary_model` is a heuristic over the SDK's `modelUsage` map and is
        // `None` when no model ran. `engine_contract::Usage::model` is a required
        // `String` (the orchestrator data contract's shape, v1.0.1) — so the
        // fallback belongs here, at the seam, rather than in the contract type.
        let model = outcome.primary_model().unwrap_or(UNKNOWN_MODEL).to_string();

        let output = serde_json::json!({
            "content": outcome.text,
            "cost_usd": outcome.cost_usd,
            "model": model,
            "structured": outcome.structured_output,
            "transport": {
                "tier": transport_info.tier,
                "model": transport_info.model,
                "endpoint": transport_info.endpoint,
            },
            // Additive, non-contract channels (`EN.ticket.token-usage-drops-cache-channels`
            // task 1): `engine_contract::Usage` only carries uncached input/output
            // (orchestrator data contract v1.0.1 §6), so the cache channels are
            // stamped here rather than widening that contract type. Cache reads
            // bill at 10% and cache creation at 125% of uncached input, so these
            // two fields are most of the real spend on a warm run.
            "cache_read_input_tokens": outcome.usage.cache_read_input_tokens,
            "cache_creation_input_tokens": outcome.usage.cache_creation_input_tokens,
        });
        ctx.nodes.insert(self.name.clone(), output);

        let usage = Usage {
            input_tokens: Some(outcome.usage.input_tokens),
            output_tokens: Some(outcome.usage.output_tokens),
            model,
        };
        ctx.node_runs
            .entry(self.name.clone())
            .and_modify(|run| run.usage = Some(usage.clone()))
            .or_insert_with(|| NodeRun {
                status: NodeRunStatus::Running,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: Some(usage),
            });

        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn stub_outcome() -> Outcome {
        stub_outcome_with_models(&[("claude-sonnet-4-5", 0.01, 34)])
    }

    /// Build an `Outcome` whose `modelUsage` carries the given
    /// `(name, cost_usd, output_tokens)` entries.
    fn stub_outcome_with_models(models: &[(&str, f64, u64)]) -> Outcome {
        Outcome {
            cost_usd: 0.01,
            usage: SdkUsage {
                input_tokens: 12,
                output_tokens: 34,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: models
                .iter()
                .map(|(name, cost_usd, output_tokens)| {
                    (
                        (*name).to_string(),
                        SdkModelUsage {
                            input_tokens: 12,
                            output_tokens: *output_tokens,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: *cost_usd,
                        },
                    )
                })
                .collect(),
            text: "ok".to_string(),
            is_error: false,
            api_error_status: None,
            structured_output: None,
        }
    }

    #[tokio::test]
    async fn success_maps_output_and_usage() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| Box::pin(async { Ok(stub_outcome()) }));

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx
            .nodes
            .get("ClaudeCodeStep")
            .expect("output present under node identity");
        assert_eq!(output["content"], "ok");
        assert_eq!(output["model"], "claude-sonnet-4-5");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry present");
        let usage = run.usage.as_ref().expect("usage stamped");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));
        assert_eq!(usage.model, "claude-sonnet-4-5");
    }

    /// The SDK reports no model when `modelUsage` is empty. The contract's
    /// `usage.model` is a required `String`, so the seam must supply a stand-in
    /// rather than panic or drop the `NodeRun`.
    #[tokio::test]
    async fn absent_model_usage_falls_back_to_unknown_model() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async { Ok(stub_outcome_with_models(&[])) })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("an empty modelUsage must not fail the node");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry");
        assert_eq!(run.usage.as_ref().expect("usage stamped").model, "unknown");
        assert_eq!(ctx.nodes["ClaudeCodeStep"]["model"], "unknown");
    }

    /// A single call can bill several models. Attribution follows the SDK's
    /// cost-ranked heuristic, so the expensive model that did the work wins over
    /// a chatty cheap one.
    #[tokio::test]
    async fn multi_model_usage_attributes_to_the_primary_model() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async {
                    Ok(stub_outcome_with_models(&[
                        ("claude-haiku-4-5", 0.001, 900),
                        ("claude-opus-4-8", 0.42, 12),
                    ]))
                })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry");
        assert_eq!(
            run.usage.as_ref().expect("usage stamped").model,
            "claude-opus-4-8"
        );
    }

    /// A plain `Transport` (no `with_meta_transport`) doesn't know its own
    /// tier/endpoint — `process` must stamp a generic `"cloud"`-tier
    /// `TransportInfo` derived from the `Outcome`'s own model attribution,
    /// with no endpoint, so existing callers get the new `transport`
    /// sub-object for free.
    #[tokio::test]
    async fn plain_transport_stamps_generic_cloud_tier_transport_info() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| Box::pin(async { Ok(stub_outcome()) }));

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx.nodes.get("ClaudeCodeStep").expect("output present");
        assert_eq!(output["transport"]["tier"], "cloud");
        assert_eq!(output["transport"]["model"], "claude-sonnet-4-5");
        assert!(output["transport"]["endpoint"].is_null());
    }

    /// `with_meta_transport` takes precedence over `with_transport`: the
    /// `TransportInfo` it returns is what gets stamped, not the generic
    /// `"cloud"` fallback.
    #[tokio::test]
    async fn meta_transport_info_is_stamped_onto_the_transport_sub_object() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_meta_transport(|_config, _prompt| {
                Box::pin(async {
                    Ok((
                        stub_outcome(),
                        TransportInfo {
                            tier: "local".to_string(),
                            model: "qwen2.5-coder:7b".to_string(),
                            endpoint: Some("http://localhost:11434".to_string()),
                        },
                    ))
                })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx.nodes.get("ClaudeCodeStep").expect("output present");
        assert_eq!(output["transport"]["tier"], "local");
        assert_eq!(output["transport"]["model"], "qwen2.5-coder:7b");
        assert_eq!(output["transport"]["endpoint"], "http://localhost:11434");
        // Existing readers of `content`/`model` are unaffected by which
        // transport variant supplied the outcome.
        assert_eq!(output["content"], "ok");
        assert_eq!(output["model"], "claude-sonnet-4-5");
    }

    /// `EN.ticket.token-usage-drops-cache-channels` task 1: a transport that
    /// reports nonzero cache channels must have both stamped into
    /// `ctx.nodes[<node>]`, distinct from each other and from the uncached
    /// input/output already stamped there.
    #[tokio::test]
    async fn nonzero_cache_channels_are_stamped_into_ctx_nodes() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async {
                    let mut outcome = stub_outcome();
                    outcome.usage.cache_read_input_tokens = 500;
                    outcome.usage.cache_creation_input_tokens = 77;
                    Ok(outcome)
                })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx.nodes.get("ClaudeCodeStep").expect("output present");
        assert_eq!(output["cache_read_input_tokens"], 500);
        assert_eq!(output["cache_creation_input_tokens"], 77);
    }

    /// A transport reporting no cache usage must yield `0`, never `null` and
    /// never a panic.
    #[tokio::test]
    async fn zero_cache_channels_stamp_as_zero_not_null() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| Box::pin(async { Ok(stub_outcome()) }));

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let output = ctx.nodes.get("ClaudeCodeStep").expect("output present");
        assert_eq!(output["cache_read_input_tokens"], 0);
        assert_eq!(output["cache_creation_input_tokens"], 0);
        assert!(!output["cache_read_input_tokens"].is_null());
        assert!(!output["cache_creation_input_tokens"].is_null());
    }

    /// `node_runs`' uncached input/output must remain unaffected by the new
    /// cache stamping — pinning the existing telemetry-facing shape.
    #[tokio::test]
    async fn node_runs_uncached_usage_unchanged_by_cache_stamping() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async {
                    let mut outcome = stub_outcome();
                    outcome.usage.cache_read_input_tokens = 500;
                    outcome.usage.cache_creation_input_tokens = 77;
                    Ok(outcome)
                })
            });

        let ctx = step
            .process(empty_context())
            .await
            .expect("process should succeed");

        let run = ctx
            .node_runs
            .get("ClaudeCodeStep")
            .expect("node_runs entry present");
        let usage = run.usage.as_ref().expect("usage stamped");
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(34));
    }

    #[tokio::test]
    async fn sdk_error_maps_to_node_error() {
        let step = ClaudeCodeStep::new("ClaudeCodeStep", Config::default(), "do the thing")
            .with_transport(|_config, _prompt| {
                Box::pin(async { Err(claude_code_rs::Error::Timeout) })
            });

        let err = step
            .process(empty_context())
            .await
            .expect_err("process should surface the SDK error");

        assert_eq!(err.message, claude_code_rs::Error::Timeout.to_string());
    }

    #[tokio::test]
    async fn prompt_builder_receives_live_context() {
        let step = ClaudeCodeStep::with_prompt_builder(
            "ClaudeCodeStep",
            Config::default(),
            |ctx: &TaskContext| format!("event was: {}", ctx.event),
        )
        .with_transport(|_config, prompt| {
            Box::pin(async move {
                let mut outcome = stub_outcome();
                outcome.text = prompt;
                Ok(outcome)
            })
        });

        let mut ctx = empty_context();
        ctx.event = serde_json::json!({"ticket_id": "T-1"});

        let out = step.process(ctx).await.expect("process should succeed");

        let output = out.nodes.get("ClaudeCodeStep").expect("output present");
        assert!(output["content"]
            .as_str()
            .unwrap()
            .contains("\"ticket_id\":\"T-1\""));
    }
}
