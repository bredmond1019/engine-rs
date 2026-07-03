# Implementation Report — EN.0.B-data-contract-postgres

**Date:** 2026-07-02
**Plan:** planning/EN.0.B-data-contract-postgres/tasks.md
**Scope:** Full spec

## What Was Built or Changed

- Added `chrono` (serde feature) and `uuid` (v4, serde features) as workspace dependencies, and
  extended the workspace `sqlx` features with `chrono`, `uuid`, `json` — `Cargo.toml`.
- Implemented the core data-contract serde types (`NodeRunStatus`, `Usage`, `NodeRun`,
  `TaskContext`) matching `orchestrator/docs/data-contract.md` v1.0.1 §5–§6 exactly (lowercase
  status casing, `usage` null-or-object, `node_runs`/`nodes` keyed by class name) —
  `crates/engine-contract/src/task_context.rs`, with unit tests for casing and usage
  null/present serialization.
- Implemented `EventsRow` mirroring the `events` table (contract §4: `id`, `workflow_type`,
  `data`, `task_context`, `created_at`, `updated_at`) — `crates/engine-contract/src/events.rs`,
  with a serde_json round-trip unit test.
- Wired both new modules into `engine-contract`'s public API —
  `crates/engine-contract/src/lib.rs`.
- Captured a Python-shaped fixture reflecting the full v1.0.1 contract shape (mixed node
  statuses, present and null `usage`, present and null `input`/`completed_at`) —
  `tests/fixtures/python_task_context.json`.
- Wrote the byte-for-byte seam guard: (a) deserializes the fixture into `EventsRow` and
  re-serializes, asserting semantic JSON equality against the original fixture with no
  field/casing/type drift; (b) constructs an equivalent `EventsRow`/`TaskContext` in Rust and
  asserts the emitted JSON matches the contract shape (top-level keys, `node_runs` entry shape,
  lowercase status, `usage` null-or-object) — `crates/engine-contract/tests/round_trip.rs`.
- Implemented the `engine-store` Postgres layer on the D2 stack (`sqlx::PgPool`): `connect`,
  `insert_event`, `update_event`, and a `get_event` read helper (needed by the round-trip test
  and future `engine-serve` read paths) against the existing `events` table schema —
  `crates/engine-store/src/postgres.rs`. Added the `chrono`/`uuid` deps to
  `crates/engine-store/Cargo.toml` and declared `pub mod postgres;` in
  `crates/engine-store/src/lib.rs`.
- Wrote the gated live Postgres round-trip test: inserts an `EventsRow`, reads it back, updates
  a node's status via `update_event`, re-reads, and cleans up — self-skips (does not fail) when
  `DATABASE_URL` is unset so `cargo test` stays green in EN.0.A's CI (which has no Postgres) —
  `crates/engine-store/tests/postgres_round_trip.rs`.

## Files Created or Modified

| File | Action |
|---|---|
| Cargo.toml | modified (workspace deps: chrono, uuid; sqlx features chrono/uuid/json) |
| crates/engine-contract/Cargo.toml | modified (added chrono, uuid deps) |
| crates/engine-contract/src/lib.rs | modified (declared task_context/events modules, re-exports) |
| crates/engine-contract/src/task_context.rs | created |
| crates/engine-contract/src/events.rs | created |
| crates/engine-contract/tests/round_trip.rs | created |
| tests/fixtures/python_task_context.json | created |
| crates/engine-store/Cargo.toml | modified (added chrono, uuid deps) |
| crates/engine-store/src/lib.rs | modified (declared postgres module, re-exports) |
| crates/engine-store/src/postgres.rs | created |
| crates/engine-store/tests/postgres_round_trip.rs | created |
| Cargo.lock | modified (dependency resolution) |

## Validation Output

**Commands run:**
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

**Results:**
```
$ cargo fmt --check
(no output — clean)

$ cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s

$ cargo test
running 10 tests (engine-contract unit tests) ... ok (10 passed)
running 2 tests (crates/engine-contract/tests/round_trip.rs) ... ok
  - fixture_round_trips_with_no_field_or_casing_drift ... ok
  - rust_constructed_events_row_matches_contract_shape ... ok
running 1 test (engine-core) ... ok
running 1 test (engine-serve) ... ok
running 1 test (engine-store unit tests) ... ok
running 1 test (crates/engine-store/tests/postgres_round_trip.rs)
  - insert_then_read_round_trips_an_events_row ... ok (self-skipped: DATABASE_URL unset,
    reported "ok" because the test returns early per the gating contract)
test result: ok for all suites; 0 failed

$ cargo build --release
    Finished `release` profile [optimized] target(s) in 7.79s
```
Status: PASSED

## Decisions and Trade-offs

- **Always-present-but-null fields on `NodeRun`.** The contract shows `started_at`, `completed_at`,
  `error`, `input`, `usage` as present keys with `null` values when unset (contract §6), not
  omitted keys. Used `#[serde(default)]` (deserialize-tolerant) without `skip_serializing_if`, so
  Rust-emitted JSON always carries these keys — required for the byte-for-byte semantic-equality
  test against the fixture to pass (a missing key is not equal to a `null`-valued key under
  `serde_json::Value` comparison).
- **`chrono::DateTime<Utc>` for all timestamps**, since D2 pinned the persistence stack (`sqlx`)
  but deferred the timestamp type choice to this block ("chrono or time, per D2" — tasks.json task
  1). Chose `chrono` because `sqlx`'s Postgres driver has first-class `chrono` support (the
  `chrono` feature flag, now added to the workspace `sqlx` dependency) for `TIMESTAMPTZ` mapping,
  matching the ISO-8601 UTC representation the contract mandates.
- **`get_event` added beyond the two functions named in the task list** (`insert_event`,
  `update_event`). The task 4 description says the layer should "write/read" against the schema,
  and the gated round-trip test needs a read path to assert the insert/update actually landed —
  added as a third function rather than folding read logic into the test itself, since a future
  `engine-serve` read path will need it too.
- **Untyped `sqlx::query`/`query_as` (not the compile-time-checked `query!`/`query_as!` macros).**
  D2 explicitly deferred the compile-time-checked-macro CI setup (reachable DB or committed
  `.sqlx` cache) to EN.0.B "when `engine-store` actually issues queries" but scoped that decision
  to pinning the dependency, not mandating the macros immediately; using the untyped fallback here
  keeps `cargo build`/`cargo test` working with no Postgres reachable in CI, consistent with the
  EN.0.A CI constraint this spec calls out. Compile-time query checking can be layered in later
  once a `.sqlx` cache or CI database is available.
- **Cleanup delete in the gated live test.** The live round-trip test deletes the row it inserted
  at the end, so repeated local runs against a persistent dev database don't accumulate test rows.

## Follow-up Work

- Compile-time-checked `sqlx::query!`/`query_as!` macros (and a committed `.sqlx` query cache or a
  CI Postgres service) are deferred, per D2's own note that this is a later concern once
  `engine-store` issues real queries against a reachable schema.
- No `events` table migration/DDL was added in this block — `engine-store` targets the existing
  orchestrator-owned schema; a Postgres instance with that schema must exist for the live test to
  actually exercise instead of self-skip.

## git diff --stat

```
 Cargo.lock                        | 219 +++++++++++++++++++++++++++++++++++++-
 Cargo.toml                        |   4 +-
 crates/engine-contract/Cargo.toml |   2 +
 crates/engine-contract/src/lib.rs |  12 ++-
 crates/engine-store/Cargo.toml    |   2 +
 crates/engine-store/src/lib.rs    |   9 +-
 planning/status.md                |   2 +-
 7 files changed, 240 insertions(+), 10 deletions(-)

Untracked (new) files:
 crates/engine-contract/src/events.rs
 crates/engine-contract/src/task_context.rs
 crates/engine-contract/tests/round_trip.rs
 crates/engine-store/src/postgres.rs
 crates/engine-store/tests/postgres_round_trip.rs
 tests/fixtures/python_task_context.json
```
