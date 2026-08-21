//! Async durable-write seam: bridges `engine-core`'s synchronous `on_progress`
//! callback to `engine-store`'s async Postgres writer.
//!
//! `Workflow::run`'s `on_progress` seam
//! (`crates/engine-core/src/workflow.rs::OnProgress`) is a synchronous
//! `FnMut(&TaskContext)`, but persisting to Postgres is async. This module
//! bridges the two with a `tokio::sync::mpsc` channel: the `on_progress`
//! closure built by [`durable_on_progress`] clones each snapshot and sends it
//! down the channel; a background task spawned by [`spawn_durable_writer`]
//! drains the channel and awaits the `engine_store` insert/update, keeping
//! Postgres I/O off the run's hot path.
//!
//! Every snapshot for a given `run_id` is persisted via
//! `engine_store::upsert_event` (with `engine_store::touch` stamping
//! `updated_at`): the first snapshot (all nodes PENDING, emitted by
//! `Workflow::run` before the first node executes) inserts the row, and every
//! subsequent snapshot for that run id updates it in place, on conflict, by
//! `id`. There is deliberately no per-process "have I seen this run before"
//! state (no `seen_runs` set) — a resume that lands in a fresh `engine-serve`
//! process must not take an insert path and hit a primary-key conflict; the
//! upsert makes durability writes idempotent regardless of which process
//! wrote the row first. Node identity (the node's type/class name) is
//! preserved as-is from the `TaskContext` snapshot, so it stays the join key
//! across `nodes`/`node_runs` per the contract.
//!
//! When no `DATABASE_URL` is configured (`pool` is `None`), the background
//! writer still drains the channel but performs no Postgres I/O — the
//! durable-write path self-skips rather than failing, keeping CI green with
//! no live database (mirrors `engine-store`'s own `postgres_round_trip.rs`
//! self-skip pattern).

use chrono::{DateTime, Utc};
use engine_contract::{EventsRow, TaskContext};
use engine_store::{touch, upsert_event};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

/// One snapshot handed to the durable writer: the run it belongs to, the
/// `workflow_type` and original triggering `data` (constant for the run's
/// lifetime), and the `TaskContext` snapshot at this boundary.
#[derive(Debug, Clone)]
pub struct DurableMessage {
    pub run_id: Uuid,
    pub workflow_type: String,
    pub data: serde_json::Value,
    pub snapshot: TaskContext,
}

/// Cheaply-cloneable handle for sending snapshots to the background durable
/// writer. Safe to clone into an `on_progress` closure and into multiple
/// concurrent runs.
#[derive(Clone)]
pub struct DurableHandle {
    sender: mpsc::UnboundedSender<DurableMessage>,
    /// The same pool [`spawn_durable_writer`] was handed, retained here so a
    /// synchronous reader (EN.6.F task 11's resume handler) can fall back to
    /// `engine_store::get_event` for a suspended run this process never saw
    /// (a restart) without threading a second `Option<PgPool>` through
    /// `AppState`. `None` when `DATABASE_URL` was unset.
    pool: Option<PgPool>,
}

impl DurableHandle {
    /// Send a snapshot to the background writer. Errors (the writer task
    /// having ended) are swallowed: a dropped durable-write must never fail
    /// or interrupt the run itself — the run's authoritative state is the
    /// in-memory `TaskContext` returned by `Workflow::run` plus the
    /// live-state store (task 2); the durable row is a best-effort record.
    pub fn send(&self, message: DurableMessage) {
        let _ = self.sender.send(message);
    }

    /// The Postgres pool this handle was constructed with, if any —
    /// `None` when `DATABASE_URL` was unset (the in-memory-only path).
    pub fn pool(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    /// Record a snapshot for `run_id`, cloning `snapshot` for the channel.
    /// Convenience wrapper around [`DurableHandle::send`] for callers that
    /// hold the constant `workflow_type`/`data` separately (e.g. an
    /// `on_progress` closure built by [`durable_on_progress`]).
    pub fn record(
        &self,
        run_id: Uuid,
        workflow_type: &str,
        data: &serde_json::Value,
        snapshot: &TaskContext,
    ) {
        self.send(DurableMessage {
            run_id,
            workflow_type: workflow_type.to_string(),
            data: data.clone(),
            snapshot: snapshot.clone(),
        });
    }
}

/// Test-only constructor: a [`DurableHandle`] wired to a channel the caller
/// can read from directly, plus its receiving end — for asserting exactly
/// what a caller (e.g. `suspend::publish_step_progress`,
/// EN.ticket.orchestration-abort-and-progress task 4) sent, without a real
/// Postgres pool. `DurableHandle`'s `sender`/`pool` fields are private to
/// this module, so callers in other modules (`suspend.rs`'s own tests)
/// cannot build one directly — this is the seam for them.
#[cfg(test)]
pub(crate) fn test_handle() -> (DurableHandle, mpsc::UnboundedReceiver<DurableMessage>) {
    let (sender, receiver) = mpsc::unbounded_channel::<DurableMessage>();
    (DurableHandle { sender, pool: None }, receiver)
}

/// Map a `DurableMessage` plus a `(created_at, updated_at)` timestamp pair
/// into the `EventsRow` the contract expects. Kept as a pure function
/// (independent of the channel/async plumbing) so the mapping's
/// byte-identical shape can be asserted directly in tests.
pub fn message_to_row(
    message: &DurableMessage,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> EventsRow {
    EventsRow {
        id: message.run_id,
        workflow_type: message.workflow_type.clone(),
        data: message.data.clone(),
        task_context: message.snapshot.clone(),
        created_at,
        updated_at,
    }
}

/// Spawn the background writer task and return the [`DurableHandle`] used to
/// feed it. `pool` is `None` when `DATABASE_URL` is unset: the task still
/// drains the channel (so senders never block/backlog) but performs no
/// Postgres I/O, self-skipping the durable write.
pub fn spawn_durable_writer(pool: Option<PgPool>) -> DurableHandle {
    let (sender, mut receiver) = mpsc::unbounded_channel::<DurableMessage>();
    let writer_pool = pool.clone();

    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            let Some(pool) = writer_pool.as_ref() else {
                // No DATABASE_URL configured: self-skip the Postgres write,
                // do not fail or panic.
                continue;
            };

            // `created_at` is immutable once a row exists (`upsert_event`
            // excludes it from the `DO UPDATE` set), so the value passed here
            // for an already-existing row is a don't-care; it only matters
            // for the very first insert of a given run id.
            let now = Utc::now();
            let mut row = message_to_row(&message, now, now);
            touch(&mut row);
            if let Err(err) = upsert_event(pool, &row).await {
                eprintln!(
                    "durable write: upsert_event failed for run {}: {err}",
                    message.run_id
                );
            }
        }
    });

    DurableHandle { sender, pool }
}

/// Build an `on_progress`-compatible closure that forwards every snapshot for
/// `run_id` to `handle`. `workflow_type` and `data` are captured once (they
/// are constant for the run's lifetime) so the closure only needs the
/// per-boundary `TaskContext` snapshot, matching
/// `engine_core::workflow::OnProgress<'a>`'s signature
/// (`Box<dyn FnMut(&TaskContext) + 'a>`).
pub fn durable_on_progress(
    handle: DurableHandle,
    run_id: Uuid,
    workflow_type: String,
    data: serde_json::Value,
) -> impl FnMut(&TaskContext) + Send + 'static {
    move |snapshot: &TaskContext| {
        handle.record(run_id, &workflow_type, &data, snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use engine_contract::{NodeRun, NodeRunStatus};
    use std::collections::HashMap;
    use std::time::Duration;

    fn all_pending_snapshot(node_identities: &[&str]) -> TaskContext {
        let mut node_runs = HashMap::new();
        for identity in node_identities {
            node_runs.insert(
                identity.to_string(),
                NodeRun {
                    status: NodeRunStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    error: None,
                    input: None,
                    usage: None,
                },
            );
        }
        TaskContext {
            event: serde_json::json!({ "ticket_id": "T-1" }),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs,
        }
    }

    /// (a) A run's first snapshot (all nodes PENDING, before the first node
    /// runs) maps to an `EventsRow` that is byte-identical (per the EN.0.B
    /// round-trip oracle: semantic JSON equality, contract shape) to what the
    /// Python orchestrator would write for an equivalent seed row.
    #[test]
    fn first_snapshot_maps_to_all_pending_events_row_matching_contract_shape() {
        let run_id = Uuid::parse_str("5b1f9a4c-6e2d-4b3a-8b8b-1e2f3a4b5c6d").unwrap();
        let created_at = Utc.with_ymd_and_hms(2026, 6, 20, 9, 0, 0).unwrap();

        let message = DurableMessage {
            run_id,
            workflow_type: "sdlc-flow".to_string(),
            data: serde_json::json!({ "ticket_id": "T-1" }),
            snapshot: all_pending_snapshot(&["DataIngestionNode", "EmbeddingNode"]),
        };

        let row = message_to_row(&message, created_at, created_at);

        // Round-trip the row through serde_json (the EN.0.B oracle's
        // technique): semantic equality, no field/casing drift.
        let json = serde_json::to_value(&row).expect("EventsRow serializes");
        let round_tripped: EventsRow =
            serde_json::from_value(json.clone()).expect("EventsRow deserializes back");
        assert_eq!(round_tripped, row);

        // Contract §4 top-level shape.
        for key in [
            "id",
            "workflow_type",
            "data",
            "task_context",
            "created_at",
            "updated_at",
        ] {
            assert!(json.get(key).is_some(), "missing top-level field: {key}");
        }

        // Every declared node is PENDING, matching `Workflow::run`'s seed
        // snapshot emitted before the first node executes.
        let node_runs = &json["task_context"]["node_runs"];
        for identity in ["DataIngestionNode", "EmbeddingNode"] {
            assert_eq!(node_runs[identity]["status"], "pending");
            assert!(node_runs[identity]["started_at"].is_null());
            assert!(node_runs[identity]["completed_at"].is_null());
        }

        assert_eq!(row.id, run_id);
        assert_eq!(row.workflow_type, "sdlc-flow");
        assert_eq!(row.created_at, created_at);
    }

    /// (b) A full snapshot sequence (seed -> RUNNING -> SUCCESS) produces the
    /// same node identity as the join key at every boundary, and the final
    /// row reflects the last snapshot's state.
    #[test]
    fn snapshot_sequence_preserves_node_identity_as_join_key() {
        let run_id = Uuid::new_v4();
        let workflow_type = "fixture".to_string();
        let data = serde_json::json!({});

        let seed = DurableMessage {
            run_id,
            workflow_type: workflow_type.clone(),
            data: data.clone(),
            snapshot: all_pending_snapshot(&["MarkerNode"]),
        };
        let seed_row = message_to_row(&seed, Utc::now(), Utc::now());

        let mut running_snapshot = all_pending_snapshot(&["MarkerNode"]);
        running_snapshot
            .node_runs
            .get_mut("MarkerNode")
            .unwrap()
            .status = NodeRunStatus::Success;

        let final_message = DurableMessage {
            run_id,
            workflow_type,
            data,
            snapshot: running_snapshot,
        };
        let final_row = message_to_row(&final_message, seed_row.created_at, Utc::now());

        assert_eq!(seed_row.id, final_row.id);
        assert!(seed_row.task_context.node_runs.contains_key("MarkerNode"));
        assert!(final_row.task_context.node_runs.contains_key("MarkerNode"));
        assert_eq!(
            final_row.task_context.node_runs["MarkerNode"].status,
            NodeRunStatus::Success
        );
    }

    /// The Postgres insert/update path self-skips (does not panic or fail)
    /// when no pool is configured (i.e. `DATABASE_URL` was unset), which is
    /// how `spawn_durable_writer` is invoked in that case.
    #[tokio::test]
    async fn writer_self_skips_when_no_pool_is_configured() {
        let handle = spawn_durable_writer(None);

        handle.record(
            Uuid::new_v4(),
            "sdlc-flow",
            &serde_json::json!({}),
            &all_pending_snapshot(&["DataIngestionNode"]),
        );
        handle.record(
            Uuid::new_v4(),
            "sdlc-flow",
            &serde_json::json!({}),
            &all_pending_snapshot(&["DataIngestionNode"]),
        );

        // Give the background task a chance to drain the channel; there is
        // nothing to assert against Postgres (no pool exists), so this test
        // passes as long as sending/draining does not panic.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[test]
    fn durable_on_progress_forwards_snapshots_to_the_handle() {
        let (sender, mut receiver) = mpsc::unbounded_channel::<DurableMessage>();
        let handle = DurableHandle { sender, pool: None };
        let run_id = Uuid::new_v4();

        let mut on_progress = durable_on_progress(
            handle,
            run_id,
            "fixture".to_string(),
            serde_json::json!({ "k": "v" }),
        );

        let snapshot = all_pending_snapshot(&["MarkerNode"]);
        on_progress(&snapshot);

        let received = receiver.try_recv().expect("a message should be queued");
        assert_eq!(received.run_id, run_id);
        assert_eq!(received.workflow_type, "fixture");
        assert_eq!(received.data, serde_json::json!({ "k": "v" }));
        assert_eq!(received.snapshot, snapshot);
    }
}
