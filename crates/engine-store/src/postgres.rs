//! Postgres read/write layer for the durable `events` record (contract §4).
//!
//! `engine-rs` is a writer here (unlike `bastion`, which is a read-only observer,
//! per the orchestrator data contract) — this crate is `engine-serve`'s persistence
//! layer for the run state it owns. Built on the D2 persistence stack (`sqlx::PgPool`).

use chrono::{DateTime, NaiveDateTime, Utc};
use engine_contract::{EventsRow, JournalDecisionKind, JournalRow, TaskContext};
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

/// engine-rs's first tracked migration set (`crates/engine-store/migrations/`),
/// embedded at compile time via the `migrate` feature of the workspace's existing
/// `sqlx` dependency (EN.14.E task 2 — see `planning/EN.14.E/spike-fork-9.md` for
/// why `diesel-async` was evaluated and not adopted: `engine-store` already depends
/// on sqlx, so this path adds no new database stack and no second connection pool).
///
/// Apply pending migrations against `pool`. Idempotent: running it again against a
/// database that already has every migration applied is a no-op, not an error.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
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

/// Serialize a [`JournalDecisionKind`] to its snake_case wire string (e.g.
/// `"step_bailed"`) for storage in the `journal`'s `kind` text column. Reuses
/// the enum's own `#[serde(rename_all = "snake_case")]` tag rather than
/// hand-maintaining a parallel string table.
fn journal_kind_to_text(kind: JournalDecisionKind) -> String {
    match serde_json::to_value(kind).expect("JournalDecisionKind always serializes") {
        serde_json::Value::String(s) => s,
        other => unreachable!("JournalDecisionKind must serialize to a string, got {other:?}"),
    }
}

/// Inverse of [`journal_kind_to_text`]: decode a stored `kind` string back
/// into a [`JournalDecisionKind`]. An unrecognized value is an `sqlx::Error`
/// (via `Error::Decode`) rather than a panic, since it can only originate
/// from a row a future/older engine-serve version wrote.
fn journal_kind_from_text(text: &str) -> Result<JournalDecisionKind, sqlx::Error> {
    serde_json::from_value(serde_json::Value::String(text.to_string()))
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Target schema (EN.12.D): `journal (id uuid primary key, campaign_id text,
/// run_id uuid, step text, kind text, reason text, detail json, created_at
/// timestamp)`. A composite index on `(campaign_id, created_at)` is required
/// so [`list_journal_rows_for_campaign`]'s `WHERE campaign_id = $1 ORDER BY
/// created_at ASC` reads back in decision order without a full table scan —
/// e.g. `CREATE INDEX journal_campaign_created_at_idx ON journal
/// (campaign_id, created_at);`. As of EN.14.E task 3 this DDL (columns and
/// index alike) is a real, tracked migration —
/// `crates/engine-store/migrations/0001_create_journal.sql`, applied via
/// [`run_migrations`] — not merely documented in this comment. The `events`
/// table above predates migration tooling and is still schema-documented in
/// comments only, provisioned as a deployment-side step; `journal` is the
/// first table in this crate for which the doc comment and the tracked
/// schema cannot drift apart.
///
/// Insert one durable journal row (EN.12.D). Follows `insert_event`'s shape:
/// a plain `INSERT`, no upsert semantics — journal rows are append-only
/// decision records, never revised in place.
///
/// Callers are expected to self-skip this call entirely when no
/// `DATABASE_URL`/pool is configured (the same discipline `durable.rs`
/// already applies to `EventsRow` writes); this function itself always
/// requires a live `&PgPool`.
pub async fn insert_journal_row(pool: &PgPool, row: &JournalRow) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO journal (id, campaign_id, run_id, step, kind, reason, detail, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(row.id)
    .bind(&row.campaign_id)
    .bind(row.run_id)
    .bind(&row.step)
    .bind(journal_kind_to_text(row.kind))
    .bind(&row.reason)
    .bind(Json(&row.detail))
    .bind(row.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// List a campaign's journal rows ordered by `created_at` ascending, so a
/// campaign's journal reads back in decision order (oldest decision first).
pub async fn list_journal_rows_for_campaign(
    pool: &PgPool,
    campaign_id: &str,
) -> Result<Vec<JournalRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, campaign_id, run_id, step, kind, reason, detail, created_at \
         FROM journal \
         WHERE campaign_id = $1 \
         ORDER BY created_at ASC",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let kind_text: String = row.try_get("kind")?;
            Ok(JournalRow {
                id: row.try_get("id")?,
                campaign_id: row.try_get("campaign_id")?,
                run_id: row.try_get("run_id")?,
                step: row.try_get("step")?,
                kind: journal_kind_from_text(&kind_text)?,
                reason: row.try_get("reason")?,
                detail: row.try_get::<Json<serde_json::Value>, _>("detail")?.0,
                created_at: row.try_get::<NaiveDateTime, _>("created_at")?.and_utc(),
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
