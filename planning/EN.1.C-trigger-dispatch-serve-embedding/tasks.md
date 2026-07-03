---
type: Plan
title: "Task Spec — Phase 1, Block C (Trigger/dispatch + dual-registry + serve embedding)"
description: Decomposed task spec for EN.1.C — embed the engine in bastion serve with dual-registry dispatch, the HTTP surface, in-memory live run state, and the async durable-write seam.
doc_id: en-1-c-tasks
layer: [engine, console]
project: engine-rs
status: draft
keywords: [dispatch, dual-registry, http surface, live state, durable write, bastion serve]
related: [master-plan, status]
---

# Task Spec — Phase 1, Block C (Trigger/dispatch + dual-registry + serve embedding)

**Status:** Not started · **Last run:** never

## Goal
Embed the engine in `bastion serve`: dual-registry dispatch keyed by `workflow_type`, the four-endpoint HTTP surface, in-memory live run state the local Console reads directly (no DB poll), and the async durable-write that implements EN.1.A's `on_progress` seam against `engine-store`.

## Context Pointers
- **Master plan:** `planning/master-plan.md` → Phase 1 → **EN.1.C** (What / Why / Files / Out of scope / Acceptance criteria). Transport architecture + preserved-seam sections in the plan's overview are the design rationale (D42): local reads in-memory, DB as durable record, serve as the seam remote observers subscribe to.
- **Existing surface it builds on:**
  - `crates/engine-core/src/workflow.rs` — `Workflow::run(event, on_progress)`, the `OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>` seam (already seeds all nodes PENDING and emits the initial snapshot before the first node; re-emits at every boundary). EN.1.C wires a durable writer into this seam; the seam signature itself should not need to change.
  - `crates/engine-core/src/schema.rs` — `WorkflowSchema { workflow_type, start_node, nodes }`, `NodeConfig { identity, connections }`.
  - `crates/engine-core/src/node.rs` — `Node`/`NodeRegistry`.
  - `crates/engine-store/src/postgres.rs` — `connect`, `insert_event`, `update_event`, `get_event`, `touch` on `sqlx::PgPool` (D2). `engine-serve` is the *writer* here.
  - `crates/engine-contract` — `EventsRow`, `TaskContext`, `NodeRun`; the EN.0.B byte-for-byte round-trip test (`crates/engine-contract/tests/round_trip.rs`) is the byte-identity oracle to reuse.
- **Crate to grow:** `crates/engine-serve/` (currently a stub `lib.rs` + `crate_name()`), which already depends on `engine-core`, `engine-contract`, `engine-store`, `tokio`, `serde`, `serde_json`.
- **Standing rules (`CLAUDE.md`):** every block ships tests (rule 1); decisions are append-only new files in `planning/decisions/` (rule 4); the HTTP-framework choice is load-bearing and gets its own decision file, linked from `planning/decisions/index.md`.
- **Harness gates:** `planning/harness.json` → fmt, clippy `-D warnings`, test, build --release — all must stay green after every task.

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- A dual-registry (`workflow_registry` + `schema_registry`) dispatch resolves a fixture `workflow_type` to a runnable `Workflow`; an unregistered `workflow_type` is rejected with a 422-equivalent typed error (surfaced as HTTP 422 by the `POST /events/` endpoint).
- The four HTTP endpoints exist and behave: `POST /events/` (requires a valid `X-API-Key`; triggers dispatch; 422 on unregistered type; 401/403 on missing/bad key), `GET /health` (200), `GET /workflows` (lists registered workflow types), `GET /workflows/{type}/graph` (returns the schema/graph for a registered type, 404 for an unknown one).
- An in-memory live run-state store records the latest `TaskContext` snapshot per run at every node boundary and exposes a local read API that returns live state **without any Postgres query** (the local-Console read path).
- The async durable-write, driven by the `on_progress` seam, writes an `events` row that is **byte-identical** (per the EN.0.B round-trip oracle) to what the Python orchestrator would write for an equivalent run: all nodes seeded PENDING and persisted before the first node runs, then re-persisted at every boundary. The Postgres portion self-skips (not fails) when `DATABASE_URL` is unset, keeping CI green.
- A local integration test triggers a fixture workflow through the dispatch path, asserts the local read sees live in-memory state with no DB poll, asserts the durable row is byte-identical, and asserts an unregistered `workflow_type` returns 422.
- The HTTP-framework choice is recorded in a new `planning/decisions/` file and linked from the index.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass clean.

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

## Notes
- **Async durable-write pattern.** The `OnProgress` seam is a **synchronous** `FnMut(&TaskContext)`, but Postgres writes are async. Bridge with a channel: `on_progress` clones the snapshot and sends it on an `mpsc` sender; a background tokio task drains the channel and performs `insert_event` (first snapshot) / `update_event` (subsequent) against `engine-store`. This keeps writes off the run's hot path and keeps `workflow.rs`'s seam signature unchanged — prefer this over changing `Workflow::run`'s signature. Only touch `workflow.rs` if a genuine signature gap surfaces (append-only if so).
- **HTTP framework.** `axum` is the natural tokio-native choice (aligns with D2's tokio runtime) and its `tower`/`oneshot` test harness makes endpoint tests cheap. Confirm and record in the decision file; if a different framework is chosen, update the file's rationale accordingly.
- **Node identity = class/type name** is the join key across `nodes`/`node_runs`; preserve it when mapping `TaskContext` → `EventsRow`.

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
