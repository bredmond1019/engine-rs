//! The run-scoped, append-only ledger of node DISPATCHES (EN.14.F).
//!
//! # Per-dispatch, not per-call
//!
//! A row appended here records a node **dispatch** — one pass through
//! [`crate::workflow`]'s single dispatch choke point (`node_context`). It is
//! NOT a record of an LLM call: 22 production node bodies dispatch an inner
//! `ClaudeCodeStep` *outside* `node_context`, so `claude_sessions`
//! ([`crate::sessions`]) is the per-CALL view and this is the per-DISPATCH
//! view. The two are different cardinalities and both are correct — a single
//! dispatch of a node whose body makes three Claude calls contributes one row
//! here and three in `claude_sessions`.
//!
//! # Why this exists alongside `ctx.nodes`, not instead of it
//!
//! `ctx.nodes` is a `HashMap<String, serde_json::Value>` — one slot per node
//! identity, so a node re-dispatched by a retry loop overwrites its own
//! history there. This module mirrors [`crate::sessions`] symbol-for-symbol to
//! give the framework an append-only, order-preserving alternative: every
//! dispatch gets its own entry, so a retried node leaves as many entries as it
//! made attempts. `ctx.nodes` itself is unchanged by this module.

use engine_contract::NodeInvocation;
use serde_json::Value;

/// The `TaskContext::metadata` key under which the invocation ledger lives.
/// Sibling to [`crate::sessions::SESSIONS_METADATA_KEY`] and
/// `workflow::RUN_ID_METADATA_KEY`/`BUDGET_METADATA_KEY`.
pub const INVOCATIONS_METADATA_KEY: &str = "node_invocations";

/// Append one invocation to `metadata`'s ledger, creating it if absent.
/// Order-preserving.
///
/// # No de-duplication, deliberately
///
/// Called exactly once per dispatch, at the point `node_context` returns.
/// Nothing replays it, for the same reason [`crate::sessions::append_session`]
/// does not dedupe: two dispatches can legitimately be identical in every
/// recorded field (same node, same outcome, same message), and collapsing
/// them would silently understate exactly the retried runs this ledger exists
/// to make auditable.
pub fn append_invocation(metadata: &mut Value, invocation: NodeInvocation) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }

    match metadata
        .get_mut(INVOCATIONS_METADATA_KEY)
        .filter(|v| v.is_array())
    {
        Some(Value::Array(entries)) => entries.push(serde_json::json!(invocation)),
        _ => metadata[INVOCATIONS_METADATA_KEY] = serde_json::json!([invocation]),
    }
}

/// Read back the ledger in order. Returns an empty vec for absent, non-array,
/// or non-object metadata, and silently skips any malformed entry — never
/// panics, never errors. A telemetry channel must not be able to fail a run.
pub fn read_invocations(metadata: &Value) -> Vec<NodeInvocation> {
    metadata
        .get(INVOCATIONS_METADATA_KEY)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| serde_json::from_value::<NodeInvocation>(e.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The next per-run sequence number: the current ledger length, read off the
/// same metadata immediately before append. Counts from 0.
#[must_use]
pub fn next_seq(metadata: &Value) -> u64 {
    read_invocations(metadata).len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use engine_contract::NodeInvocationStatus;
    use uuid::Uuid;

    fn invocation(node: &str, seq: u64, status: NodeInvocationStatus) -> NodeInvocation {
        NodeInvocation {
            id: Uuid::new_v4(),
            run_id: Some("run-1".to_string()),
            campaign_id: None,
            node: node.to_string(),
            seq,
            started_at: DateTime::<Utc>::from_timestamp(seq as i64, 0).unwrap(),
            completed_at: DateTime::<Utc>::from_timestamp(seq as i64 + 1, 0).unwrap(),
            status,
            error: None,
        }
    }

    #[test]
    fn appends_in_order_and_reads_back() {
        let mut meta = serde_json::json!({});
        append_invocation(
            &mut meta,
            invocation("Implement", 0, NodeInvocationStatus::Success),
        );
        append_invocation(
            &mut meta,
            invocation("Test", 1, NodeInvocationStatus::Success),
        );
        append_invocation(
            &mut meta,
            invocation("Review", 2, NodeInvocationStatus::Failed),
        );

        let entries = read_invocations(&meta);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].node, "Implement");
        assert_eq!(entries[2].status, NodeInvocationStatus::Failed);
    }

    /// THE PROPERTY `ctx.nodes` STRUCTURALLY CANNOT HAVE. Two dispatches of the
    /// same node name produce two entries here, where `ctx.nodes` would hold
    /// only the second.
    #[test]
    fn two_dispatches_of_the_same_node_produce_two_entries() {
        let mut meta = serde_json::json!({});
        append_invocation(
            &mut meta,
            invocation("Implement", 0, NodeInvocationStatus::Failed),
        );
        append_invocation(
            &mut meta,
            invocation("Implement", 1, NodeInvocationStatus::Success),
        );

        let entries = read_invocations(&meta);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.node == "Implement"));
        assert_eq!(entries[0].status, NodeInvocationStatus::Failed);
        assert_eq!(entries[1].status, NodeInvocationStatus::Success);
    }

    #[test]
    fn next_seq_counts_from_zero_and_increments() {
        let mut meta = serde_json::json!({});
        assert_eq!(next_seq(&meta), 0);

        append_invocation(
            &mut meta,
            invocation("Implement", 0, NodeInvocationStatus::Success),
        );
        assert_eq!(next_seq(&meta), 1);

        append_invocation(
            &mut meta,
            invocation("Implement", 1, NodeInvocationStatus::Success),
        );
        assert_eq!(next_seq(&meta), 2);
    }

    #[test]
    fn append_repairs_a_non_object_or_non_array_ledger() {
        let mut meta = serde_json::json!("not an object");
        append_invocation(
            &mut meta,
            invocation("Implement", 0, NodeInvocationStatus::Success),
        );
        assert_eq!(read_invocations(&meta).len(), 1);

        let mut meta = serde_json::json!({ INVOCATIONS_METADATA_KEY: "not an array" });
        append_invocation(
            &mut meta,
            invocation("Implement", 0, NodeInvocationStatus::Success),
        );
        assert_eq!(read_invocations(&meta).len(), 1);
    }

    #[test]
    fn reading_absent_or_malformed_metadata_yields_an_empty_ledger_and_never_panics() {
        assert!(read_invocations(&serde_json::json!({})).is_empty());
        assert!(read_invocations(&serde_json::json!(null)).is_empty());
        assert!(read_invocations(&serde_json::json!({ INVOCATIONS_METADATA_KEY: 7 })).is_empty());
        assert_eq!(next_seq(&serde_json::json!({})), 0);
    }

    /// A malformed entry is skipped, not fatal, and never hides the valid ones around it.
    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        let good1 = invocation("Implement", 0, NodeInvocationStatus::Success);
        let good2 = invocation("Review", 1, NodeInvocationStatus::Failed);
        let meta = serde_json::json!({
            INVOCATIONS_METADATA_KEY: [
                serde_json::to_value(&good1).unwrap(),
                { "nonsense": true },
                serde_json::to_value(&good2).unwrap(),
            ]
        });

        let entries = read_invocations(&meta);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].node, "Implement");
        assert_eq!(entries[1].node, "Review");
    }
}
