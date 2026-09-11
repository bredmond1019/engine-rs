//! `JudgmentNode` (`EN.17.D` task 1) — a reusable, bounded, schema-constrained
//! `claude` call: byte-capped input slices, a model tier, and a turn ceiling,
//! returning a typed [`JudgmentError`] for every way the call can fail.
//!
//! Follows [`crate::workflows::claim_reaffirm::judge::JudgeClaimNode`]'s
//! shape: a composed [`AgentCodeStep`] with `Config.json_schema` set, the
//! tier applied via [`crate::policy::apply_model_tier`], and a transport seam
//! ([`TransportSlot`]) so the gated suite never spawns a real subprocess. It
//! adds three things that node does not need: per-slice byte-capped
//! truncation at a UTF-8 boundary (with a marker naming the slice and the
//! bytes dropped), `Config.max_turns` from the spec, and a typed failure
//! enum covering every way the call can come back without a usable verdict.
//!
//! [`JudgmentNode::judge`] takes `&TaskContext` and returns its result to the
//! caller rather than writing it onto `ctx.nodes` itself, because a repeat
//! call from the same caller (e.g. preflight judging several claims) would
//! overwrite the one node-result slot a `ctx.nodes` identity holds
//! (carryover `ctx-nodes-holds-one-slot-per-node-so-repeat-invocations-overwrite-history`).
//! It runs the composed step against a clone of the caller's context, so the
//! caller's own `ctx.nodes` key set is unchanged by a call — asserted by
//! `judgment_writes_no_ctx_slot`.
//!
//! `JudgeClaimNode` itself is NOT migrated onto this node (out of scope for
//! this block).

use serde::de::DeserializeOwned;
use serde_json::Value;

use claude_code_rs::Config;
use engine_contract::TaskContext;

use crate::node::Node;
use crate::nodes::agent_code_step::{AgentCodeStep, MetaTransport};
use crate::policy::{LocalConfig, ModelTier};
use crate::sessions::ClaudeSession;
use crate::workflows::sdlc_flow::policy::TransportRetry;
use crate::workflows::{
    parse_structured_or_fenced, session_baseline, sessions_since, ModelTransport, TransportSlot,
};

/// `JudgmentNode` makes exactly one bounded transport attempt per call — no
/// retry-with-backoff. A judgment call's cost/session accounting (e.g. "an
/// `is_error` envelope gives `CliError` with ONE session") assumes exactly
/// one attempt; a retrying call would multiply both the bill and the
/// session count for what is meant to be a single cheap, bounded check.
const NO_RETRY: TransportRetry = TransportRetry {
    max_attempts: 1,
    initial_backoff_ms: 0,
};

/// One byte-capped excerpt of a block record (or other judged text) fed into
/// the prompt as its own named section.
#[derive(Debug, Clone)]
pub struct InputSlice {
    /// The slice's name — echoed in its prompt section header, its
    /// truncation marker (if truncated), and [`JudgmentResult::truncated_slices`]
    /// / [`JudgmentResult::input_bytes_by_slice`].
    pub name: String,
    /// The slice's raw text, before any truncation.
    pub text: String,
    /// The maximum number of bytes of `text` allowed to reach the transport.
    /// A UTF-8-safe cap: a slice longer than this is cut at the nearest char
    /// boundary at or below `max_bytes`, never mid-codepoint.
    pub max_bytes: usize,
}

/// One `JudgmentNode::judge` call's full specification: the model call's
/// identity, its structured-output schema, its stable prompt prefix, the
/// byte-capped slices to judge, the model tier, and an optional turn
/// ceiling.
#[derive(Debug, Clone)]
pub struct JudgmentSpec {
    /// The `Node::name()`-style identity the composed `AgentCodeStep` runs
    /// under — also the `ctx.nodes` key its (discarded, per-call) stamp is
    /// written to on the internal clone.
    pub identity: String,
    /// The JSON schema `Config.json_schema` is set to, constraining the
    /// model's structured reply.
    pub json_schema: Value,
    /// The stable, run-invariant prompt text — a whole task prompt (D24: a
    /// file loaded via `include_str!`, per this spec's own consumer), not a
    /// cache-anchor prefix.
    pub stable_prompt: &'static str,
    /// The byte-capped input slices appended to the prompt body, each under
    /// its own named section.
    pub slices: Vec<InputSlice>,
    /// The resolved model tier for this call.
    pub tier: ModelTier,
    /// `Config.max_turns` — the turn ceiling. `None` leaves the CLI's own
    /// default.
    pub max_turns: Option<u32>,
}

/// A successful `judge` call's result: the caller's typed verdict plus the
/// call's own bookkeeping (resolved tier/turn ceiling, and per-slice
/// truncation accounting) so a caller can stamp both onto its own report
/// without re-deriving them.
#[derive(Debug, Clone)]
pub struct JudgmentResult<T> {
    /// The model's reply, deserialized into the caller's schema type.
    pub verdict: T,
    /// The tier this call actually ran at (echoes `JudgmentSpec::tier`).
    pub tier: ModelTier,
    /// The turn ceiling this call actually ran with (echoes
    /// `JudgmentSpec::max_turns`).
    pub max_turns: Option<u32>,
    /// `(slice name, bytes actually sent)` for every slice, in the order
    /// they were given — the byte count after truncation, never more than
    /// that slice's `max_bytes`.
    pub input_bytes_by_slice: Vec<(String, usize)>,
    /// The names of every slice that was truncated (a subset of the names in
    /// `input_bytes_by_slice`). Empty when every slice fit within its cap.
    pub truncated_slices: Vec<String>,
}

/// Every way a [`JudgmentNode::judge`] call can fail to produce a usable
/// verdict. Every variant that follows a billed call carries that call's
/// sessions (`Vec<ClaudeSession>`) so a caller surfacing this as a
/// [`crate::node::NodeError`] can attach them via `NodeError::with_sessions`
/// — never an eighteenth place a billed session is silently dropped
/// (carryover `seventeen-wrapper-files-still-drop-billed-sessions-on-failure`).
#[derive(Debug)]
pub enum JudgmentError {
    /// The call exceeded its configured timeout
    /// (`claude_code_rs::Error::Timeout`). No envelope was ever produced, so
    /// there is no session to carry.
    Timeout,
    /// `claude-code-rs` returned an error that carries an envelope (an
    /// `is_error: true` reply, or any other transport failure short of a
    /// timeout) — the CLI ran (or attempted to) but did not hand back a
    /// usable outcome.
    CliError {
        /// Sessions billed before this failure was detected.
        sessions: Vec<ClaudeSession>,
        /// The underlying transport error's message.
        message: String,
    },
    /// The call succeeded but its reply carried no structured payload, and
    /// its raw content is not JSON either. This is the expected shape of a
    /// call that ran out of turns before producing a final answer:
    /// `claude-code-rs`'s `Outcome` has no `subtype` field and classifies
    /// only by `is_error`, so turns exhaustion cannot be told apart from any
    /// other unstructured reply from this crate alone.
    NoStructuredResult {
        /// Sessions billed by the call that produced this reply.
        sessions: Vec<ClaudeSession>,
        /// The turn ceiling this call ran with, echoed for a caller that
        /// wants to report it alongside the failure.
        max_turns: Option<u32>,
    },
    /// JSON was present (either a `structured` payload or a bare/fenced JSON
    /// reply) but does not deserialize into the caller's schema type `T` —
    /// including an out-of-enum value.
    SchemaViolation {
        /// Sessions billed by the call that produced this reply.
        sessions: Vec<ClaudeSession>,
        /// The `serde_json` deserialization error.
        error: serde_json::Error,
    },
}

/// Truncate `text` to at most `max_bytes`, at the nearest UTF-8 char
/// boundary at or below that cap — never mid-codepoint. Returns the
/// (possibly truncated) text plus `true` when truncation occurred; a slice
/// already within its cap comes back byte-identical with `false`.
fn truncate_to_max_bytes(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (&text[..boundary], true)
}

/// Build the full prompt body (`stable_prompt` followed by each slice, in
/// order, under its own named section) plus the per-slice byte/truncation
/// bookkeeping [`JudgmentResult`] carries.
fn build_prompt(
    stable_prompt: &str,
    slices: &[InputSlice],
) -> (String, Vec<(String, usize)>, Vec<String>) {
    let mut prompt = String::from(stable_prompt);
    let mut input_bytes_by_slice = Vec::with_capacity(slices.len());
    let mut truncated_slices = Vec::new();

    for slice in slices {
        let (sent, truncated) = truncate_to_max_bytes(&slice.text, slice.max_bytes);
        input_bytes_by_slice.push((slice.name.clone(), sent.len()));
        prompt.push_str(&format!("\n\n=== {} ===\n{sent}", slice.name));
        if truncated {
            let dropped = slice.text.len() - sent.len();
            prompt.push_str(&format!(
                "\n[TRUNCATED: slice \"{}\" dropped {dropped} bytes]",
                slice.name
            ));
            truncated_slices.push(slice.name.clone());
        }
    }

    (prompt, input_bytes_by_slice, truncated_slices)
}

/// A bounded, schema-constrained `claude` call over byte-capped input
/// slices. See the module doc comment for the full shape and rationale.
pub struct JudgmentNode<T> {
    transport: TransportSlot,
    _marker: std::marker::PhantomData<T>,
}

impl<T: DeserializeOwned> JudgmentNode<T> {
    /// Construct a `JudgmentNode` with the live transport (a real `claude`
    /// subprocess call via `claude_code_rs::execute`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: TransportSlot::default(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Override the transport used by the composed `AgentCodeStep`. Tests
    /// inject a stub so the gated suite never spawns a real subprocess.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware `MetaTransport`. Takes
    /// precedence over [`Self::with_transport`] when both are set.
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }

    /// Run one bounded, schema-constrained `claude` call per `spec`,
    /// returning the caller's typed verdict or a typed [`JudgmentError`].
    ///
    /// Runs against a clone of `ctx` — the caller's own `ctx.nodes` is never
    /// written to (see the module doc comment for why).
    pub async fn judge(
        &self,
        ctx: &TaskContext,
        spec: JudgmentSpec,
    ) -> Result<JudgmentResult<T>, JudgmentError> {
        let (prompt, input_bytes_by_slice, truncated_slices) =
            build_prompt(spec.stable_prompt, &spec.slices);

        let local_model = LocalConfig::default().model;
        let mut config = Config {
            json_schema: Some(spec.json_schema.clone()),
            max_turns: spec.max_turns,
            ..Config::default()
        };
        config = crate::policy::apply_model_tier(config, spec.tier, &local_model);

        let step = self
            .transport
            .apply(AgentCodeStep::new(spec.identity.clone(), config, prompt))
            .with_retry_policy(NO_RETRY);

        let call_ctx = ctx.clone();
        let baseline = session_baseline(&call_ctx);

        let result_ctx = match step.process(call_ctx).await {
            Ok(result_ctx) => result_ctx,
            Err(err) => {
                // `claude_code_rs::Error::Timeout` never carries an envelope
                // (`billed_failure` in `agent_code_step.rs` records no
                // session for it, and `AgentCodeStep::process` forwards its
                // `Display` text verbatim as `NodeError::message`), so its
                // exact message string is the one stable signal left once
                // the original error type has been collapsed into a plain
                // `NodeError` by the composed step.
                if err.message == claude_code_rs::Error::Timeout.to_string() {
                    return Err(JudgmentError::Timeout);
                }
                return Err(JudgmentError::CliError {
                    sessions: err.sessions,
                    message: err.message,
                });
            }
        };

        let sessions = sessions_since(&result_ctx, baseline);
        let content = result_ctx
            .nodes
            .get(&spec.identity)
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        let raw: Result<Value, serde_json::Error> =
            parse_structured_or_fenced(&result_ctx, &spec.identity, content);
        let raw = match raw {
            Ok(value) => value,
            Err(_) => {
                return Err(JudgmentError::NoStructuredResult {
                    sessions,
                    max_turns: spec.max_turns,
                });
            }
        };

        let verdict: T = serde_json::from_value(raw)
            .map_err(|error| JudgmentError::SchemaViolation { sessions, error })?;

        Ok(JudgmentResult {
            verdict,
            tier: spec.tier,
            max_turns: spec.max_turns,
            input_bytes_by_slice,
            truncated_slices,
        })
    }
}

impl<T: DeserializeOwned> Default for JudgmentNode<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_within_cap_is_byte_identical() {
        let (sent, truncated) = truncate_to_max_bytes("hello", 10);
        assert_eq!(sent, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_over_cap_cuts_at_char_boundary() {
        // "café" is 5 bytes in UTF-8 (c=1, a=1, f=1, é=2); a 4-byte cap must
        // not land mid-`é`.
        let (sent, truncated) = truncate_to_max_bytes("café", 4);
        assert!(truncated);
        assert!(sent.len() <= 4);
        assert!(std::str::from_utf8(sent.as_bytes()).is_ok());
    }
}
