# Task Spec — Phase 1, Block B (EN.1.B — Router + parallel nodes + validator)

**Status:** Not started · **Last run:** never

## Goal
Port the two execution patterns (runtime routing, parallel fan-out/merge) and the structural correctness guard (the acyclic graph validator) onto the EN.1.A `Node`/`Workflow` core.

## Context Pointers
- **Plan:** `planning/master-plan.md` → Phase 1 → **EN.1.B — Router + parallel nodes + validator** (the only section that governs this spec).
- **Builds on (EN.1.A, already landed):**
  - `crates/engine-core/src/node.rs` — `Node { process(ctx) -> Result<TaskContext, NodeError>, name() }` (`Send + Sync`); `NodeRegistry` (identity → `Box<dyn Node>`, `get`/`contains`/`register`).
  - `crates/engine-core/src/schema.rs` — `WorkflowSchema { workflow_type, start_node, nodes: HashMap<String, NodeConfig> }`; `NodeConfig { identity, connections: Vec<String> }` with `next()` (= `connections[0]`), `start()`, `next_after()`.
  - `crates/engine-core/src/workflow.rs` — `Workflow { registry, schema }`, `Workflow::new(registry, schema)` (**infallible today**), `run(event, on_progress) -> Result<TaskContext, WorkflowError>`, the `node_context` envelope, `OnProgress = Box<dyn FnMut(&TaskContext)>`.
  - `engine-contract` — `TaskContext`, `NodeRun`, `NodeRunStatus`.
- **Routing semantics (D42 / master-plan):** non-routers walk `connections[0]`; routers pick their next node at runtime via `route(ctx)`. Retry/back-edges are **deliberately undeclared** as connections so the acyclic validator passes — *declared-acyclic, runtime-cyclic*. `route()` may return any registered node identity, including one not in the router's declared `connections`.
- **Validator semantics:** BFS reachability from `start_node` (every declared node reachable); DFS cycle check over declared connections that **skips edges out of router nodes**; only routers may declare >1 connection.
- **`ParallelNode` semantics:** deep-copy (`clone`) the `TaskContext` per branch, run each branch, merge each branch's `nodes` + `node_runs` back into the parent with **last-write-wins**.
- **CLAUDE.md standing rules:** every block ships tests (rule 1); decisions append-only (rule 4); sequence not calendar (rule 3).

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- A fixture workflow with a **router** plus an **undeclared retry back-edge** passes validation *and* executes the retry path correctly at runtime (routes back to re-run an earlier node under a runtime condition, then forward to completion — no infinite loop).
- A fixture **`ParallelNode`** fan-out/merge test confirms last-write-wins: two branches writing the same `nodes`/`node_runs` key resolve to a deterministic winner, and disjoint keys from every branch are all present after merge.
- A fixture **cyclic (non-router) workflow** — a declared connection edge forming a cycle between plain nodes — is **rejected** by the validator; a non-router declaring >1 connection is also rejected.
- Router-aware selection does not regress EN.1.A behavior: a non-router node still walks `connections[0]`; the existing `crates/engine-core/tests/workflow_runner.rs` still passes **unchanged** (the validator is additive — do not change `Workflow::new`'s existing signature; add a validating constructor instead).
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
- **Out of scope (hard boundary):** the trigger/dispatch HTTP path, dual-registry, and serve embedding (EN.1.C); the Claude Code step node (EN.2.A). This block stays a pure library-level engine addition.
- **Do not break the EN.1.A seam.** Keep `Workflow::new(registry, schema)` infallible so `tests/workflow_runner.rs` and the in-file EN.1.A unit tests compile unchanged; expose validation through a **new** fallible constructor (e.g. `Workflow::new_validated(...) -> Result<Self, ValidationError>`) or a standalone `WorkflowValidator::validate(&registry, &schema)` the caller runs first.
- Prefer `std::thread::scope` for `ParallelNode` fan-out (nodes are `Send + Sync`, no new runtime dependency needed). Only add a thread-pool crate to `engine-core/Cargo.toml` if `std::thread::scope` proves insufficient — and if so, that manifest edit belongs to the parallel task alone.

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
