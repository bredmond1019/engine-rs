//! The suspension primitive: `PauseSignal` + the `metadata.suspension` marker
//! (EN.6.F task 1).
//!
//! `Workflow::walk` (EN.6.F task 4) checks a [`PauseSignal`] at each node
//! boundary — the operator-pause path — and a [`SuspendNode`](crate::nodes::suspend::SuspendNode)
//! can additionally *request* suspension from inside the walk via
//! [`request_suspension`]. Both origins converge on one marker, written by
//! [`stamp_suspended`] under `metadata["suspension"]`, mirroring how
//! [`crate::cancellation::stamp_cancelled`] writes `metadata["cancellation"]`.
//!
//! **`suspended` is not a `NodeRunStatus` variant.** Per D6, new run outcomes
//! are spelled as `TaskContext::metadata` keys, never as a new
//! `NodeRunStatus` variant — see
//! `planning/decisions/D6-cancellation-and-budget-semantics.md`.
//!
//! **`PauseSignal` is deliberately NOT `CancellationToken`.**
//! `CancellationToken::cancel` is a documented one-way idempotent latch (D6);
//! abort semantics must not silently become resettable. A pause, by
//! contrast, must be clearable — a resumed run gets a *fresh* signal, and a
//! signal that outlived its run must not leak a stuck "paused" state onto a
//! new one.

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::budget::BudgetLedger;

/// The `TaskContext::metadata` key under which a run's suspension marker is
/// recorded — see [`stamp_suspended`] / [`read_suspension`].
pub const SUSPENSION_METADATA_KEY: &str = "suspension";

/// A cheaply-cloneable, `Send + Sync`, **two-way** pause flag observed from
/// any number of clones.
///
/// Backed by a `tokio::sync::watch` channel, matching
/// [`crate::cancellation::CancellationToken`]'s shape — but `clear()` gives
/// it the un-fire the cancellation latch deliberately lacks.
#[derive(Debug, Clone)]
pub struct PauseSignal {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for PauseSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl PauseSignal {
    /// Creates a fresh, cleared (not-paused) signal.
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self { tx: Arc::new(tx) }
    }

    /// Sets the signal to paused. Observable through every clone, including
    /// ones made before this call.
    pub fn pause(&self) {
        let _ = self.tx.send_replace(true);
    }

    /// Clears the signal back to not-paused. Observable through every clone.
    pub fn clear(&self) {
        let _ = self.tx.send_replace(false);
    }

    /// Returns `true` if [`PauseSignal::pause`] has been called on this
    /// signal or any of its clones more recently than [`PauseSignal::clear`].
    pub fn is_paused(&self) -> bool {
        *self.tx.borrow()
    }
}

/// Which of the two suspension origins produced a given marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspendReason {
    /// An operator's `POST /events/{run_id}/pause`.
    OperatorPause,
    /// A workflow-authored `SuspendNode` requested it via
    /// [`request_suspension`].
    SuspendNode,
}

/// Everything the walk knows at the moment it stops — the write side passed
/// to [`stamp_suspended`].
pub struct Suspension<'a> {
    pub resume_at: &'a str,
    pub reason: SuspendReason,
    pub origin_identity: Option<&'a str>,
    pub ledger: &'a BudgetLedger,
}

/// A snapshot of the running budget totals at the moment of suspension —
/// what makes a resume start from the pre-suspend spend rather than zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LedgerSnapshot {
    pub total_tokens: u64,
    pub total_cost_usd: f64,
}

/// Typed read side of the `metadata.suspension` marker, so callers never
/// hand-index the JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuspensionState {
    pub suspended: bool,
    pub resume_at: Option<String>,
    pub reason: Option<SuspendReason>,
    pub origin_identity: Option<String>,
    pub ledger: Option<LedgerSnapshot>,
    #[serde(default)]
    pub resume_count: u32,
    #[serde(default)]
    pub requested: bool,
}

/// Stamps `metadata` with the suspended terminal-state marker:
/// `{ "suspension": { "suspended": true, "at": <iso8601>, "resume_at": ...,
/// "reason": ..., "origin_identity": ..., "ledger": {...},
/// "resume_count": N, "requested": false } }`.
///
/// If `metadata` is not already a JSON object, it is replaced with one first
/// (matching [`crate::cancellation::stamp_cancelled`]'s tolerance and
/// `TaskContext::metadata`'s default shape of `{}`).
///
/// `resume_count` is preserved/carried forward from any existing marker
/// (defaulting to `0` for a first-time suspension) — it only increments via
/// [`stamp_resumed`]. `requested` is reset to `false`: a fresh suspension
/// clears whatever request produced it.
pub fn stamp_suspended(metadata: &mut serde_json::Value, s: Suspension<'_>) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    let existing_resume_count = read_suspension(metadata)
        .map(|st| st.resume_count)
        .unwrap_or(0);
    let at = Utc::now().to_rfc3339();
    metadata[SUSPENSION_METADATA_KEY] = serde_json::json!({
        "suspended": true,
        "at": at,
        "resume_at": s.resume_at,
        "reason": s.reason,
        "origin_identity": s.origin_identity,
        "ledger": LedgerSnapshot {
            total_tokens: s.ledger.total_tokens(),
            total_cost_usd: s.ledger.total_cost_usd(),
        },
        "resume_count": existing_resume_count,
        "requested": false,
    });
}

/// Marks a suspended run as resumed: sets `suspended: false`, clears
/// `requested`, and increments `resume_count`. Does **not** delete the key —
/// a stable key shape across suspended/resumed/never-suspended states is
/// what makes the "round-trips identically" acceptance criterion honest.
///
/// A no-op (still leaves `metadata` a JSON object) if `metadata` carries no
/// suspension marker yet.
pub fn stamp_resumed(metadata: &mut serde_json::Value) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    let resume_count = read_suspension(metadata)
        .map(|s| s.resume_count)
        .unwrap_or(0);
    if let Some(existing) = metadata.get(SUSPENSION_METADATA_KEY).cloned() {
        let mut updated = existing;
        updated["suspended"] = serde_json::json!(false);
        updated["requested"] = serde_json::json!(false);
        updated["resume_count"] = serde_json::json!(resume_count + 1);
        metadata[SUSPENSION_METADATA_KEY] = updated;
    }
}

/// `SuspendNode`'s write: records that suspension has been requested,
/// without itself stopping the walk (a `Node` cannot do that — only
/// `Workflow::walk` can). Tolerant of non-object `metadata`.
pub fn request_suspension(metadata: &mut serde_json::Value) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    if let Some(existing) = metadata.get(SUSPENSION_METADATA_KEY).cloned() {
        let mut updated = existing;
        updated["requested"] = serde_json::json!(true);
        metadata[SUSPENSION_METADATA_KEY] = updated;
    } else {
        metadata[SUSPENSION_METADATA_KEY] = serde_json::json!({
            "suspended": false,
            "at": serde_json::Value::Null,
            "resume_at": serde_json::Value::Null,
            "reason": serde_json::Value::Null,
            "origin_identity": serde_json::Value::Null,
            "ledger": serde_json::Value::Null,
            "resume_count": 0,
            "requested": true,
        });
    }
}

/// Reads whether `metadata`'s suspension marker has `requested: true` set.
/// `false` on absent/malformed metadata.
pub fn suspension_requested(metadata: &serde_json::Value) -> bool {
    metadata
        .get(SUSPENSION_METADATA_KEY)
        .and_then(|v| v.get("requested"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Reads `metadata`'s suspension marker as a typed [`SuspensionState`], or
/// `None` if absent/malformed.
pub fn read_suspension(metadata: &serde_json::Value) -> Option<SuspensionState> {
    metadata
        .get(SUSPENSION_METADATA_KEY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// `true` iff `metadata`'s suspension marker has `suspended: true`. `false`
/// on absent/empty/malformed metadata.
pub fn is_suspended(metadata: &serde_json::Value) -> bool {
    read_suspension(metadata)
        .map(|s| s.suspended)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- PauseSignal --------------------------------------------------

    #[test]
    fn fresh_signal_is_not_paused() {
        let sig = PauseSignal::new();
        assert!(!sig.is_paused());
    }

    #[test]
    fn pause_then_clear_round_trips_and_clones_observe_it() {
        let sig = PauseSignal::new();
        let clone = sig.clone();

        sig.pause();
        assert!(sig.is_paused());
        assert!(clone.is_paused(), "clone must observe the same state");

        sig.clear();
        assert!(!sig.is_paused());
        assert!(!clone.is_paused());
    }

    #[test]
    fn default_is_a_fresh_cleared_signal() {
        let sig = PauseSignal::default();
        assert!(!sig.is_paused());
    }

    // -- stamp_suspended / read_suspension -----------------------------

    fn ledger_with(tokens: u64, cost: f64) -> BudgetLedger {
        let mut ledger = BudgetLedger::new();
        ledger.record(
            Some(&engine_contract::Usage {
                input_tokens: Some(tokens),
                output_tokens: Some(0),
                model: String::new(),
            }),
            Some(cost),
        );
        ledger
    }

    #[test]
    fn stamp_suspended_then_read_suspension_round_trips_every_field() {
        let mut metadata = serde_json::json!({});
        let ledger = ledger_with(1234, 0.56);

        stamp_suspended(
            &mut metadata,
            Suspension {
                resume_at: "ReviewNode",
                reason: SuspendReason::OperatorPause,
                origin_identity: Some("SomeNode"),
                ledger: &ledger,
            },
        );

        let state = read_suspension(&metadata).expect("marker should be present");
        assert!(state.suspended);
        assert_eq!(state.resume_at.as_deref(), Some("ReviewNode"));
        assert_eq!(state.reason, Some(SuspendReason::OperatorPause));
        assert_eq!(state.origin_identity.as_deref(), Some("SomeNode"));
        let snap = state.ledger.expect("ledger snapshot should be present");
        assert_eq!(snap.total_tokens, 1234);
        assert!((snap.total_cost_usd - 0.56).abs() < f64::EPSILON);
        assert_eq!(state.resume_count, 0);
        assert!(!state.requested);
    }

    #[test]
    fn is_suspended_is_false_on_absent_and_empty_metadata() {
        let empty = serde_json::json!({});
        assert!(!is_suspended(&empty));

        let missing = serde_json::Value::Null;
        assert!(!is_suspended(&missing));
    }

    #[test]
    fn is_suspended_is_false_after_stamp_resumed() {
        let mut metadata = serde_json::json!({});
        let ledger = ledger_with(10, 0.1);
        stamp_suspended(
            &mut metadata,
            Suspension {
                resume_at: "Next",
                reason: SuspendReason::SuspendNode,
                origin_identity: None,
                ledger: &ledger,
            },
        );
        assert!(is_suspended(&metadata));

        stamp_resumed(&mut metadata);
        assert!(!is_suspended(&metadata));
    }

    #[test]
    fn stamp_resumed_preserves_key_and_increments_resume_count() {
        let mut metadata = serde_json::json!({});
        let ledger = ledger_with(5, 0.01);

        stamp_suspended(
            &mut metadata,
            Suspension {
                resume_at: "A",
                reason: SuspendReason::OperatorPause,
                origin_identity: None,
                ledger: &ledger,
            },
        );
        stamp_resumed(&mut metadata);
        let state = read_suspension(&metadata).expect("key must survive resume");
        assert_eq!(state.resume_count, 1);
        assert!(!state.requested);

        // Second suspend/resume cycle.
        stamp_suspended(
            &mut metadata,
            Suspension {
                resume_at: "B",
                reason: SuspendReason::OperatorPause,
                origin_identity: None,
                ledger: &ledger,
            },
        );
        let mid = read_suspension(&metadata).unwrap();
        assert_eq!(
            mid.resume_count, 1,
            "resume_count carries forward across a re-suspend"
        );

        stamp_resumed(&mut metadata);
        let state = read_suspension(&metadata).unwrap();
        assert_eq!(state.resume_count, 2);
    }

    #[test]
    fn request_suspension_then_suspension_requested_round_trips() {
        let mut metadata = serde_json::json!({});
        assert!(!suspension_requested(&metadata));

        request_suspension(&mut metadata);
        assert!(suspension_requested(&metadata));
        assert!(
            !is_suspended(&metadata),
            "requesting does not itself suspend"
        );
    }

    #[test]
    fn stamp_suspended_tolerates_non_object_metadata() {
        let mut metadata = serde_json::Value::Null;
        let ledger = ledger_with(0, 0.0);
        stamp_suspended(
            &mut metadata,
            Suspension {
                resume_at: "X",
                reason: SuspendReason::OperatorPause,
                origin_identity: None,
                ledger: &ledger,
            },
        );
        assert!(metadata.is_object());
        assert!(is_suspended(&metadata));
    }

    #[test]
    fn request_suspension_tolerates_non_object_metadata() {
        let mut metadata = serde_json::Value::Null;
        request_suspension(&mut metadata);
        assert!(metadata.is_object());
        assert!(suspension_requested(&metadata));
    }
}
