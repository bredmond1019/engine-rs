//! Live Postgres insert/read round-trip for the `events` table.
//!
//! This test is `#[ignore]`d: it requires a live Postgres, which CI does not have
//! (per EN.0.A). `cargo test` reports it as `ignored`, never as `passed` — an
//! honest signal that it did not run. To actually execute it against a live
//! database, opt in explicitly:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p engine-store -- --ignored
//! ```
//!
//! Reaching the test body at all means a developer explicitly requested the
//! `--ignored` run, so an unset `DATABASE_URL` at that point is a hard failure,
//! not a silent skip.
//!
//! The target `events` table is the orchestrator's existing schema (contract §4):
//! `id uuid, workflow_type varchar(150), data json, task_context json,
//! created_at timestamp, updated_at timestamp`.

use std::collections::HashMap;

use chrono::Utc;
use engine_contract::{EventsRow, NodeRun, NodeRunStatus, TaskContext};
use engine_store::{
    connect, get_event, get_task_context, insert_event, update_event, upsert_event,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-store -- --ignored`"]
async fn insert_then_read_round_trips_an_events_row() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");

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

/// `upsert_event` is idempotent on `id`: an upsert of an already-existing row
/// updates `workflow_type`/`data`/`task_context`/`updated_at` in place while
/// leaving `created_at` untouched, and `get_task_context` reads the updated
/// `task_context` straight back; a missing id maps to `Ok(None)` rather than
/// an sqlx error.
#[tokio::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-store -- --ignored`"]
async fn upsert_event_is_idempotent_and_leaves_created_at_immutable() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");

    let pool = connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let id = Uuid::new_v4();
    let original_created_at = Utc::now();

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

    let row = EventsRow {
        id,
        workflow_type: "engine-store-upsert-test".to_string(),
        data: serde_json::json!({ "ticket_id": "T-upsert" }),
        task_context: TaskContext {
            event: serde_json::json!({ "ticket_id": "T-upsert" }),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs,
        },
        created_at: original_created_at,
        updated_at: original_created_at,
    };

    // First call: no existing row, so this is the insert branch.
    upsert_event(&pool, &row)
        .await
        .expect("upsert_event should succeed on insert");

    // Second call: same id, changed task_context, a later created_at that
    // must be ignored, and a later updated_at that must land.
    let mut changed = row.clone();
    changed.task_context.node_runs.insert(
        "DataIngestionNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(original_created_at),
            completed_at: Some(Utc::now()),
            error: None,
            input: None,
            usage: None,
        },
    );
    changed.created_at = Utc::now(); // deliberately different; must be ignored
    changed.updated_at = Utc::now();

    upsert_event(&pool, &changed)
        .await
        .expect("upsert_event should succeed on conflict update");

    let read_back = get_event(&pool, id)
        .await
        .expect("get_event should find the upserted row");

    assert_eq!(
        read_back.created_at, original_created_at,
        "created_at must stay immutable across an upsert"
    );
    assert_eq!(
        read_back.task_context.node_runs["DataIngestionNode"].status,
        NodeRunStatus::Success,
        "task_context must reflect the second upsert's payload"
    );

    let task_context = get_task_context(&pool, id)
        .await
        .expect("get_task_context should succeed")
        .expect("get_task_context should find the row");
    assert_eq!(
        task_context.node_runs["DataIngestionNode"].status,
        NodeRunStatus::Success
    );

    let missing = get_task_context(&pool, Uuid::new_v4())
        .await
        .expect("get_task_context should not error on a missing id");
    assert!(missing.is_none(), "a missing id must map to Ok(None)");

    // Clean up so repeated local runs don't accumulate rows.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup delete should succeed");
}
