# Review Report — EN.0.B-data-contract-postgres

**Date:** 2026-07-02
**Spec:** planning/EN.0.B-data-contract-postgres/tasks.md
**Scope:** Full spec
**Verdict:** PASS

## Acceptance Criteria Check
| Criterion | Status | Evidence |
|---|---|---|
| `engine-contract` exposes `TaskContext`, `NodeRun`, and a `NodeRunStatus` enum whose serde representation is lowercase `pending\|running\|success\|failed`; `usage` serializes as `{input_tokens, output_tokens, model}` or `null` | MET | `crates/engine-contract/src/task_context.rs:16-53` (`#[serde(rename_all = "lowercase")]` on `NodeRunStatus`; `Usage` struct; `NodeRun.usage: Option<Usage>`), verified by unit tests `node_run_status_serializes_lowercase`, `node_run_status_deserializes_lowercase`, `node_run_status_rejects_unknown_casing`, `usage_serializes_object_when_present`, `usage_serializes_null_when_absent` (all pass) |
| `engine-contract` exposes an `EventsRow` with `id`, `workflow_type`, `data`, `task_context`, `created_at`, `updated_at`, `task_context` typed as `TaskContext` | MET | `crates/engine-contract/src/events.rs:14-22`; verified by `events_row_has_contract_top_level_fields` and `events_row_round_trips_through_serde_json` |
| Round-trip test deserializes the captured Python fixture and re-serializes with no field/casing/type drift; also constructs a Rust `TaskContext`/`EventsRow` and asserts emitted JSON matches contract shape | MET | `crates/engine-contract/tests/round_trip.rs` — `fixture_round_trips_with_no_field_or_casing_drift` compares `serde_json::Value` of fixture vs. re-serialized `EventsRow`; `rust_constructed_events_row_matches_contract_shape` asserts top-level/task_context/node_runs shape, lowercase status, usage null-or-object. Fixture at `tests/fixtures/python_task_context.json` covers mixed statuses, null and present `usage`/`input`/`completed_at`. Both tests pass. |
| `engine-store` provides a connection pool plus `insert_event` and `update_event` against the existing `events` table schema, using the EN.0.A persistence stack | MET | `crates/engine-store/src/postgres.rs:14-49` — `connect` (sqlx `PgPoolOptions`), `insert_event`, `update_event` against the 6-column `events` schema (contract §4), built on `sqlx::PgPool` per D2 |
| A live Postgres insert/read round-trip test passes when `DATABASE_URL` points at a database with the `events` table, and is skipped (not failed) when `DATABASE_URL` is unset so CI stays green | MET | `crates/engine-store/tests/postgres_round_trip.rs:26-29` — early return with `eprintln!` when `DATABASE_URL` is unset (test reports "ok", not skipped-as-ignored, but does not fail); confirmed in fresh `cargo test` run — no `DATABASE_URL` set locally, test passed as a self-skip |
| `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass | MET | Fresh re-run of all four gating checks — all passed (see below) |

## Fresh Test Results
```
$ cargo fmt --check
(no output — clean, exit 0)

$ cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.08s
(exit 0, no warnings)

$ cargo test
running 10 tests (engine-contract unit tests) ... 10 passed
running 2 tests (engine-contract/tests/round_trip.rs) ... 2 passed
running 1 test (engine-core) ... 1 passed
running 1 test (engine-serve) ... 1 passed
running 1 test (engine-store unit tests) ... 1 passed
running 1 test (engine-store/tests/postgres_round_trip.rs) ... 1 passed
  (insert_then_read_round_trips_an_events_row self-skipped: DATABASE_URL unset)
Doc-tests: 0 tests across all 4 crates
Total: 16 tests, 0 failed

$ cargo build --release
    Finished `release` profile [optimized] target(s) in 0.10s (exit 0)
```

All four gating checks (`fmt`, `clippy`, `test`, `build` — all `gates: true` in
`planning/harness.json`) pass fresh.

## Verdict: PASS
All acceptance criteria are fully met by the current code, and every fresh gating check
(`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`) passes
with exit 0. The data-contract types (`NodeRunStatus`, `Usage`, `NodeRun`, `TaskContext`,
`EventsRow`) match `orchestrator/docs/data-contract.md` v1.0.1 §4-§6 field-for-field, including
lowercase status casing and the always-present-but-nullable `NodeRun` fields required for
byte-for-byte semantic equality against the captured fixture. `engine-store`'s `connect`,
`insert_event`, and `update_event` are implemented on the D2-pinned `sqlx::PgPool` stack against
the existing `events` schema, and the live Postgres round-trip test correctly self-skips (not
fails) when `DATABASE_URL` is unset, keeping CI green per the EN.0.A constraint. No standing-rule
violations (test coverage, OKF frontmatter, identity integrity) were found.

## Issues Found
None.

## Next Steps
Proceed to `/document` to update project docs, then `/log-work` to close out the block. The
implement report's follow-up notes (compile-time-checked `sqlx::query!`/`query_as!` macros with a
`.sqlx` cache or CI Postgres service, and an `events` table migration/DDL) are deferred, non-blocking
items appropriately scoped to a later block.
