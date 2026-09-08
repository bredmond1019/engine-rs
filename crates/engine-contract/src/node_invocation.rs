//! `NodeInvocation` — the run-scoped, append-only ledger of node DISPATCHES (EN.14.F).
//!
//! # Per-dispatch, not per-call
//!
//! A row here records a node **dispatch** — one pass through the framework's
//! single dispatch choke point (`node_context` in `engine-core`'s
//! `workflow.rs`). It is NOT a record of an LLM call: 22 production node
//! bodies dispatch an inner `ClaudeCodeStep` *outside* `node_context`, so the
//! per-call view is `claude_sessions` (`ClaudeSession` in
//! `engine-core::sessions`), and this is the per-dispatch view. They have
//! different cardinalities and both are correct: a single dispatch of a node
//! whose body makes three Claude calls contributes one row here and three there.
//!
//! # Why a ledger and not `ctx.nodes`
//!
//! `ctx.nodes` is a `HashMap<String, serde_json::Value>` — one slot per node
//! identity. A node re-dispatched by a retry loop overwrites its own history
//! there, so neither "how many times did this node actually run" nor "what did
//! attempt 2 look like" survives to the end of the run. This ledger is
//! append-only and order-preserving instead: every dispatch gets its own row,
//! keyed by a fresh [`Uuid`] and an ever-increasing `seq`, so a retried node
//! leaves as many rows as it made attempts.
//!
//! Modelled directly on `engine-core::sessions::ClaudeSession` — read that file
//! first; this is deliberately its sibling, not a new pattern.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Whether a dispatch succeeded or failed. A failed dispatch is still a
/// dispatch and still gets a row — `node_context` appends on both branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeInvocationStatus {
    Success,
    Failed,
}

/// One node dispatch. See the module doc for the per-dispatch vs. per-call
/// distinction and why this exists alongside, not instead of, `ctx.nodes`.
///
/// Every optional field carries `#[serde(default)]`, matching `ClaudeSession`'s
/// forward-tolerance discipline: a ledger entry must always deserialize, even
/// one written before a field existed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeInvocation {
    /// Identifies this row. A fresh id is minted per dispatch, which is what
    /// makes the store-side writer's `ON CONFLICT (id) DO NOTHING` safe: a
    /// re-dispatch of the same node always carries a new id, so it can never
    /// collide with a prior row.
    pub id: Uuid,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub campaign_id: Option<String>,
    /// The `Node::name()` identity of the dispatched node — the same
    /// attribution key `ClaudeSession::node` uses.
    pub node: String,
    /// Monotonically increasing per-run sequence number, assigned by
    /// [`crate::node_invocation`]'s companion reader in `engine-core::invocations`
    /// (`next_seq`) immediately before append. Distinguishes dispatches of the
    /// same node from each other independent of wall-clock resolution.
    pub seq: u64,
    /// Captured before `node.process(ctx)` is called.
    pub started_at: DateTime<Utc>,
    /// Captured after `node.process(ctx)` returns.
    pub completed_at: DateTime<Utc>,
    pub status: NodeInvocationStatus,
    /// The node's error message on a `Failed` dispatch. `None` on `Success`.
    #[serde(default)]
    pub error: Option<String>,
    /// The dispatch's output payload (its own entry in the post-call
    /// `ctx.nodes`), captured here so a retried node's earlier attempts
    /// survive `ctx.nodes` overwriting itself (EN.14.G). `None` on a `Failed`
    /// dispatch — there is no output payload to retain — and possibly `None`
    /// on `Success` too, if the node's `ctx.nodes` entry was absent.
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    /// Set when `payload` was replaced by an explicit truncation marker
    /// because it exceeded `payload_cap_bytes`. A caller must never be able
    /// to mistake a truncated payload for a short one — see
    /// `engine_core::invocations::truncate_payload`.
    #[serde(default)]
    pub payload_truncated: bool,
    /// The cap ACTUALLY APPLIED to this row's payload, not merely the
    /// configured one — recorded even on a `Failed` dispatch (where
    /// `payload` is `None`) so every row is interpretable on the same terms
    /// months later.
    #[serde(default)]
    pub payload_cap_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NodeInvocation {
        NodeInvocation {
            id: Uuid::nil(),
            run_id: Some("run-1".to_string()),
            campaign_id: None,
            node: "Implement".to_string(),
            seq: 0,
            started_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            completed_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
            status: NodeInvocationStatus::Success,
            error: None,
            payload: Some(serde_json::json!({"modified_files": ["a.rs"]})),
            payload_truncated: false,
            payload_cap_bytes: 65_536,
        }
    }

    #[test]
    fn node_invocation_round_trips_through_serde() {
        let inv = sample();
        let json = serde_json::to_value(&inv).unwrap();
        let back: NodeInvocation = serde_json::from_value(json).unwrap();
        assert_eq!(inv, back);
    }

    #[test]
    fn node_invocation_status_round_trips_and_uses_snake_case() {
        assert_eq!(
            serde_json::to_value(NodeInvocationStatus::Success).unwrap(),
            serde_json::json!("success")
        );
        assert_eq!(
            serde_json::to_value(NodeInvocationStatus::Failed).unwrap(),
            serde_json::json!("failed")
        );

        let success: NodeInvocationStatus =
            serde_json::from_value(serde_json::json!("success")).unwrap();
        assert_eq!(success, NodeInvocationStatus::Success);
        let failed: NodeInvocationStatus =
            serde_json::from_value(serde_json::json!("failed")).unwrap();
        assert_eq!(failed, NodeInvocationStatus::Failed);
    }

    /// A failed dispatch still records an error message; a successful one carries none.
    #[test]
    fn failed_status_carries_an_error_message() {
        let mut inv = sample();
        inv.status = NodeInvocationStatus::Failed;
        inv.error = Some("boom".to_string());

        let json = serde_json::to_value(&inv).unwrap();
        let back: NodeInvocation = serde_json::from_value(json).unwrap();
        assert_eq!(back.status, NodeInvocationStatus::Failed);
        assert_eq!(back.error.as_deref(), Some("boom"));
    }

    /// An entry written before optional fields existed must still deserialize —
    /// the same forward-tolerance `ClaudeSession` gives its own optional fields.
    #[test]
    fn deserializes_without_optional_fields_present() {
        let json = serde_json::json!({
            "id": Uuid::nil(),
            "node": "Implement",
            "seq": 0,
            "started_at": "1970-01-01T00:00:00Z",
            "completed_at": "1970-01-01T00:00:01Z",
            "status": "success",
        });

        let inv: NodeInvocation = serde_json::from_value(json).unwrap();
        assert_eq!(inv.run_id, None);
        assert_eq!(inv.campaign_id, None);
        assert_eq!(inv.error, None);
        assert_eq!(inv.payload, None);
        assert!(!inv.payload_truncated);
        assert_eq!(inv.payload_cap_bytes, 0);
    }

    /// An EN.14.F-era entry — written before this block's payload fields
    /// existed — carries none of `payload`/`payload_truncated`/
    /// `payload_cap_bytes` and must still deserialize, per this file's
    /// forward-tolerance discipline.
    #[test]
    fn en_14_f_era_entry_without_payload_fields_still_deserializes() {
        let json = serde_json::json!({
            "id": Uuid::nil(),
            "run_id": "run-1",
            "campaign_id": null,
            "node": "Implement",
            "seq": 0,
            "started_at": "1970-01-01T00:00:00Z",
            "completed_at": "1970-01-01T00:00:01Z",
            "status": "success",
            "error": null,
        });

        let inv: NodeInvocation = serde_json::from_value(json).unwrap();
        assert_eq!(inv.node, "Implement");
        assert_eq!(inv.payload, None);
        assert!(!inv.payload_truncated);
        assert_eq!(inv.payload_cap_bytes, 0);
    }
}
