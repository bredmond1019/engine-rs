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
//! Task 1 moves only the supporting types (plus the two opaque handles
//! [`OperatorResponse`] embeds, so its field set matches bastion's exactly).
//! The `OperatorTransport` trait itself and its `NoopTransport` test double
//! land in later tasks of this same block.
//!
//! Provenance (bastion source lines this task's types were copied from):
//!
//! | Type | Bastion source |
//! |---|---|
//! | [`AckHandle`] | `src/serve/notify/mod.rs:39` |
//! | [`MessageHandle`] | `src/serve/notify/mod.rs:48` |
//! | [`OperatorResponse`] | `src/serve/notify/mod.rs:62` |
//! | [`DeliveredMessage`] | `src/serve/notify/mod.rs:90` |
//! | [`UpdateCursor`] | `src/serve/notify/mod.rs:103` |
//! | [`NotifyError`] | `src/serve/notify/mod.rs:118` |
//! | [`ResponseVerdict`] | `src/serve/notify/telegram.rs:328` (NOT `mod.rs` —
//! |   its fields `gate_id`/`option_key`/`digest`/`decided_at` are
//! |   channel-agnostic; only its home was channel-specific) |

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
