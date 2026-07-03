# Documentation Report — EN.0.B-data-contract-postgres

**Date:** 2026-07-02
**Spec:** planning/EN.0.B-data-contract-postgres/tasks.md
**Verdict gate:** PASS (confirmed)

## Docs Patched
| Doc File | Section Updated | Change Summary |
|---|---|---|

None. `docs/architecture.md` is the only doc referencing the changed source (module map,
`Core Types`, `Build & CI`), and it is a top-level architecture/overview doc — per instruction 6
it is flagged for human review below rather than edited directly.

## Docs Flagged NEEDS_REVIEW

- **`docs/architecture.md`** — this block landed real implementations that the doc still
  describes as stubs:
  - `Module Map`: `engine-contract` and `engine-store` no longer hold trivial stub
    `src/lib.rs` files. `engine-contract` now exposes `task_context.rs` (`NodeRunStatus`,
    `Usage`, `NodeRun`, `TaskContext`) and `events.rs` (`EventsRow`), plus
    `tests/round_trip.rs` and `tests/fixtures/python_task_context.json`. `engine-store` now
    exposes `postgres.rs` (`connect`, `insert_event`, `update_event`, `get_event`) and
    `tests/postgres_round_trip.rs` (gated on `DATABASE_URL`).
  - `Core Types` section (currently marked `<!-- Stub -->`) should be updated to reflect the
    concrete field lists now implemented: `NodeRunStatus` (`pending|running|success|failed`,
    lowercase serde), `Usage` (`{input_tokens, output_tokens, model}`, null-or-object),
    `NodeRun` (adds always-present-but-nullable `started_at`, `completed_at`, `error`,
    `input`, `usage`), `TaskContext`, and the new `EventsRow` (`id`, `workflow_type`, `data`,
    `task_context`, `created_at`, `updated_at`) mirroring the `events` table.
  - `Build & CI` section states persistence deps are used by `engine-store`/`engine-serve` but
    doesn't yet mention that `engine-store` now issues real `sqlx` queries (untyped
    `query`/`query_as`, not compile-time-checked macros — deferred per this block's
    trade-off notes) against the existing `events` schema, or that `chrono`/`uuid` were added
    as workspace + per-crate dependencies.
  - Consider adding a short note that the live Postgres round-trip test in `engine-store`
    self-skips (not fails) when `DATABASE_URL` is unset, matching the CI-green constraint from
    EN.0.A.

## Docs Clean (checked, no changes needed)

- `docs/cli.md` — still accurately scoped as a stub; this block added no CLI/binary surface.
- `docs/index.md` — navigation index unchanged; no new doc files were created in this block.
