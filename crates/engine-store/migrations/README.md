# engine-store migrations

engine-rs's first tracked migration directory, applied with `sqlx::migrate!` /
`sqlx::migrate::Migrator` (the `migrate` feature of the workspace's existing `sqlx` dependency —
no new database stack, no second connection pool; see `planning/EN.14.E/spike-fork-9.md` for why
`diesel-async` was evaluated and not adopted).

Files here follow sqlx's `<VERSION>_<description>.sql` naming convention (a leading integer
version, an underscore, a description, `.sql`). A file that does not match this shape (this
README included) is ignored by the migration resolver.

Apply the pending migrations against a database with:

```rust
let pool = engine_store::connect(database_url).await?;
sqlx::migrate!("./migrations").run(&pool).await?;
```

or from the command line with `sqlx-cli`:

```sh
sqlx migrate run --source crates/engine-store/migrations --database-url "$DATABASE_URL"
```

Tests that apply migrations always run against a scratch database created and dropped by the test
itself, never against `orchestration_dev` — see
`crates/engine-store/tests/migrations_apply_cleanly.rs`.
