-- EN.14.I task 1 — engine-rs's own `events` table.
--
-- Per brain D84 Amendment 1: `events` is the workflow-dispatch table for BOTH
-- runtimes. engine-rs does NOT take over Synapse's `events` table — instead
-- each runtime gets its own table, under the same name, in its own database.
-- Synapse keeps its own `events` for its four Brain workflows
-- (`DOCUMENT_INGEST`, `DOCUMENT_QA`, `MEMORY_INGEST`, `MEMORY_CONSOLIDATION` —
-- see `core/synapse/app/api/schema_registry.py:9-14` and
-- `app/worker/tasks.py::process_incoming_event`). This migration exists so an
-- engine database can be stood up from engine-rs's own migrations alone — no
-- Synapse checkout, no alembic anywhere in the path.
--
-- SCHEMA IS DIFFED FROM THE LIVE DATABASE, NOT AUTHORED FROM
-- `docs/data-contract.md` — see `planning/EN.14.I/live-schema-diff.md`
-- (captured 2026-09-07 against `orchestration_dev`) for the exact `psql`
-- commands and their output. Two things the contract document gets wrong or
-- omits:
--
--   - `data` and `task_context` are `json`, NOT `jsonb`. `get_task_context`
--     (`postgres.rs`) reads `try_get::<Json<TaskContext>>` and
--     `list_orphan_candidates`'s own doc comment states the dependency
--     outright: the `->` operator works on `json` directly in Postgres, so
--     no cast is needed there. A `jsonb` column compiles here and then
--     diverges at read time.
--
--   - NULLABILITY is the divergence nobody named before this measurement:
--     `data`, `task_context`, `created_at` and `updated_at` are all
--     NULLABLE in the live table, even though `EventsRow`
--     (`engine-contract/src/events.rs`) types all six fields as non-`Option`.
--     This is a DELIBERATE choice made against the measured schema, not an
--     oversight: writing `NOT NULL` here would reject rows that the
--     operator's dump-and-restore (`OP.database-brain-engine-split`) will
--     carry across from the live table. Every write path in
--     `crates/engine-store/src/postgres.rs` always supplies a value for
--     these columns, so the wider nullability is inert for engine-rs's own
--     writers and only matters for rows migrated in from elsewhere.
--
--   - `created_at`/`updated_at` are `timestamp` WITHOUT time zone (matches
--     the live schema and the same trap `0001_create_journal.sql` and
--     `0002_create_node_invocations.sql` already document: the reader does
--     `try_get::<NaiveDateTime>` and then `.and_utc()` in Rust). A
--     `timestamptz` column compiles here and then fails at read time.
--
--   - `workflow_type` is `VARCHAR(150)`, matching the live table's
--     `character_maximum_length`.
--
-- The live table's only index is `events_pkey`, a unique btree on `id` — no
-- additional indexes are created here.

CREATE TABLE events (
    id UUID NOT NULL PRIMARY KEY,
    workflow_type VARCHAR(150) NOT NULL,
    data JSON,
    task_context JSON,
    created_at TIMESTAMP,
    updated_at TIMESTAMP
);
