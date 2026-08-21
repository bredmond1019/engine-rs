//! `engine-core::operator::transport` — the operator-transport abstraction
//! (`EN.12.J` task 1: the five supporting types this module's trait needs).
//!
//! Extracted from `bastion`'s `src/serve/notify/mod.rs` and
//! `src/serve/notify/telegram.rs` so an engine node can send a message
//! outward without `engine-rs` depending on `bastion` (per brain
//! `BA.21.A`'s own record: "bastion depends on engine-rs, never the
//! reverse"). `engine-core` now defines the canonical copies; `bastion`
//! keeps its own until `BA.21.A` ports them over — the two coexisting for
//! that window is expected, not drift, and is called out per-type below.
//!
//! Task 2 adds [`OperatorTransport`] itself — the three-method,
//! `#[async_trait]`, object-safe trait an engine node calls to reach a
//! human. `acknowledge` is default-implemented as a no-op `Ok(())` so a
//! transport with no acknowledgement concept (WhatsApp, and every existing
//! test fake) needs no change to keep compiling.
//!
//! Task 3 adds the object-safety proof and [`NoopTransport`] — a trivial
//! test double, modeled on bastion's own at
//! `src/serve/notify/tests.rs:143`, that implements all three methods and
//! is reachable from other `engine-core` tests (it is `#[cfg(test)] pub`,
//! not private to this module's own `tests` submodule, so
//! `crate::operator::tests` and any future integration suite can build one
//! without redefining it).
//!
//! Every type in this module is a **temporary duplicate**: bastion keeps
//! its own copies of all five supporting types plus its own trait
//! definition until `BA.21.A` ports its four existing impls
//! (`TelegramTransport` and three test doubles at `notify/tests.rs:143`,
//! `notify/tests.rs:257`, and `handlers/notify.rs:300`) over to this one and
//! deletes the bastion-side originals. Until that lands, the two definitions
//! coexisting is expected, not drift — this block (`EN.12.J`) ships the
//! abstraction only; deleting bastion's copies is explicitly out of scope
//! here (see the block record's `out_of_scope`).
//!
//! Provenance (bastion source lines this module's types were copied from):
//!
//! | Type | Bastion source |
//! |---|---|
//! | [`AckHandle`] | `src/serve/notify/mod.rs:39` |
//! | [`MessageHandle`] | `src/serve/notify/mod.rs:48` |
//! | [`OperatorResponse`] | `src/serve/notify/mod.rs:62` |
//! | [`DeliveredMessage`] | `src/serve/notify/mod.rs:90` |
//! | [`UpdateCursor`] | `src/serve/notify/mod.rs:103` |
//! | [`NotifyError`] | `src/serve/notify/mod.rs:118` |
//! | [`OperatorTransport`] | `src/serve/notify/mod.rs:182` |
//! | [`ResponseVerdict`] | `src/serve/notify/telegram.rs:328` (NOT `mod.rs` —
//! |   its fields `gate_id`/`option_key`/`digest`/`decided_at` are
//! |   channel-agnostic; only its home was channel-specific) |
//! | [`NoopTransport`] | `src/serve/notify/tests.rs:143` (test double only —
//! |   bastion keeps its own for its own coverage; this one exists so
//! |   `engine-core`'s own tests have a transport to inject without a
//! |   network dependency) |

use std::fmt;

use super::validate::ValidatedOperatorPayload;
use async_trait::async_trait;
use thiserror::Error;

/// An opaque handle a transport mints when it observes an inbound response,
/// and later consumes to acknowledge that same response
/// (`OperatorTransport::acknowledge`). The encoding is entirely
/// transport-specific (Telegram: the callback query's `id`); callers must
/// not parse it, only round-trip it. A transport with no acknowledgement
/// concept (e.g. WhatsApp) never mints one — its responses carry `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckHandle(pub String);

/// An opaque handle identifying which message an inbound response's
/// original prompt was delivered as, so a later edit (dropping its buttons
/// and showing the chosen option) can target the right message. The
/// encoding is entirely transport-specific (Telegram: `chat_id` +
/// `message_id`); callers must not parse it, only round-trip it. A
/// transport with no such concept, or a response the transport could not
/// resolve a message for, carries `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHandle {
    /// Transport-specific chat identifier the original message lives in.
    pub chat_id: String,
    /// Transport-specific identifier of the original message itself.
    pub message_id: i64,
}

/// A response the operator gave, resolved back to the gate and digest it
/// answers. `option_key` is the stable machine key of the tapped option
/// (`OperatorResponseOption::key`), never the operator-visible label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorResponse {
    /// The gate this response answers.
    pub gate_id: String,
    /// The digest of the payload the operator was shown when they
    /// responded — used to reject a response against a payload that has
    /// since been mutated (stale-digest rejection).
    pub digest: String,
    /// The stable machine key of the option the operator tapped.
    pub option_key: String,
    /// When this transport observed the response.
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Opaque handle to acknowledge this response back to the transport it
    /// arrived on, if that transport has an acknowledgement concept. `None`
    /// for a transport with no such concept, or when the id could not be
    /// captured — losing a real decision because it cannot be acknowledged
    /// is strictly worse than not acknowledging it.
    pub ack: Option<AckHandle>,
    /// Opaque handle to the message the operator responded to, if the
    /// transport can resolve one — used to edit that message and drop its
    /// live buttons once a decision is taken. `None` for a transport with
    /// no such concept, or when the location could not be captured.
    pub message: Option<MessageHandle>,
}

/// Confirmation that `OperatorTransport::send` delivered a payload.
/// Transport-agnostic: a channel-specific message id, if any, belongs to
/// that transport's own impl module, not this shared shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredMessage {
    /// Opaque transport-assigned identifier for the delivered message
    /// (e.g. a Telegram `message_id` rendered as a string). Transports that
    /// have no such id may leave this empty.
    pub transport_message_id: String,
}

/// Opaque position in the inbound update stream, threaded back into the
/// next `OperatorTransport::poll_responses` call so a restart resumes
/// instead of replaying (or dropping) the backlog. The concrete encoding is
/// transport-specific (Telegram: the next `offset`); callers must not parse
/// it, only round-trip it.
#[derive(Clone, PartialEq, Eq)]
pub struct UpdateCursor(pub String);

impl fmt::Debug for UpdateCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The cursor is opaque and transport-specific (Telegram: a plain
        // integer offset), not a credential — but formatted explicitly
        // (rather than derived) so a future transport that encodes
        // something sensitive into the cursor does not get free `Debug`
        // access without a deliberate decision here. Matches bastion's own
        // `impl Debug for UpdateCursor` at `src/serve/notify/mod.rs:221`.
        f.debug_tuple("UpdateCursor").field(&self.0).finish()
    }
}

/// Why an `OperatorTransport` operation failed. Variants split along one
/// axis: whether the caller should retry.
///
/// - `Transport` / `RateLimited` are **retryable** — a transient send/poll
///   failure (connect error, timeout, HTTP 429).
/// - `PayloadRejected` / `Unauthorized` / `Malformed` are **permanent** — a
///   retry with the same inputs cannot succeed.
///
/// No variant's `Display` may interpolate a token or other credential.
/// Constructing an `Unauthorized` or `Transport` variant never takes the
/// credential as a field for exactly this reason.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NotifyError {
    /// A transient transport-level failure (connect error, timeout, DNS
    /// failure). Retryable.
    #[error("operator transport failure: {reason}")]
    Transport {
        /// Human-readable failure reason. Must never contain a credential.
        reason: String,
    },
    /// The transport reported a rate limit; retry after the given delay.
    /// Retryable.
    #[error("operator transport rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait before retrying, per the transport's own hint.
        retry_after_secs: u64,
    },
    /// The payload cannot be sent over this transport (e.g. it exceeds the
    /// transport's confirmed limits). Permanent — resending the same
    /// payload cannot succeed; the caller must re-render it.
    #[error("operator payload rejected by transport: {reason}")]
    PayloadRejected {
        /// Why the payload was rejected. Must never contain a credential.
        reason: String,
    },
    /// The transport rejected the credentials (401/403). Permanent from
    /// this call's perspective — deliberately carries no credential value,
    /// only the fact of the rejection.
    #[error("operator transport unauthorized")]
    Unauthorized,
    /// The transport returned a response this code could not parse (e.g.
    /// not the expected envelope shape). Permanent for this response;
    /// does not imply the whole batch is unusable (skip-and-continue policy
    /// for individual malformed updates).
    #[error("operator transport returned a malformed response: {reason}")]
    Malformed {
        /// What was malformed. Must never contain a credential.
        reason: String,
    },
}

impl NotifyError {
    /// Whether the caller should retry the operation that produced this
    /// error. `true` for transient transport-level failures; `false` for
    /// anything permanent (bad payload, bad credentials, unparseable
    /// response).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            NotifyError::Transport { .. } | NotifyError::RateLimited { .. }
        )
    }
}

/// The outcome of resolving an [`OperatorResponse`] against the payload the
/// caller expects it to answer.
///
/// `Accepted` and `StaleDigest` both carry the digest prefix the operator's
/// tap actually presented and when it was observed — both already exist on
/// the inbound `OperatorResponse` (`digest`, `received_at`) and would
/// otherwise be dropped here rather than threaded through a second channel
/// alongside this enum.
///
/// Moved from bastion's `telegram.rs`, not `mod.rs` — its fields
/// (`gate_id`/`option_key`/`digest`/`decided_at`) are channel-agnostic; only
/// its home was channel-specific, so this is a straightforward relocation
/// rather than a generalization of the type's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseVerdict {
    /// Both the gate id and the digest prefix match: this response applies.
    Accepted {
        /// The gate this response answers.
        gate_id: String,
        /// The stable machine key of the option the operator tapped.
        option_key: String,
        /// The digest prefix the operator's tap presented (`resp.digest`).
        digest: String,
        /// When this response was observed (`resp.received_at`).
        decided_at: chrono::DateTime<chrono::Utc>,
    },
    /// The gate matches but the digest does not — the payload was mutated
    /// (re-rendered) after this response was shown, so it must re-queue
    /// rather than execute. Never conflated with `Accepted`. Carries its
    /// `gate_id` (previously dropped) so a sink can re-queue the right
    /// item, plus the same digest/option/time fields `Accepted` carries so
    /// a full verdict can be built regardless of which arm resolution
    /// landed on.
    StaleDigest {
        /// The gate this response answers.
        gate_id: String,
        /// The stable machine key of the option the operator tapped.
        option_key: String,
        /// The digest prefix the operator's tap presented (`resp.digest`).
        digest: String,
        /// When this response was observed (`resp.received_at`).
        decided_at: chrono::DateTime<chrono::Utc>,
    },
    /// The response answers a different gate than `expected` — not this
    /// payload's response at all.
    UnknownGate,
}

/// The transport seam: deliver a validated operator payload, and long-poll
/// for the operator's response. Implemented once per channel (Telegram
/// first; WhatsApp is meant to be a second `impl` sharing this trait
/// unchanged).
///
/// Object-safe: all three methods take `&self`, return `Result<_,
/// NotifyError>` futures with no generic parameters, and the trait has no
/// associated types or `Self: Sized` bounds — `Box<dyn OperatorTransport>` /
/// `Arc<dyn OperatorTransport>` are both nameable. Marked `#[async_trait]`
/// to keep it object-safe under stable Rust (a native `async fn` in a trait
/// is not, without boxing the returned future by hand).
#[async_trait]
pub trait OperatorTransport: Send + Sync {
    /// Deliver `payload` over this transport. Must reject (via
    /// `NotifyError::PayloadRejected`) anything that would not survive the
    /// narrowest target channel's limits, rather than sending a truncated
    /// or partial rendering.
    async fn send(
        &self,
        payload: &ValidatedOperatorPayload,
    ) -> Result<DeliveredMessage, NotifyError>;

    /// Long-poll for operator responses since `since` (or from the start of
    /// the backlog if `None`). Returns the observed responses and the
    /// cursor to pass on the next call.
    async fn poll_responses(
        &self,
        since: Option<UpdateCursor>,
    ) -> Result<(Vec<OperatorResponse>, Option<UpdateCursor>), NotifyError>;

    /// Acknowledge `response`, whose verdict has already been resolved to
    /// `verdict`, back to the transport it arrived on — so the operator's
    /// tap stops spinning and (where the transport supports it) the
    /// original message is edited to show the decision and drop its live
    /// buttons.
    ///
    /// Default-implemented as a no-op `Ok(())`: a transport with no
    /// acknowledgement concept (WhatsApp, and every existing test fake)
    /// needs no change to keep compiling and behaving correctly. Telegram
    /// overrides this to actually call `answerCallbackQuery` (and, best
    /// effort, `editMessageText`).
    async fn acknowledge(
        &self,
        _response: &OperatorResponse,
        _verdict: &ResponseVerdict,
    ) -> Result<(), NotifyError> {
        Ok(())
    }
}

/// A trivial [`OperatorTransport`] that never fails and never observes any
/// responses — the same shape as bastion's own test double at
/// `src/serve/notify/tests.rs:143`. `send` always succeeds with an empty
/// [`DeliveredMessage`]; `poll_responses` always returns an empty batch and
/// echoes back whatever cursor it was given; `acknowledge` uses the
/// trait's no-op default.
///
/// `#[cfg(test)] pub` (not private, and not nested inside this module's own
/// `tests` submodule) so it is reachable from any other `engine-core` test
/// module — e.g. `crate::operator::tests` — without redefining it.
#[cfg(test)]
pub struct NoopTransport;

#[cfg(test)]
#[async_trait]
impl OperatorTransport for NoopTransport {
    async fn send(
        &self,
        _payload: &ValidatedOperatorPayload,
    ) -> Result<DeliveredMessage, NotifyError> {
        Ok(DeliveredMessage {
            transport_message_id: String::new(),
        })
    }

    async fn poll_responses(
        &self,
        since: Option<UpdateCursor>,
    ) -> Result<(Vec<OperatorResponse>, Option<UpdateCursor>), NotifyError> {
        Ok((Vec::new(), since))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Compile-time evidence that `OperatorTransport` is object-safe: a
    /// `NoopTransport` value can be named behind both `Box<dyn
    /// OperatorTransport>` and `Arc<dyn OperatorTransport>`. This is the
    /// property that makes the seam usable at all (bastion injects the
    /// same `Arc` into both its poll loop and its app state), and it
    /// silently breaks the moment someone adds a generic method or an
    /// associated type to the trait — so it is asserted here, not assumed.
    #[test]
    fn operator_transport_is_object_safe() {
        let _boxed: Box<dyn OperatorTransport> = Box::new(NoopTransport);
        let _arced: Arc<dyn OperatorTransport> = Arc::new(NoopTransport);
    }

    /// `NoopTransport::send` never panics and returns the documented `Ok`
    /// shape (an empty `transport_message_id`), exercised through the
    /// `dyn OperatorTransport` interface exactly as a real caller would use
    /// it.
    #[tokio::test]
    async fn noop_transport_send_returns_documented_ok_shape() {
        let transport: Arc<dyn OperatorTransport> = Arc::new(NoopTransport);
        let payload = crate::operator::OperatorPayload::new(
            "gate-1",
            "diff summary",
            vec![
                crate::operator::OperatorResponseOption::new("approve", "Approve"),
                crate::operator::OperatorResponseOption::new("reject", "Reject"),
            ],
        );
        let validated =
            crate::operator::validate(payload, &crate::operator::OperatorPayloadLimits::default())
                .expect("payload validates");

        let delivered = transport
            .send(&validated)
            .await
            .expect("NoopTransport::send never fails");
        assert_eq!(delivered.transport_message_id, "");
    }

    /// `NoopTransport::poll_responses` never observes any responses and
    /// echoes back whatever cursor it was given, unchanged.
    #[tokio::test]
    async fn noop_transport_poll_responses_is_empty_and_echoes_cursor() {
        let transport: Arc<dyn OperatorTransport> = Arc::new(NoopTransport);
        let cursor = Some(UpdateCursor("17".to_string()));

        let (responses, next_cursor) = transport
            .poll_responses(cursor.clone())
            .await
            .expect("NoopTransport::poll_responses never fails");

        assert!(responses.is_empty());
        assert_eq!(next_cursor, cursor);
    }

    /// `NoopTransport` relies on the trait's default `acknowledge` — a
    /// no-op `Ok(())` — and that default must actually be reachable and
    /// callable through the `dyn` interface.
    #[tokio::test]
    async fn noop_transport_acknowledge_uses_default_noop() {
        let transport: Arc<dyn OperatorTransport> = Arc::new(NoopTransport);
        let response = OperatorResponse {
            gate_id: "gate-1".to_string(),
            digest: "abc123".to_string(),
            option_key: "approve".to_string(),
            received_at: chrono::Utc::now(),
            ack: None,
            message: None,
        };
        let verdict = ResponseVerdict::UnknownGate;

        transport
            .acknowledge(&response, &verdict)
            .await
            .expect("default acknowledge is a no-op Ok(())");
    }
}
