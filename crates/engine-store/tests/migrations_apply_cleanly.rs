//! Proves engine-store's migration tooling (EN.14.E task 2) applies cleanly to a
//! throwaway database created from empty, and that running it a second time is a
//! no-op rather than an error.
//!
//! This test is `#[ignore]`d: it requires a live Postgres capable of `CREATE
//! DATABASE`, which CI does not have (per EN.0.A). `cargo test`/`cargo nextest`
//! reports it as `ignored`, never as `passed` — an honest signal that it did not
//! run. To actually execute it against a live database, opt in explicitly:
//!
//! ```sh
//! DATABASE_URL=postgres://<superuser>@localhost:5432/postgres \
//!   cargo nextest run -p engine-store --run-ignored ignored-only
//! ```
//!
//! `DATABASE_URL` here must name a role with `CREATEDB` (locally, the OS-trust
//! superuser connecting to the `postgres` maintenance database — NOT
//! `orchestration_dev`/`orchestration_sandbox`, whose `orchestration` role has no
//! `CREATEDB` privilege). **The username must be given explicitly in the URL**:
//! `nextest` runs each test in its own process with a scrubbed environment, so
//! sqlx's no-username fallback (which otherwise reads `$USER`/`whoami`) resolves
//! to a role that does not exist rather than the invoking shell's user. The test
//! creates a uniquely-named scratch database, points a second connection at
//! *that* database to run the migrations, then drops the scratch database again
//! — `orchestration_dev` is never touched, and no state from this test outlives
//! the test.

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, PgPool, Row};

/// Assert task 3's `journal` table migration produced exactly the schema
/// stated at `postgres.rs:198-207`: `created_at` is `timestamp` WITHOUT time
/// zone (not `timestamptz`), `detail` is `json` (not `jsonb`), and the
/// `(campaign_id, created_at)` composite index exists so
/// `list_journal_rows_for_campaign`'s query never falls back to a full table
/// scan.
async fn assert_journal_table_and_index_exist(pool: &PgPool) -> Result<(), String> {
    let columns = sqlx::query(
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_name = 'journal'",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("failed to read information_schema.columns for journal: {e}"))?;

    if columns.is_empty() {
        return Err("journal table does not exist after migrating".to_string());
    }

    let mut by_name = std::collections::HashMap::new();
    for row in &columns {
        let name: String = row.try_get("column_name").map_err(|e| e.to_string())?;
        let data_type: String = row.try_get("data_type").map_err(|e| e.to_string())?;
        by_name.insert(name, data_type);
    }

    let expect_type = |col: &str, expected: &str| -> Result<(), String> {
        match by_name.get(col) {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(format!(
                "journal.{col} has type \"{actual}\", expected \"{expected}\""
            )),
            None => Err(format!("journal is missing column \"{col}\"")),
        }
    };

    expect_type("id", "uuid")?;
    expect_type("campaign_id", "text")?;
    expect_type("run_id", "uuid")?;
    expect_type("step", "text")?;
    expect_type("kind", "text")?;
    expect_type("reason", "text")?;
    expect_type("detail", "json")?;
    // The trap this whole block warns about: WITHOUT time zone, not "timestamp
    // with time zone" — matches alembic's sa.DateTime() and the reader's
    // try_get::<NaiveDateTime>.
    expect_type("created_at", "timestamp without time zone")?;

    let index_count: i64 = sqlx::query(
        "SELECT count(*) AS count FROM pg_indexes \
         WHERE tablename = 'journal' AND indexname = 'journal_campaign_created_at_idx'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("failed to query pg_indexes for journal: {e}"))?
    .try_get("count")
    .map_err(|e| e.to_string())?;

    if index_count != 1 {
        return Err(format!(
            "expected exactly one journal_campaign_created_at_idx index, found {index_count}"
        ));
    }

    Ok(())
}

/// Assert EN.14.F task 3's `node_invocations` table migration produced exactly
/// the schema documented in `0002_create_node_invocations.sql`, EXTENDED by
/// EN.14.G task 3's `0003_add_node_invocation_payload.sql`: `started_at`/
/// `completed_at` are `timestamp` WITHOUT time zone (not `timestamptz`),
/// `status` is `text`, `payload` is `json` (not `jsonb` — the same trap
/// `0001_create_journal.sql`'s header already documents for `detail`), and
/// the `(run_id, seq)` index exists so `list_node_invocations_for_run`'s
/// query never falls back to a full table scan. Mirrors
/// [`assert_journal_table_and_index_exist`] exactly.
async fn assert_node_invocations_table_and_index_exist(pool: &PgPool) -> Result<(), String> {
    let columns = sqlx::query(
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_name = 'node_invocations'",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("failed to read information_schema.columns for node_invocations: {e}"))?;

    if columns.is_empty() {
        return Err("node_invocations table does not exist after migrating".to_string());
    }

    let mut by_name = std::collections::HashMap::new();
    for row in &columns {
        let name: String = row.try_get("column_name").map_err(|e| e.to_string())?;
        let data_type: String = row.try_get("data_type").map_err(|e| e.to_string())?;
        by_name.insert(name, data_type);
    }

    let expect_type = |col: &str, expected: &str| -> Result<(), String> {
        match by_name.get(col) {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(format!(
                "node_invocations.{col} has type \"{actual}\", expected \"{expected}\""
            )),
            None => Err(format!("node_invocations is missing column \"{col}\"")),
        }
    };

    expect_type("id", "uuid")?;
    expect_type("run_id", "text")?;
    expect_type("campaign_id", "text")?;
    expect_type("node", "text")?;
    expect_type("seq", "bigint")?;
    // The trap this whole block warns about: WITHOUT time zone, not "timestamp
    // with time zone" — matches alembic's sa.DateTime() and the reader's
    // try_get::<NaiveDateTime>.
    expect_type("started_at", "timestamp without time zone")?;
    expect_type("completed_at", "timestamp without time zone")?;
    expect_type("status", "text")?;
    expect_type("error", "text")?;
    // EN.14.G task 3: `payload` MUST be `json`, not `jsonb` — matching
    // `0001_create_journal.sql`'s `detail` column and `postgres.rs`'s
    // dependence on that exact type.
    expect_type("payload", "json")?;
    expect_type("payload_truncated", "boolean")?;
    expect_type("payload_cap_bytes", "bigint")?;

    let index_count: i64 = sqlx::query(
        "SELECT count(*) AS count FROM pg_indexes \
         WHERE tablename = 'node_invocations' AND indexname = 'node_invocations_run_id_seq_idx'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("failed to query pg_indexes for node_invocations: {e}"))?
    .try_get("count")
    .map_err(|e| e.to_string())?;

    if index_count != 1 {
        return Err(format!(
            "expected exactly one node_invocations_run_id_seq_idx index, found {index_count}"
        ));
    }

    Ok(())
}

/// Assert EN.14.I task 1's `events` table migration
/// (`0004_create_events.sql`) produced exactly the schema recorded in
/// `planning/EN.14.I/live-schema-diff.md`: `data`/`task_context` are `json`
/// (NOT `jsonb`), `created_at`/`updated_at` are `timestamp` WITHOUT time
/// zone, and `data`/`task_context`/`created_at`/`updated_at` are all
/// NULLABLE — matching the live table's measured nullability rather than
/// `EventsRow`'s non-`Option` Rust fields. Mirrors
/// [`assert_journal_table_and_index_exist`] /
/// [`assert_node_invocations_table_and_index_exist`] exactly.
async fn assert_events_table_and_columns_exist(pool: &PgPool) -> Result<(), String> {
    let columns = sqlx::query(
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns \
         WHERE table_name = 'events'",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("failed to read information_schema.columns for events: {e}"))?;

    if columns.is_empty() {
        return Err("events table does not exist after migrating".to_string());
    }

    let mut by_name = std::collections::HashMap::new();
    for row in &columns {
        let name: String = row.try_get("column_name").map_err(|e| e.to_string())?;
        let data_type: String = row.try_get("data_type").map_err(|e| e.to_string())?;
        let is_nullable: String = row.try_get("is_nullable").map_err(|e| e.to_string())?;
        by_name.insert(name, (data_type, is_nullable));
    }

    let expect = |col: &str, expected_type: &str, expected_nullable: &str| -> Result<(), String> {
        match by_name.get(col) {
            Some((actual_type, actual_nullable)) => {
                if actual_type != expected_type {
                    return Err(format!(
                        "events.{col} has type \"{actual_type}\", expected \"{expected_type}\""
                    ));
                }
                if actual_nullable != expected_nullable {
                    return Err(format!(
                        "events.{col} has is_nullable \"{actual_nullable}\", expected \"{expected_nullable}\""
                    ));
                }
                Ok(())
            }
            None => Err(format!("events is missing column \"{col}\"")),
        }
    };

    expect("id", "uuid", "NO")?;
    expect("workflow_type", "character varying", "NO")?;
    // `json`, NOT `jsonb` — `get_task_context` reads `try_get::<Json<TaskContext>>`
    // and `list_orphan_candidates` depends on the `->` operator working on
    // `json` directly (postgres.rs). A `jsonb` column compiles here and then
    // diverges at read time.
    expect("data", "json", "YES")?;
    expect("task_context", "json", "YES")?;
    // The trap this whole block warns about: WITHOUT time zone, not "timestamp
    // with time zone" — matches the live schema and the reader's
    // try_get::<NaiveDateTime>.
    expect("created_at", "timestamp without time zone", "YES")?;
    expect("updated_at", "timestamp without time zone", "YES")?;

    Ok(())
}

/// Assert that `events`, `journal` and `node_invocations` are ALL present, as a
/// set, queried from `information_schema.tables` — the block's actual
/// deliverable: an engine database stood up from engine-rs's migrations alone,
/// with no Synapse checkout and no alembic anywhere in the path. A missing
/// table names itself in the returned error rather than surfacing only as a
/// downstream column-lookup failure.
async fn assert_events_journal_and_node_invocations_all_present(
    pool: &PgPool,
) -> Result<(), String> {
    const EXPECTED: [&str; 3] = ["events", "journal", "node_invocations"];

    let rows = sqlx::query(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = ANY($1)",
    )
    .bind(EXPECTED.as_slice())
    .fetch_all(pool)
    .await
    .map_err(|e| format!("failed to read information_schema.tables: {e}"))?;

    let present: std::collections::HashSet<String> = rows
        .iter()
        .map(|row| {
            row.try_get::<String, _>("table_name")
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;

    let missing: Vec<&str> = EXPECTED
        .iter()
        .copied()
        .filter(|t| !present.contains(*t))
        .collect();

    if !missing.is_empty() {
        return Err(format!(
            "expected tables {EXPECTED:?} to all exist after migrating from empty, but missing: {missing:?}"
        ));
    }

    Ok(())
}

/// Build the admin connection string this test was configured with (must be able
/// to `CREATE DATABASE`/`DROP DATABASE`), and read back the maintenance database's
/// name so cleanup can reconnect to it.
fn admin_options() -> PgConnectOptions {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");
    PgConnectOptions::from_str(&database_url)
        .expect("DATABASE_URL must parse as a Postgres connection string")
}

#[tokio::test]
#[ignore = "requires a live Postgres with CREATEDB; run with DATABASE_URL set and --run-ignored ignored-only (see file header)"]
async fn migrations_apply_cleanly_to_a_scratch_database_created_from_empty() {
    let admin_opts = admin_options();
    let scratch_db = format!(
        "engine_store_migrate_test_{}",
        uuid::Uuid::new_v4().simple()
    );

    // Guard: never let this test run against the live shared databases.
    assert_ne!(
        scratch_db, "orchestration_dev",
        "scratch database name must never collide with the live shared database"
    );

    let mut admin_conn = PgConnection::connect_with(&admin_opts)
        .await
        .expect("failed to connect to the admin/maintenance database named by DATABASE_URL");

    let create_stmt = format!(r#"CREATE DATABASE "{scratch_db}""#);
    admin_conn
        .execute(AssertSqlSafe(create_stmt))
        .await
        .expect("failed to CREATE DATABASE for the scratch migration target");

    // Run the whole thing in a closure so a failure partway through still lets us
    // drop the scratch database in cleanup below, rather than leaking it.
    let scratch_opts = admin_opts.clone().database(&scratch_db);
    let outcome: Result<(), String> = async {
        let scratch_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(scratch_opts)
            .await
            .map_err(|e| format!("failed to connect to scratch database {scratch_db}: {e}"))?;

        engine_store::run_migrations(&scratch_pool)
            .await
            .map_err(|e| format!("first migration run failed against an empty database: {e}"))?;

        // Running it again against a database that already has every migration
        // applied must be a no-op, not an error.
        engine_store::run_migrations(&scratch_pool)
            .await
            .map_err(|e| format!("second (idempotent) migration run failed: {e}"))?;

        assert_journal_table_and_index_exist(&scratch_pool).await?;
        assert_node_invocations_table_and_index_exist(&scratch_pool).await?;
        assert_events_table_and_columns_exist(&scratch_pool).await?;
        assert_events_journal_and_node_invocations_all_present(&scratch_pool).await?;

        scratch_pool.close().await;
        Ok(())
    }
    .await;

    // Cleanup always runs, whether or not migrations succeeded, so a failing
    // assertion never leaks a scratch database on this machine.
    let terminate_stmt = format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = '{scratch_db}' AND pid <> pg_backend_pid()"
    );
    admin_conn
        .execute(AssertSqlSafe(terminate_stmt))
        .await
        .expect("failed to terminate lingering connections to the scratch database");
    let drop_stmt = format!(r#"DROP DATABASE IF EXISTS "{scratch_db}""#);
    admin_conn.execute(AssertSqlSafe(drop_stmt)).await.expect(
        "failed to DROP DATABASE for the scratch migration target — cleanup must not leak it",
    );

    outcome.expect("migration tooling did not apply cleanly to a scratch database from empty");
}

/// POSITIVE CONTROL, required by the block and by carryover
/// `gate-scope-must-be-shown-capable-of-failing`: prove the three-table
/// assertion above is actually capable of failing, rather than vacuously
/// passing no matter what. Migrates a scratch database to completion (all
/// three tables present), then simulates one migration's effect being
/// absent via a RUNTIME inversion — dropping the `events` table after
/// migrating, rather than committing a red case by deleting a migration
/// file from this gated repo — and asserts
/// [`assert_events_journal_and_node_invocations_all_present`] goes red and
/// NAMES `events` as the missing table.
#[tokio::test]
#[ignore = "requires a live Postgres with CREATEDB; run with DATABASE_URL set and --run-ignored ignored-only (see file header)"]
async fn three_table_assertion_fails_when_a_migrations_effect_is_absent() {
    let admin_opts = admin_options();
    let scratch_db = format!(
        "engine_store_migrate_control_{}",
        uuid::Uuid::new_v4().simple()
    );

    assert_ne!(
        scratch_db, "orchestration_dev",
        "scratch database name must never collide with the live shared database"
    );

    let mut admin_conn = PgConnection::connect_with(&admin_opts)
        .await
        .expect("failed to connect to the admin/maintenance database named by DATABASE_URL");

    let create_stmt = format!(r#"CREATE DATABASE "{scratch_db}""#);
    admin_conn
        .execute(AssertSqlSafe(create_stmt))
        .await
        .expect("failed to CREATE DATABASE for the scratch migration target");

    let scratch_opts = admin_opts.clone().database(&scratch_db);
    let outcome: Result<(), String> = async {
        let scratch_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(scratch_opts)
            .await
            .map_err(|e| format!("failed to connect to scratch database {scratch_db}: {e}"))?;

        engine_store::run_migrations(&scratch_pool)
            .await
            .map_err(|e| format!("migration run failed against an empty database: {e}"))?;

        // Sanity: the full set is present before we simulate the absence.
        assert_events_journal_and_node_invocations_all_present(&scratch_pool).await?;

        // Simulate 0004_create_events.sql's effect being absent — a runtime
        // inversion rather than deleting the migration file, which would
        // commit a red case to a file every later gate runs against.
        sqlx::query("DROP TABLE events")
            .execute(&scratch_pool)
            .await
            .map_err(|e| format!("failed to drop events table for the positive control: {e}"))?;

        let result = assert_events_journal_and_node_invocations_all_present(&scratch_pool).await;
        scratch_pool.close().await;

        match result {
            Ok(()) => Err(
                "expected the three-table assertion to fail once `events` was dropped, but it passed"
                    .to_string(),
            ),
            Err(msg) if msg.contains("events") => Ok(()),
            Err(msg) => Err(format!(
                "assertion failed as expected, but did not name the missing table \"events\": {msg}"
            )),
        }
    }
    .await;

    let terminate_stmt = format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = '{scratch_db}' AND pid <> pg_backend_pid()"
    );
    admin_conn
        .execute(AssertSqlSafe(terminate_stmt))
        .await
        .expect("failed to terminate lingering connections to the scratch database");
    let drop_stmt = format!(r#"DROP DATABASE IF EXISTS "{scratch_db}""#);
    admin_conn.execute(AssertSqlSafe(drop_stmt)).await.expect(
        "failed to DROP DATABASE for the scratch migration target — cleanup must not leak it",
    );

    outcome.expect("positive control did not observe the three-table assertion fail and name the missing table");
}
