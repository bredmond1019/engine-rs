-- EN.14.F task 3 — the `node_invocations` table's initial migration revision.
--
-- A row here records a node DISPATCH, not an LLM call: 22 production node
-- bodies dispatch an inner `ClaudeCodeStep` OUTSIDE `node_context`, so the
-- per-call view is `claude_sessions` and this is the per-DISPATCH view. The
-- two are different cardinalities and both are correct — a single dispatch
-- of a node whose body makes three Claude calls contributes one row here and
-- three in `claude_sessions`.
--
-- Schema mirrors the live read/write path added in
-- `crates/engine-store/src/postgres.rs` (`insert_node_invocation` /
-- `list_node_invocations_for_run`).
--
-- Same trap `0001_create_journal.sql` already documents, gotten wrong twice
-- in this initiative:
--   - `started_at`/`completed_at` are `timestamp` WITHOUT time zone (matches
--     alembic's `sa.DateTime()`; the reader does `try_get::<NaiveDateTime>`
--     and then `.and_utc()` in Rust, at `postgres.rs:274`). A `timestamptz`
--     column compiles here and then fails at read time.
--
-- APPEND-ONLY, DELIBERATELY: unlike `upsert_event`'s
-- `INSERT ... ON CONFLICT (id) DO UPDATE` pattern, this table is never
-- revised in place — see `insert_node_invocation`'s doc comment for why
-- `ON CONFLICT (id) DO NOTHING` is the correct idempotency guard here
-- instead.
--
-- `run_id`/`campaign_id` are nullable `TEXT`, matching
-- `NodeInvocation::run_id`/`campaign_id` (`Option<String>` — read off
-- `node_context`'s existing `read_run_id`/`read_campaign_id` metadata
-- readers, which are themselves `Option<String>`, not `Uuid`). `node`,
-- `seq`, `started_at`, `completed_at` and `status` are `NOT NULL`; `error` is
-- nullable (`None` on a `Success` dispatch).

CREATE TABLE node_invocations (
    id UUID NOT NULL PRIMARY KEY,
    run_id TEXT,
    campaign_id TEXT,
    node TEXT NOT NULL,
    seq BIGINT NOT NULL,
    started_at TIMESTAMP NOT NULL,
    completed_at TIMESTAMP NOT NULL,
    status TEXT NOT NULL,
    error TEXT
);

-- Required so a per-run read (`list_node_invocations_for_run`'s
-- `WHERE run_id = $1 ORDER BY seq ASC`) never falls back to a full table scan.
CREATE INDEX node_invocations_run_id_seq_idx ON node_invocations (run_id, seq);
