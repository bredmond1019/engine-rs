-- EN.14.G task 3 — retained-payload columns on `node_invocations`.
--
-- Extends the table `0002_create_node_invocations.sql` created: the block's
-- job is to make one attempt's dispatch output distinguishable from
-- another's, which requires actually retaining the payload, not just the
-- fact that a dispatch happened.
--
-- Same trap this initiative has already gotten wrong twice, restated here
-- for a THIRD column type — matches `0001_create_journal.sql`'s `detail`
-- and `postgres.rs:142`'s dependence on that choice:
--   - `payload` is `json`, NOT `jsonb`. A `jsonb` column compiles here and
--     silently diverges from the live schema this migration must match.
--
-- `payload_truncated`/`payload_cap_bytes` are `NOT NULL` with defaults so a
-- pre-EN.14.G row (which never wrote these columns) still reads back as an
-- interpretable "no cap was ever recorded for this row" rather than NULL —
-- mirroring `NodeInvocation`'s own `#[serde(default)]` forward-tolerance on
-- these same three fields.

ALTER TABLE node_invocations
    ADD COLUMN payload JSON,
    ADD COLUMN payload_truncated BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN payload_cap_bytes BIGINT NOT NULL DEFAULT 0;
