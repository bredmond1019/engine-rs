//! A cheaply-cloneable cancellation token for the run loop (EN.2.B task 1).
//!
//! `Workflow::run` (EN.2.B task 3) checks a `CancellationToken` at each node
//! boundary before dispatching the next node; `ClaudeCodeStep` (EN.2.B task 4)
//! races it against an in-flight session future so a cancel drops the future
//! rather than awaiting it to completion.
//!
//! **Cancelled is not a `NodeRunStatus` variant.** Contract §6 fixes
//! `NodeRunStatus` to exactly `pending|running|success|failed`, and §8 makes
//! adding a status value a MAJOR bump — this block ships a MINOR (v1.0.1 →
//! v1.1.0). Instead, a cancelled run is recorded in `TaskContext::metadata`
//! (free-form JSON per §5) via [`stamp_cancelled`]. See
//! `planning/decisions/D6-cancellation-and-budget-semantics.md`.

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::watch;

/// The `TaskContext::metadata` key under which a cancelled run's terminal
/// marker is recorded — see [`stamp_cancelled`].
pub const CANCELLATION_METADATA_KEY: &str = "cancellation";

/// A cheaply-cloneable, `Send + Sync` flag that can be triggered once and
/// observed from any number of clones — including from an awaited future via
/// [`CancellationToken::cancelled`].
///
/// Backed by a `tokio::sync::watch` channel rather than a bare `AtomicBool` +
/// `Notify` pair: `watch::Receiver::changed` compares against the last-seen
/// value internally, so checking [`CancellationToken::is_cancelled`] and then
/// subscribing before awaiting is race-free — a `cancel()` that lands between
/// the check and the subscribe is still visible as the channel's *current*
/// value once the receiver is created.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Creates a fresh, not-yet-cancelled token.
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    /// Triggers cancellation. Observable through every clone of this token,
    /// including ones made before this call. Idempotent.
    pub fn cancel(&self) {
        // `Sender::send` no-ops (and returns `Err`) when there are zero live
        // receivers — which is the common case here, since `new()` doesn't
        // keep its initial receiver alive and callers may cancel before any
        // `cancelled()` waiter has subscribed. `send_replace` updates the
        // retained value unconditionally, so `is_cancelled()`/`borrow()`
        // observe the flip regardless of whether anyone is subscribed yet.
        let _ = self.tx.send_replace(true);
    }

    /// Returns `true` once [`CancellationToken::cancel`] has been called on
    /// this token or any of its clones.
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves once the token is cancelled. Resolves immediately if it is
    /// already cancelled. Intended for `tokio::select!`/`futures::select`
    /// races against an in-flight future (see `nodes::claude_code_step`).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without ever cancelling — treat as "never
                // cancelled" and return so callers don't hang forever.
                return;
            }
        }
    }
}

/// Stamps `metadata` with the cancelled terminal-state marker:
/// `{ "cancellation": { "cancelled": true, "at": <iso8601> } }`.
///
/// If `metadata` is not already a JSON object, it is replaced with one first
/// (matching `TaskContext::metadata`'s default shape of `{}`). This is the
/// canonical way to spell "this run was cancelled" per D6 — never a
/// `NodeRunStatus::Cancelled` variant.
pub fn stamp_cancelled(metadata: &mut serde_json::Value) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    let at = Utc::now().to_rfc3339();
    metadata[CANCELLATION_METADATA_KEY] = serde_json::json!({
        "cancelled": true,
        "at": at,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::TaskContext;
    use std::collections::HashMap;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observable_through_a_clone() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());

        token.cancel();

        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let token = CancellationToken::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_resolves_once_cancelled() {
        let token = CancellationToken::new();
        let waiter = token.clone();

        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
        });

        // Give the spawned task a chance to start waiting before cancelling.
        tokio::task::yield_now().await;
        token.cancel();

        // If `cancelled()` never resolves, this await hangs and the test
        // times out — that's the failure mode we're guarding against.
        handle.await.expect("waiter task panicked");
    }

    #[tokio::test]
    async fn cancelled_resolves_immediately_when_already_cancelled() {
        let token = CancellationToken::new();
        token.cancel();

        // Must not hang.
        token.cancelled().await;
    }

    #[test]
    fn stamp_cancelled_sets_the_metadata_shape() {
        let mut metadata = serde_json::json!({});
        stamp_cancelled(&mut metadata);

        let cancellation = &metadata[CANCELLATION_METADATA_KEY];
        assert_eq!(cancellation["cancelled"], serde_json::json!(true));
        assert!(cancellation["at"].is_string());
    }

    #[test]
    fn stamp_cancelled_preserves_existing_metadata_keys() {
        let mut metadata = serde_json::json!({ "workflow": "sdlc-flow" });
        stamp_cancelled(&mut metadata);

        assert_eq!(metadata["workflow"], serde_json::json!("sdlc-flow"));
        assert_eq!(
            metadata[CANCELLATION_METADATA_KEY]["cancelled"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn stamp_cancelled_replaces_non_object_metadata() {
        let mut metadata = serde_json::Value::Null;
        stamp_cancelled(&mut metadata);

        assert!(metadata.is_object());
        assert_eq!(
            metadata[CANCELLATION_METADATA_KEY]["cancelled"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn metadata_helper_shape_survives_task_context_round_trip() {
        let mut metadata = serde_json::json!({});
        stamp_cancelled(&mut metadata);

        let tc = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata,
            node_runs: HashMap::new(),
        };

        let v = serde_json::to_value(&tc).unwrap();
        let round_tripped: TaskContext = serde_json::from_value(v).unwrap();

        assert_eq!(
            round_tripped.metadata[CANCELLATION_METADATA_KEY]["cancelled"],
            serde_json::json!(true)
        );
        assert!(round_tripped.metadata[CANCELLATION_METADATA_KEY]["at"].is_string());
    }
}
