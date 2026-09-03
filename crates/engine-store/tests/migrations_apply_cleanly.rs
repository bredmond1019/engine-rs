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
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

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
