# Task Spec — Phase 2, Block 0 (EN.2.0 — Async `Node` trait)

**Status:** Done · **Last run:** 2026-07-03 (PASS)

## Goal
Make `engine-core`'s `Node::process` an `async fn` (via `async-trait`), converting the workflow
runner and `ParallelNode` fan-out to async and dropping `engine-serve`'s `web::block` seam, with no
behavior change.

## Context Pointers
- Master-plan block: `planning/master-plan.md` → Phase 2 → **EN.2.0 — Async `Node` trait**.
- Decision: `planning/decisions/D5-async-node-trait.md` (async-trait choice, sync `Router`/`OnProgress`,
  `join_all` fan-out, `web::block` removal rationale).
- Full blast-radius map + Rust/Python comparison: `planning/async-node/notes.md`.
- Standing rule (`CLAUDE.md` #1): every block ships with tests — here the *existing* suite is the
  regression guard; behavior must be identical, so no new test logic is required, but every fixture
  and test harness must be migrated so the suite still exercises the async paths.
- Key source: `crates/engine-core/src/{node.rs,workflow.rs,parallel.rs,routing.rs,validate.rs}`,
  `crates/engine-serve/src/{http.rs,dispatch.rs,durable.rs}`, and the `tests/` dirs of both crates.

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- `Node::process` is `async fn` (via `#[async_trait::async_trait]`); `name`/`as_router` stay sync.
- `Workflow::run` and `node_context` are `async`; the pointer-walk loop is otherwise unchanged.
- `ParallelNode` fans out via `futures::future::join_all` (no `std::thread::scope`); the
  last-write-wins merge semantics are unchanged.
- `Router::route` and `OnProgress` remain synchronous; `OnProgress`'s type signature is unchanged.
- `web::block` is **gone** from `crates/engine-serve/src/http.rs`; `post_events` `.await`s
  `workflow.run(...)` directly.
- All pre-existing tests pass with identical behavior, including
  `crates/engine-serve/tests/dispatch_integration.rs` under the async runner.
- All four gated checks pass: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`,
  `cargo build --release`.

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

## Notes
- The trait-signature change is compile-coupled across `engine-core` **and** `engine-serve` (the
  latter depends on the former's `Node`), so the conversion of all `.rs` sites lands in a single
  task (Task 2) — splitting it further would leave the workspace non-compiling between tasks, which
  every task is required to avoid. Task 1 (dependency additions only) is separable because an added-
  but-unused dependency leaves all four gates green.

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
2026-07-03 [task 2] `validator.rs`'s `router_route_dispatch_matches_runner_behavior` and `cyclic_non_router_workflow_is_rejected_by_new_validated` were left as plain `#[test]` rather than migrated to `#[tokio::test]`, since neither test calls `.process()`/`.run()` and both remain valid under the sync `WorkflowValidator`/`Router::route` surface — a narrower reading of "every fixture and test harness must be migrated" than the spec's Context Pointers implied, kept for a minimal diff.
