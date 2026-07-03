//! Postgres read/write layer for the durable `events` record (contract §4).
//!
//! `engine-rs` is a writer here (unlike `bastion`, which is a read-only observer,
//! per the orchestrator data contract) — this crate is `engine-serve`'s persistence
//! layer for the run state it owns. Built on the D2 persistence stack (`sqlx::PgPool`).

use chrono::Utc;
use engine_contract::EventsRow;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Build a connection pool against `database_url`. Callers own the pool's lifetime
/// (typically one pool per `engine-serve` process).
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}

/// Insert a new `events` row. Existing schema (contract §4): `id`, `workflow_type`,
/// `data`, `task_context`, `created_at`, `updated_at`.
pub async fn insert_event(pool: &PgPool, row: &EventsRow) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO events (id, workflow_type, data, task_context, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(row.id)
    .bind(&row.workflow_type)
    .bind(Json(&row.data))
    .bind(Json(&row.task_context))
    .bind(row.created_at)
    .bind(row.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update the mutable fields of an existing `events` row by id: `data`,
/// `task_context`, and `updated_at` (advances at every node boundary, per contract
/// §4). `id` and `created_at` are immutable once inserted.
pub async fn update_event(pool: &PgPool, row: &EventsRow) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE events SET data = $2, task_context = $3, updated_at = $4 WHERE id = $1")
        .bind(row.id)
        .bind(Json(&row.data))
        .bind(Json(&row.task_context))
        .bind(row.updated_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Read a single `events` row by id. Used by the round-trip test and by future
/// `engine-serve` read paths.
pub async fn get_event(pool: &PgPool, id: Uuid) -> Result<EventsRow, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, workflow_type, data, task_context, created_at, updated_at FROM events WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    Ok(EventsRow {
        id: row.try_get("id")?,
        workflow_type: row.try_get("workflow_type")?,
        data: row.try_get::<Json<serde_json::Value>, _>("data")?.0,
        task_context: row
            .try_get::<Json<engine_contract::TaskContext>, _>("task_context")?
            .0,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Stamp `updated_at = now()` on a mutable `EventsRow` before writing. Small helper
/// so callers don't have to import `chrono::Utc` themselves.
pub fn touch(row: &mut EventsRow) {
    row.updated_at = Utc::now();
}
