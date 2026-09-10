//! The run-scoped, append-only ledger of node DISPATCHES (EN.14.F).
//!
//! # Per-dispatch, not per-call
//!
//! A row appended here records a node **dispatch** — one pass through
//! [`crate::workflow`]'s single dispatch choke point (`node_context`). It is
//! NOT a record of an LLM call: 22 production node bodies dispatch an inner
//! `AgentCodeStep` *outside* `node_context`, so `claude_sessions`
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

use engine_contract::{NodeInvocation, TaskContext};
use serde_json::Value;

use crate::policy::profiles::RESOLVED_POLICY_IDENTITY;

/// The `TaskContext::metadata` key under which the invocation ledger lives.
/// Sibling to [`crate::sessions::SESSIONS_METADATA_KEY`] and
/// `workflow::RUN_ID_METADATA_KEY`/`BUDGET_METADATA_KEY`.
pub const INVOCATIONS_METADATA_KEY: &str = "node_invocations";

/// The `ResolvedPolicy` stamp field a workflow's payload-retention knob is
/// read from (EN.14.G task 1). Kept as a named constant so the string used to
/// write the field (in each workflow's `PartialPolicy` -> `serde_json` stamp)
/// and the string used to read it here can never silently drift apart.
pub const NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD: &str = "node_invocation_payload_cap_bytes";

/// Built-in default for the per-payload retention cap (EN.14.G).
///
/// Payload size is entirely unmeasured as of this block — no run with
/// retained payloads exists anywhere to measure. 64 KiB is a starting point,
/// not a derived figure: generous enough to hold a typical `modified_files`
/// list or a small structured result without truncating in the common case,
/// small enough that a single dispatch's payload cannot make a meaningful
/// dent in Postgres row size or the in-memory `ctx.metadata` clone this
/// ledger already piggybacks on. Task 4 measures actual per-run growth on a
/// multi-node run and records the comparison against EN.14.F's no-payload
/// baseline; this value is free to move once that measurement exists.
/// Behavior-stable: this is also the value every named profile's `baseline`
/// bundle sets explicitly, so introducing the knob changes no existing run.
pub const DEFAULT_PAYLOAD_CAP_BYTES: u64 = 65_536;

/// Read the per-payload retention cap off the already-resolved policy stamp,
/// UNTYPED — this is framework-level code with no workflow's policy type in
/// scope, so it can never call `policy::resolved_policy_strict::<P>` (which
/// requires a concrete `P`). Instead it looks up
/// `ctx.nodes[RESOLVED_POLICY_IDENTITY]` as a plain `serde_json::Value` and
/// reads the optional numeric field
/// [`NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD`] off it.
///
/// Falls back to [`DEFAULT_PAYLOAD_CAP_BYTES`] when the stamp is absent, is
/// not a JSON object, or lacks the field (or the field is present but not a
/// non-negative integer) — never panics, mirroring this module's existing
/// "a telemetry channel must not be able to fail a run" discipline.
#[must_use]
pub fn payload_cap_from_resolved_policy(ctx: &TaskContext) -> u64 {
    ctx.nodes
        .get(RESOLVED_POLICY_IDENTITY)
        .and_then(Value::as_object)
        .and_then(|obj| obj.get(NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAYLOAD_CAP_BYTES)
}

/// Cap a dispatch's output payload for retention.
///
/// Returns `(value.clone(), false)` unchanged when its serialized byte length
/// (`serde_json::to_vec`) is within `cap_bytes`. Otherwise returns an
/// explicit REPLACEMENT value marked as truncated — never a silently
/// shortened copy of the original shape, so a caller can never mistake a
/// truncated payload for a short one — paired with `true`.
#[must_use]
pub fn truncate_payload(value: &Value, cap_bytes: u64) -> (Value, bool) {
    let byte_len = serde_json::to_vec(value).map(|bytes| bytes.len() as u64);

    match byte_len {
        Ok(len) if len <= cap_bytes => (value.clone(), false),
        _ => (
            serde_json::json!({
                "__truncated__": true,
                "cap_bytes": cap_bytes,
            }),
            true,
        ),
    }
}

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
            payload: None,
            payload_truncated: false,
            payload_cap_bytes: 0,
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

    fn ctx_with_nodes(nodes: std::collections::HashMap<String, Value>) -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes,
            metadata: serde_json::json!({}),
            node_runs: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn payload_cap_from_resolved_policy_falls_back_to_default_when_stamp_absent() {
        let ctx = ctx_with_nodes(std::collections::HashMap::new());
        assert_eq!(
            payload_cap_from_resolved_policy(&ctx),
            DEFAULT_PAYLOAD_CAP_BYTES
        );
    }

    #[test]
    fn payload_cap_from_resolved_policy_reads_the_stamped_value() {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!({ NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD: 4096 }),
        );
        let ctx = ctx_with_nodes(nodes);
        assert_eq!(payload_cap_from_resolved_policy(&ctx), 4096);
    }

    #[test]
    fn payload_cap_from_resolved_policy_falls_back_when_stamp_is_not_an_object() {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!("not an object"),
        );
        let ctx = ctx_with_nodes(nodes);
        assert_eq!(
            payload_cap_from_resolved_policy(&ctx),
            DEFAULT_PAYLOAD_CAP_BYTES
        );
    }

    #[test]
    fn payload_cap_from_resolved_policy_falls_back_when_field_is_wrong_typed_or_missing() {
        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!({ "some_other_field": 1 }),
        );
        let ctx = ctx_with_nodes(nodes);
        assert_eq!(
            payload_cap_from_resolved_policy(&ctx),
            DEFAULT_PAYLOAD_CAP_BYTES
        );

        let mut nodes = std::collections::HashMap::new();
        nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!({ NODE_INVOCATION_PAYLOAD_CAP_BYTES_FIELD: "not a number" }),
        );
        let ctx = ctx_with_nodes(nodes);
        assert_eq!(
            payload_cap_from_resolved_policy(&ctx),
            DEFAULT_PAYLOAD_CAP_BYTES
        );
    }

    #[test]
    fn truncate_payload_returns_under_cap_payload_unchanged() {
        let value = serde_json::json!({ "modified_files": ["a.rs", "b.rs"] });
        let (out, truncated) = truncate_payload(&value, 4096);
        assert_eq!(out, value);
        assert!(!truncated);
    }

    #[test]
    fn truncate_payload_marks_over_cap_payload_explicitly_rather_than_shortening_it() {
        let value = serde_json::json!({ "modified_files": vec!["x"; 10_000] });
        let (out, truncated) = truncate_payload(&value, 16);
        assert!(truncated);
        // The caller must never mistake this for a short copy of the
        // original shape — it carries no `modified_files` key at all.
        assert!(out.get("modified_files").is_none());
        assert_eq!(out.get("__truncated__"), Some(&serde_json::json!(true)));
        assert_eq!(out.get("cap_bytes"), Some(&serde_json::json!(16)));
    }
}
