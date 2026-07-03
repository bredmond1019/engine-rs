//! Live Postgres insert/read round-trip for the `events` table.
//!
//! Gated on `DATABASE_URL`: self-skips (does not fail) when the env var is unset so
//! `cargo test` stays green in CI, which runs with no Postgres available (per
//! EN.0.A). Run against a live database with:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p engine-store
//! ```
//!
//! The target `events` table is the orchestrator's existing schema (contract §4):
//! `id uuid, workflow_type varchar(150), data json, task_context json,
//! created_at timestamp, updated_at timestamp`.

use std::collections::HashMap;

use chrono::Utc;
use engine_contract::{EventsRow, NodeRun, NodeRunStatus, TaskContext};
use engine_store::{connect, get_event, insert_event, update_event};
use uuid::Uuid;

#[tokio::test]
async fn insert_then_read_round_trips_an_events_row() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL unset — skipping live Postgres round-trip test");
        return;
    };

    let pool = connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let mut node_runs = HashMap::new();
    node_runs.insert(
        "DataIngestionNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Pending,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        },
    );

    let now = Utc::now();
    let row = EventsRow {
        id: Uuid::new_v4(),
        workflow_type: "engine-store-round-trip-test".to_string(),
        data: serde_json::json!({ "ticket_id": "T-round-trip" }),
        task_context: TaskContext {
            event: serde_json::json!({ "ticket_id": "T-round-trip" }),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs,
        },
        created_at: now,
        updated_at: now,
    };

    insert_event(&pool, &row)
        .await
        .expect("insert_event should succeed");

    let read_back = get_event(&pool, row.id)
        .await
        .expect("get_event should find the inserted row");
    assert_eq!(read_back, row, "read-back row must match the inserted row");

    // Exercise update_event: flip the node to success and re-read.
    let mut updated = row.clone();
    updated.task_context.node_runs.insert(
        "DataIngestionNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(now),
            completed_at: Some(Utc::now()),
            error: None,
            input: None,
            usage: None,
        },
    );
    updated.updated_at = Utc::now();

    update_event(&pool, &updated)
        .await
        .expect("update_event should succeed");

    let read_back_after_update = get_event(&pool, row.id)
        .await
        .expect("get_event should find the updated row");
    assert_eq!(
        read_back_after_update.task_context.node_runs["DataIngestionNode"].status,
        NodeRunStatus::Success
    );

    // Clean up so repeated local runs don't accumulate rows.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(row.id)
        .execute(&pool)
        .await
        .expect("cleanup delete should succeed");
}
