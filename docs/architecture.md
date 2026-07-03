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

The Cargo workspace (EN.0.A) declares the four member crates below; each currently holds a
compiling `src/lib.rs` stub with one trivial passing test. Real types land as Phase 0/1 blocks
fill each crate in.

```
engine-rs/
├── Cargo.toml            (workspace root — resolver 2, workspace.package, workspace.dependencies)
├── crates/
│   ├── engine-core/       ← Node trait, Workflow runner, WorkflowSchema/NodeConfig, validator
│   ├── engine-contract/   ← data-contract serde types (events row, task_context, NodeRun)
│   ├── engine-store/      ← Postgres read/write for the durable `events` record
│   └── engine-serve/      ← bastion serve embedding: in-memory run state, trigger/dispatch, HTTP surface
└── tests/                 ← round-trip + integration fixtures
```

## Build & CI

Async runtime + persistence: `tokio` + `sqlx` (postgres, runtime-tokio, tls-rustls) — see
`planning/decisions/D2-async-runtime-choice.md`. `engine-store` and `engine-serve` carry these as
real dependencies; `engine-core`'s stub only needs `tokio` as a dev-dependency so far.

CI (`.github/workflows/ci.yml`) runs on every push (all branches) and on pull requests, running
the same four gate commands as `planning/harness.json`: `cargo fmt --check`,
`cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.

## Core Types

<!-- Stub — to be filled from Phase 0 (data-contract serde types) and Phase 1 (Node/Workflow/Router). -->

- `Node` (trait) — single `process(ctx) -> ctx` method; identity = the implementing type's name,
  ported from `orchestrator/app/core/nodes/base.py`.
- `Workflow` — pointer-walk runner (not a topo-scheduler); owns `TaskContext`, seeds all nodes
  PENDING, walks `current_node` until `None`, ported from `workflow.py`.
- `TaskContext` — `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`
  — the preserved data-contract shape (see `orchestrator/docs/data-contract.md` v1.0.1).
- `NodeRun` — `status` (`pending|running|success|failed`), `started_at`/`completed_at`, `error`,
  `input`, `usage` (`{input_tokens, output_tokens, model}` for LLM nodes).

## Data Flow

<!-- Stub — to be filled once the serve-embedding trigger/dispatch path lands (Phase 1). -->

1. `bastion serve` receives a trigger (local CLI, remote BastionUI over Tailscale, or an
   orchestrator-equivalent event POST).
2. The dual registry (`workflow_registry` + `schema_registry`) resolves the event to a `Workflow`.
3. The engine seeds all nodes PENDING, emits the initial in-memory snapshot, and persists the
   first durable `events` row before the first node runs.
4. The pointer-walk runs each node inside a framework-owned envelope (RUNNING → SUCCESS/FAILED +
   timing), re-persisting the durable row at every node boundary.
5. Local Console reads live state directly from in-memory shared state/channel; remote observers
   (BastionUI) subscribe to serve's event stream / read-API rather than polling Postgres.
