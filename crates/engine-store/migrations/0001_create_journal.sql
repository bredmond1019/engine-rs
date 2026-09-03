-- EN.14.E task 3 — the `journal` table's initial migration revision.
--
-- Schema is derived from the live, already-shipped read/write path in
-- `crates/engine-store/src/postgres.rs` (`insert_journal_row` /
-- `list_journal_rows_for_campaign`), NOT from `docs/data-contract.md` — see
-- that function's doc comment at :198-207 for the authoritative statement of
-- this DDL and why it must not be re-derived from the contract doc.
--
-- Two traps, both already gotten wrong once in this initiative:
--   - `created_at` is `timestamp` WITHOUT time zone (matches alembic's
--     `sa.DateTime()`; the reader does `try_get::<NaiveDateTime>` and then
--     `.and_utc()` in Rust, at `postgres.rs:274`). A `timestamptz` column
--     compiles here and then fails at read time.
--   - `detail` is `json`, not `jsonb`.
--
-- `JournalRow` (`engine-contract/src/journal.rs`) carries no nullable
-- fields, so every column below is `NOT NULL`.

CREATE TABLE journal (
    id UUID NOT NULL PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    run_id UUID NOT NULL,
    step TEXT NOT NULL,
    kind TEXT NOT NULL,
    reason TEXT NOT NULL,
    detail JSON NOT NULL,
    created_at TIMESTAMP NOT NULL
);

-- Required so `list_journal_rows_for_campaign`'s
-- `WHERE campaign_id = $1 ORDER BY created_at ASC` reads back in decision
-- order without a full table scan.
CREATE INDEX journal_campaign_created_at_idx ON journal (campaign_id, created_at);
