# Task Spec — Phase 0, Block B (EN.0.B) — Data-contract serde types + Postgres round-trip

**Status:** Not started · **Last run:** never

## Goal
Implement the serde types for the preserved data-contract seam (`events` row, `TaskContext`, `NodeRun`) and the `engine-store` Postgres read/write layer, proven by a byte-for-byte round-trip test against a captured Python fixture and a live Postgres insert/read.

## Context Pointers
- **Plan:** `planning/master-plan.md` → Phase 0 → **EN.0.B — Data-contract serde types + Postgres round-trip** (authoritative Files / Out of scope / Acceptance criteria), plus the *Architecture / Design Overview → The preserved seam (byte-for-byte)* section for the exact field shapes.
- **Source of truth for the seam:** `orchestrator/docs/data-contract.md` **v1.0.1** — the `events` row columns, the `task_context` JSON shape (`{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`), the `NodeRun` shape (lowercase `pending|running|success|failed` status; ISO-8601 UTC `started_at`/`completed_at`; `error`, `input`, `usage` = `{input_tokens, output_tokens, model}` or null). Any drift here breaks `bastion` (context.md governing principle 4).
- **Depends on EN.0.A** — uses the workspace crates (`engine-contract`, `engine-store`) and the async-runtime/persistence stack chosen in `planning/decisions/D2-async-runtime-choice.md`.
- **Standing rules** (`CLAUDE.md`): every block ships with tests (rule 1); reuse-not-depend on `workflow-engine-rs` token/cost types where audited-reusable (context.md principle 5) — but do not adopt wholesale.
- **CI constraint:** EN.0.A's CI runs `cargo test` with **no Postgres available**. The live-DB round-trip test MUST be gated (skip/no-op when `DATABASE_URL` is unset) so the gated `cargo test` check stays green in CI; the fixture-based round-trip test runs unconditionally.

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- `engine-contract` exposes `TaskContext`, `NodeRun`, and a `NodeRunStatus` enum whose serde representation is lowercase `pending|running|success|failed`; `usage` serializes as `{input_tokens, output_tokens, model}` or `null`.
- `engine-contract` exposes an `EventsRow` with `id`, `workflow_type`, `data`, `task_context`, `created_at`, `updated_at`, its `task_context` field typed as the `TaskContext` above.
- A round-trip test deserializes the captured Python fixture into the Rust types and re-serializes, asserting semantic JSON equality with **no field, casing, or type difference** (field order aside); it also constructs a Rust `TaskContext`/`EventsRow` and asserts the emitted JSON matches the contract shape.
- `engine-store` provides a connection pool plus `insert_event` and `update_event` against the existing `events` table schema, using the EN.0.A persistence stack.
- A live Postgres insert/read round-trip test passes when `DATABASE_URL` points at a database with the `events` table, and is **skipped (not failed)** when `DATABASE_URL` is unset so CI stays green.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass.

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```
<!-- The live Postgres round-trip test runs only when DATABASE_URL is set (e.g.
     DATABASE_URL=postgres://… cargo test -p engine-store); it self-skips otherwise. -->

## Notes
<filled in as work happens>

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
