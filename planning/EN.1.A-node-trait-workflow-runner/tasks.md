# Task Spec — Phase 1, Block A (EN.1.A — Node trait + Workflow runner)

**Status:** Not started · **Last run:** never

## Goal
Port the engine's execution core to idiomatic Rust: a `Node` trait plus a `Workflow` pointer-walk runner that seeds all nodes PENDING, walks node-to-node inside a framework-owned envelope, and stamps RUNNING → SUCCESS/FAILED + timing on each `NodeRun`.

## Context Pointers
- **Plan:** `planning/master-plan.md` → Phase 1 → **EN.1.A — Node trait + Workflow runner** (the only section that governs this spec).
- **Consumes (EN.0.B, already landed):** `crates/engine-contract/src/task_context.rs` — `TaskContext { event, nodes, metadata, node_runs }`, `NodeRun { status, started_at, completed_at, error, input, usage }`, `NodeRunStatus { Pending|Running|Success|Failed }`. Node identity = the implementing type's name, used as the map key in both `TaskContext::nodes` and `TaskContext::node_runs` (contract §1).
- **Contract source of truth:** `orchestrator/docs/data-contract.md` v1.0.1 — the seam stays byte-for-byte; do not drift `NodeRun`/`TaskContext` shape.
- **Shared surface (defines here, implemented by EN.1.C):** the `on_progress` callback is the injected persistence seam; this block only *defines its signature* and calls it at node boundaries — it does **not** wire Postgres.
- **CLAUDE.md standing rules:** every block ships tests (rule 1); decisions are append-only (rule 4); work the sequence (rule 3).
- **Runtime/persistence stack:** tokio + sqlx, per `planning/decisions/D2-async-runtime-choice.md`; timestamps use `chrono` (already the workspace dep the contract types serialize with).

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- A fixture 3-node **linear** workflow runs end-to-end through `Workflow::run`, producing a `TaskContext` whose `nodes`/`node_runs` match the expected shape at each step.
- Every node's `NodeRun` transitions PENDING → RUNNING → (SUCCESS | FAILED); `started_at` is set on entry and `completed_at` on exit; a node that returns an error lands FAILED with `error` populated and halts the walk.
- All nodes are seeded PENDING (and the initial snapshot emitted via `on_progress`) **before** the first node runs; `on_progress` is invoked again at every node boundary.
- Node identity (the type-name registry key) is the same string used as the key in `TaskContext::nodes` and `TaskContext::node_runs`.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass on a clean checkout.

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```
<!-- Standard project checks from planning/harness.json (validation.checks[]). -->

## Notes
- Router/parallel-node logic and the acyclic validator are **EN.1.B** — out of scope here. The runner walks `connections[0]` (the single next node) only; no `route(ctx)` branching yet.
- Trigger/dispatch HTTP path, dual-registry, and serve embedding are **EN.1.C** — out of scope. `on_progress` is defined as a signature/seam here, not implemented against `engine-store`.
- Event-schema parsing: parse the triggering event against a `WorkflowSchema` (this block) — the schema type carries the node config + start node; full dual-registry resolution is EN.1.C.

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
