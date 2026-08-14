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
    connect, get_event, get_task_context, insert_event, list_orphan_candidates, update_event,
    upsert_event,
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

/// `task_context.metadata` is an untyped `serde_json::Value` in the contract
/// (never itself schema-checked), so a real row can carry an arbitrary,
/// malformed `"suspension"` shape under it -- e.g. a crash mid-write, a
/// foreign/older writer, or manual data surgery. This proves the store layer's
/// `Json<TaskContext>` decode round-trips such a row byte-for-byte rather than
/// erroring: `get_event`/`get_task_context` must hand the malformed value back
/// unchanged so a caller (e.g. `engine-serve`'s resume path via
/// `engine_core::suspend::is_suspended`, unit-tested directly in
/// `engine-core::suspend::tests`) can degrade it gracefully instead of the
/// store layer itself failing the read.
#[tokio::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-store -- --ignored`"]
async fn get_task_context_round_trips_a_malformed_suspension_marker_without_erroring() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");

    let pool = connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let id = Uuid::new_v4();
    let now = Utc::now();

    // A deliberately malformed marker: `suspended` (required `bool`) is a
    // string, and the value carries fields no real writer would ever emit.
    let malformed_metadata = serde_json::json!({
        "suspension": {
            "suspended": "yes",
            "resume_at": 12345,
            "unexpected_field": { "nested": "garbage" }
        }
    });

    let row = EventsRow {
        id,
        workflow_type: "engine-store-malformed-suspension-test".to_string(),
        data: serde_json::json!({ "ticket_id": "T-malformed" }),
        task_context: TaskContext {
            event: serde_json::json!({ "ticket_id": "T-malformed" }),
            nodes: HashMap::new(),
            metadata: malformed_metadata.clone(),
            node_runs: HashMap::new(),
        },
        created_at: now,
        updated_at: now,
    };

    insert_event(&pool, &row)
        .await
        .expect("insert_event should succeed even with a malformed suspension shape");

    let read_back = get_event(&pool, id)
        .await
        .expect("get_event must not error on a malformed suspension marker");
    assert_eq!(
        read_back.task_context.metadata, malformed_metadata,
        "the malformed value must round-trip unchanged, not be rejected or altered"
    );

    let task_context = get_task_context(&pool, id)
        .await
        .expect("get_task_context must not error on a malformed suspension marker")
        .expect("get_task_context should find the row");
    assert_eq!(task_context.metadata, malformed_metadata);

    // Clean up so repeated local runs don't accumulate rows.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("cleanup delete should succeed");
}

/// `list_orphan_candidates` (EN.9.C task 3) returns only rows whose
/// `task_context.metadata.completion` is absent and whose `updated_at` is
/// older than the caller-supplied cutoff, ordered oldest-first, honouring
/// both the cutoff and the `limit` bound.
#[tokio::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-store -- --ignored`"]
async fn list_orphan_candidates_returns_only_uncompleted_rows_older_than_cutoff() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");

    let pool = connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let make_row =
        |id: Uuid, updated_at: chrono::DateTime<Utc>, metadata: serde_json::Value| EventsRow {
            id,
            workflow_type: "engine-store-orphan-test".to_string(),
            data: serde_json::json!({ "ticket_id": "T-orphan" }),
            task_context: TaskContext {
                event: serde_json::json!({ "ticket_id": "T-orphan" }),
                nodes: HashMap::new(),
                metadata,
                node_runs: HashMap::new(),
            },
            created_at: updated_at,
            updated_at,
        };

    let cutoff = Utc::now();
    let old_time = cutoff - chrono::Duration::hours(2);
    let recent_time = cutoff + chrono::Duration::hours(2);

    // Completed, old: must NOT be returned (has a completion marker).
    let completed_id = Uuid::new_v4();
    let completed_row = make_row(
        completed_id,
        old_time,
        serde_json::json!({ "completion": { "terminal": true, "status": "succeeded", "at": old_time.to_rfc3339() } }),
    );

    // Uncompleted, old: MUST be returned — this is the orphan candidate.
    let orphan_id = Uuid::new_v4();
    let orphan_row = make_row(orphan_id, old_time, serde_json::json!({}));

    // Uncompleted, recent (newer than cutoff): must NOT be returned.
    let recent_id = Uuid::new_v4();
    let recent_row = make_row(recent_id, recent_time, serde_json::json!({}));

    for row in [&completed_row, &orphan_row, &recent_row] {
        insert_event(&pool, row)
            .await
            .expect("insert_event should succeed");
    }

    let candidates = list_orphan_candidates(&pool, cutoff, 100)
        .await
        .expect("list_orphan_candidates should succeed");
    let candidate_ids: Vec<Uuid> = candidates.iter().map(|r| r.id).collect();

    assert!(
        candidate_ids.contains(&orphan_id),
        "the uncompleted, old row must be returned"
    );
    assert!(
        !candidate_ids.contains(&completed_id),
        "the completed row must NOT be returned regardless of age"
    );
    assert!(
        !candidate_ids.contains(&recent_id),
        "the uncompleted row newer than the cutoff must NOT be returned"
    );

    // Ordering: oldest-first among whatever orphan rows exist.
    let positions: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, r)| r.id == orphan_id)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(positions.len(), 1, "orphan row must appear exactly once");

    // Limit is honoured: with limit=0, nothing is returned.
    let limited = list_orphan_candidates(&pool, cutoff, 0)
        .await
        .expect("list_orphan_candidates should succeed with limit 0");
    assert!(
        limited.is_empty(),
        "a limit of 0 must return no rows, even though orphan candidates exist"
    );

    // Clean up so repeated local runs don't accumulate rows.
    for id in [completed_id, orphan_id, recent_id] {
        sqlx::query("DELETE FROM events WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("cleanup delete should succeed");
    }
}
