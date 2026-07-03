---
type: Reference
title: engine-rs Architecture
description: Module map, core types, and data flow for engine-rs — Bastion's native Rust execution engine.
doc_id: architecture
layer: [engine, console]
project: engine-rs
status: active
keywords: [architecture, module map, core types, data flow, Rust, workflow runtime]
related: [docs-index, context, master-plan]
---

# engine-rs — Architecture

## Overview

`engine-rs` is a graph-validated workflow runtime that embeds directly in the `bastion serve`
daemon. It holds live run state in-memory (Engine and Console share one process, one language —
no DB poll on the local hot path) and asynchronously persists the data-contract `events` row to
Postgres at node boundaries as a durable record for crash-recovery, history, and remote-observer
catch-up (D42). It is a parallel-pilot rewrite of the Python `orchestrator` engine core
(`orchestrator/app/core/`), not a fork.

## Module Map

The Cargo workspace (EN.0.A) declares the four member crates below. `engine-core` now holds real
types (EN.1.A — `Node` trait, `NodeRegistry`, `WorkflowSchema`/`NodeConfig`, `Workflow` pointer-walk
runner); the other three crates still hold a compiling `src/lib.rs` stub with one trivial passing
test pending their own Phase 0/1 blocks.

```
engine-rs/
├── Cargo.toml            (workspace root — resolver 2, workspace.package, workspace.dependencies)
├── crates/
│   ├── engine-core/       ← node.rs (Node trait + NodeRegistry), schema.rs (WorkflowSchema/
│   │                         NodeConfig), workflow.rs (Workflow pointer-walk runner +
│   │                         on_progress seam); graph validator still to land
│   ├── engine-contract/   ← data-contract serde types (events row, task_context, NodeRun)
│   ├── engine-store/      ← Postgres read/write for the durable `events` record
│   └── engine-serve/      ← bastion serve embedding: in-memory run state, trigger/dispatch, HTTP surface
└── tests/                 ← round-trip + integration fixtures
    (crates/engine-core/tests/workflow_runner.rs — fixture 3-node linear workflow integration test)
```

## Build & CI

Async runtime + persistence: `tokio` + `sqlx` (postgres, runtime-tokio, tls-rustls) — see
`planning/decisions/D2-async-runtime-choice.md`. `engine-store` and `engine-serve` carry these as
real dependencies; `engine-core`'s stub only needs `tokio` as a dev-dependency so far.

CI (`.github/workflows/ci.yml`) runs on every push (all branches) and on pull requests, running
the same four gate commands as `planning/harness.json`: `cargo fmt --check`,
`cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.

## Core Types

- `Node` (trait, `engine-core::node`) — `fn process(&self, ctx: TaskContext) -> Result<TaskContext,
  NodeError>` + `fn name(&self) -> &str`; identity = the implementing type's own `name()` string,
  ported from `orchestrator/app/core/nodes/base.py`. Bounded `Send + Sync` so boxed trait objects
  work across async boundaries later.
- `NodeError` (`engine-core::node`) — a `{ message: String }` struct implementing `Display` +
  `std::error::Error`; carried into the node's `NodeRun.error` on failure.
- `NodeRegistry` (`engine-core::node`) — `HashMap<String, Box<dyn Node>>` keyed by `Node::name()`,
  with `register`/`get`/`contains`/`len`/`is_empty`, so the runner can resolve the next node to
  execute by identity string.
- `WorkflowSchema` / `NodeConfig` (`engine-core::schema`, serde `Serialize`/`Deserialize`) — the
  declarative graph description: `WorkflowSchema { workflow_type, start_node, nodes:
  HashMap<String, NodeConfig> }`; `NodeConfig { identity, connections: Vec<String> }` with a
  `next()` helper returning `connections[0]`. `WorkflowSchema::start()` resolves the start node's
  `NodeConfig`; `next_after(identity)` resolves a node's `connections[0]` next-node identity. Only
  `connections[0]` is walked in this block — branching over the rest is EN.1.B.
- `Workflow` (`engine-core::workflow`) — pointer-walk runner (not a topo-scheduler); pairs a
  `NodeRegistry` with a `WorkflowSchema`. `run(event, on_progress) -> Result<TaskContext,
  WorkflowError>` seeds every declared node PENDING, emits the initial snapshot via `on_progress`,
  then walks `current_node` via `next_after` until `None`, ported from `workflow.py`.
- `OnProgress<'a>` (`engine-core::workflow`, `type OnProgress<'a> = Box<dyn FnMut(&TaskContext) +
  'a>`) — the injected persistence seam invoked at every node boundary (initial seed, RUNNING
  entry, SUCCESS/FAILED exit). This block only defines the signature; EN.1.C wires it to Postgres.
- `WorkflowError` (`engine-core::workflow`) — a `{ message: String }` struct for graph-shape
  failures (e.g. an unresolvable node identity); distinct from `NodeError` — a node's own failure
  is captured in its `NodeRun` and does not short-circuit `run()` with an `Err`.
- `TaskContext` — `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`
  — the preserved data-contract shape (see `orchestrator/docs/data-contract.md` v1.0.1).
- `NodeRun` — `status` (`pending|running|success|failed`), `started_at`/`completed_at`, `error`,
  `input`, `usage` (`{input_tokens, output_tokens, model}` for LLM nodes). Stamped RUNNING →
  SUCCESS/FAILED by the framework-owned `node_context` envelope in `workflow.rs`, not by the node
  itself.

## Data Flow

<!-- Trigger/dispatch + serve-embedding paths (items 1-2, 5) still stub — Phase 1 serve blocks. -->

1. `bastion serve` receives a trigger (local CLI, remote BastionUI over Tailscale, or an
   orchestrator-equivalent event POST).
2. The dual registry (`workflow_registry` + `schema_registry`) resolves the event to a `Workflow`.
3. `Workflow::run` seeds all nodes declared in the `WorkflowSchema` PENDING in `TaskContext::node_runs`,
   emits the initial in-memory snapshot via `on_progress`, and (once EN.1.C lands) persists the
   first durable `events` row before the first node runs.
4. The pointer-walk runs each node inside the framework-owned `node_context` envelope (RUNNING →
   SUCCESS/FAILED + `started_at`/`completed_at` timing, following `connections[0]` only), invoking
   `on_progress` after every transition; a node returning `Err` halts the walk but `run()` still
   returns `Ok(TaskContext)` with the accumulated state.
5. Local Console reads live state directly from in-memory shared state/channel; remote observers
   (BastionUI) subscribe to serve's event stream / read-API rather than polling Postgres.
