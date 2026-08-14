//! Postgres read/write layer for the durable `events` record (contract §4).
//!
//! `engine-rs` is a writer here (unlike `bastion`, which is a read-only observer,
//! per the orchestrator data contract) — this crate is `engine-serve`'s persistence
//! layer for the run state it owns. Built on the D2 persistence stack (`sqlx::PgPool`).

use chrono::{DateTime, NaiveDateTime, Utc};
use engine_contract::{EventsRow, TaskContext};
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
        created_at: row.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
        updated_at: row.try_get::<NaiveDateTime, _>("updated_at")?.and_utc(),
    })
}

/// Stamp `updated_at = now()` on a mutable `EventsRow` before writing. Small helper
/// so callers don't have to import `chrono::Utc` themselves.
pub fn touch(row: &mut EventsRow) {
    row.updated_at = Utc::now();
}

/// Idempotent upsert of an `events` row by `id`: inserts if absent, otherwise
/// updates `workflow_type`, `data`, `task_context`, and `updated_at`.
///
/// `created_at` is deliberately **excluded** from the `DO UPDATE` set so it stays
/// immutable across repeated upserts of the same id — that immutability is what
/// makes a resumed run's `EventsRow` shape-identical to an uninterrupted run's
/// across process restarts (a resume happening in a fresh `engine-serve` process
/// no longer has any per-process "have I inserted this row yet" state to get out
/// of sync with Postgres; see `durable.rs`'s removal of `seen_runs`).
pub async fn upsert_event(pool: &PgPool, row: &EventsRow) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO events (id, workflow_type, data, task_context, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (id) DO UPDATE \
           SET workflow_type = EXCLUDED.workflow_type, \
               data          = EXCLUDED.data, \
               task_context  = EXCLUDED.task_context, \
               updated_at    = EXCLUDED.updated_at",
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

/// Read a single `events` row by id and return just its `task_context`.
///
/// A missing id maps to `Ok(None)` rather than an `sqlx::Error::RowNotFound`, so
/// callers (the resume path's rehydration) don't need to pattern-match sqlx
/// internals to distinguish "not found" from a real I/O failure. This is also
/// exactly the read `SourcePayload::TaskContextRef { event_id }` will need;
/// wiring that node is deliberately out of this spec.
pub async fn get_task_context(pool: &PgPool, id: Uuid) -> Result<Option<TaskContext>, sqlx::Error> {
    let row = sqlx::query("SELECT task_context FROM events WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(match row {
        Some(row) => Some(row.try_get::<Json<TaskContext>, _>("task_context")?.0),
        None => None,
    })
}

/// List `events` rows that look crash-stranded: no `metadata.completion`
/// marker (task 1's [`engine_core::completion::stamp_completion`] never ran
/// for them) and `updated_at` older than `older_than` — the boot sweep's
/// candidate set (EN.9.C task 3; see `planning/EN.9.C/tasks.md`'s design
/// decision for why deriving status alone cannot substitute for this marker).
///
/// Ordered oldest-first so the sweep reconciles the longest-stranded runs
/// first. `limit` is a hard bound: a first sweep over a long-lived database
/// must not attempt to load an unbounded result set into memory.
///
/// `task_context` is a `json` column (contract §4), not `jsonb`; the `->`
/// operator works on `json` directly in Postgres, so no cast is needed here.
pub async fn list_orphan_candidates(
    pool: &PgPool,
    older_than: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<EventsRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, workflow_type, data, task_context, created_at, updated_at \
         FROM events \
         WHERE task_context->'metadata'->'completion' IS NULL \
           AND updated_at < $1 \
         ORDER BY updated_at ASC \
         LIMIT $2",
    )
    .bind(older_than)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(EventsRow {
                id: row.try_get("id")?,
                workflow_type: row.try_get("workflow_type")?,
                data: row.try_get::<Json<serde_json::Value>, _>("data")?.0,
                task_context: row
                    .try_get::<Json<engine_contract::TaskContext>, _>("task_context")?
                    .0,
                created_at: row.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
                updated_at: row.try_get::<NaiveDateTime, _>("updated_at")?.and_utc(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};

    /// Non-live coverage for the `get_event` decode fix: the orchestrator's `events`
    /// table stores `created_at`/`updated_at` as `timestamp without time zone`
    /// (contract §4), so sqlx yields a `NaiveDateTime` on read. This asserts the
    /// `.and_utc()` conversion used in `get_event` produces the expected
    /// `DateTime<Utc>` without needing a database connection.
    #[test]
    fn naive_datetime_and_utc_round_trips_to_expected_utc_datetime() {
        let naive = NaiveDate::from_ymd_opt(2026, 7, 17)
            .unwrap()
            .and_hms_opt(12, 30, 45)
            .unwrap();

        let converted = naive.and_utc();

        let expected = Utc.with_ymd_and_hms(2026, 7, 17, 12, 30, 45).unwrap();
        assert_eq!(converted, expected);
    }
}
