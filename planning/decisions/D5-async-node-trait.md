---
type: Decision
title: "D5: Async Node Trait"
description: Makes engine-core's Node::process an async fn via the async-trait crate, converting the workflow runner and parallel fan-out to async while keeping Router::route and the OnProgress seam synchronous.
doc_id: D5-async-node-trait
layer: [engine]
project: engine-rs
status: active
keywords: [async, async-trait, Node trait, ParallelNode, join_all, tokio, concurrency]
related: [decisions-index, master-plan, D2-async-runtime-choice, async-node]
---

# D5 — Async `Node` Trait

**Decided:** 2026-07-03
**Status:** Accepted

## Context

`engine-core`'s node/workflow execution is fully synchronous today — a faithful port of the
Python `orchestrator`, which is also synchronous at the node level (its concurrency comes from
Celery worker *processes* and thread pools, not `asyncio`). See `planning/async-node/notes.md`
for the full Rust/Python comparison and blast-radius map.

EN.2.A introduces `ClaudeCodeStep`, which spawns a Claude Code subprocess and awaits its
completion — an inherently async, I/O-bound operation. With a synchronous `Node::process`, that
node would have to block a whole OS thread (via `web::block`/`spawn_blocking`) for the entire
session, the same ceiling Python has. Making the trait async is the load-bearing lever that lets
concurrent runs and I/O-bound parallel branches share the Tokio runtime instead of each claiming a
dedicated thread — the one place Rust can structurally exceed the Python path (which can't easily
retrofit async through `pydantic_ai`'s sync integration).

This is settled as its own decision (and its own block, `EN.2.0`) rather than falling out of the
EN.2.A transport choice, because changing the trait signature ripples through every node
implementation and all of Phase 1 (`Router`, `ParallelNode`, the `OnProgress` seam).

## Decision

1. **`Node::process` becomes `async fn`, dispatched via the `async-trait` crate.**
   ```rust
   #[async_trait::async_trait]
   pub trait Node: Send + Sync {
       async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError>;
       fn name(&self) -> &str;                          // stays sync
       fn as_router(&self) -> Option<&dyn Router> { None } // stays sync
   }
   ```
   `async-trait` is required because native `async fn` in traits is not `dyn`-safe on stable, and
   `Box<dyn Node>` is load-bearing across `NodeRegistry`, `ParallelNode.branches`, and the
   `Router: Node` supertrait. Keep the default (`Send`) future desugaring — `process` takes only
   `&self` + a `Send` `TaskContext`, so a `Send`-capable future is free and keeps options open.

2. **`Workflow::run` and the private `node_context` become `async`**; the pointer-walk loop is
   structurally unchanged, with `.await` added on the `node.process(ctx)` call.

3. **`ParallelNode` fan-out moves from `std::thread::scope` to `futures::future::join_all`.**
   `join_all` polls the branch futures in-place on the current task, requiring neither `Send` nor
   `'static` on them — so borrowed `&self.branches` still works. (`tokio::task::JoinSet`/`spawn`
   would force `'static + Send` branches and break on actix's single-threaded per-worker runtime.)
   The deterministic last-write-wins merge is unchanged.

4. **`Router::route` and the `OnProgress` seam stay synchronous.** Both are pure in-memory
   operations (a routing decision; a channel `send` + map write). The async run loop calls
   `on_progress` synchronously between awaits. `OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>`
   keeps its exact shape — no `Send` bound added.

5. **`engine-serve`'s `post_events` drops the `web::block` wrapper** and `.await`s
   `workflow.run(...)` directly. Actix request futures run on a per-worker single-threaded runtime,
   so the non-`Send` `OnProgress` box needs no thread-pool escape hatch and no `Send` plumbing.

## What This Commits engine-rs To

- `async-trait = "0.1"` and `futures = "0.3"` become real dependencies of `engine-core`
  (`async-trait` also of `engine-serve`); `tokio` can stay a dev-dependency of `engine-core`.
- Every `Node`/`Router` implementation (production `ParallelNode` + the test fixtures) carries
  `#[async_trait::async_trait]` and an `async fn process`; tests calling `run`/`process` become
  `#[tokio::test]` and `.await`.
- `ParallelNode`'s true concurrency now comes from the async runtime's I/O driver rather than OS
  threads — appropriate for the I/O-bound Claude Code node, and the direction the whole engine
  leans from EN.2.A onward.

## Rejected Alternatives

- **Keep `Node::process` synchronous, block a thread in `ClaudeCodeStep`.** Simplest, no refactor,
  but bakes in Python's thread-blocking ceiling at exactly the point Rust could exceed it, and
  leaves `web::block` carrying every run. Rejected — the async lever is the reason for the Rust port.
- **Native `async fn` in traits (RPITIT), no `async-trait`.** Stable since 1.75 but not `dyn`-safe;
  `Box<dyn Node>` is pervasive, so this would force manual `Pin<Box<dyn Future>>` at every site.
  `async-trait` generates exactly that, ecosystem-standard, at negligible per-call boxing cost next
  to a subprocess spawn. Revisitable later via `trait-variant` if `dyn`-async stabilizes.
- **`JoinSet`/`tokio::spawn` for parallel fan-out.** Would force `'static + Send` branch futures
  (cloning branches into `Arc`s) and a multi-thread runtime handle — heavier, and incompatible with
  actix's single-thread worker. `join_all` preserves the current borrow-not-`'static` design.
- **Make `Router::route`/`OnProgress` async too.** No benefit — both are non-blocking in-memory
  operations — and it would widen the refactor into `dispatch_route`, the validator's router
  classification, and the durable-writer bridge for nothing.
