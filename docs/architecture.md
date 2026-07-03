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

The Cargo workspace (EN.0.A) declares the four member crates below. `engine-core` (EN.1.A),
`engine-contract` (EN.0.B), `engine-store` (EN.0.B), and now `engine-serve` (EN.1.C) hold real
types: dispatch, in-memory live state, the durable-write bridge, and the actix-web HTTP surface.

```
engine-rs/
├── Cargo.toml            (workspace root — resolver 2, workspace.package, workspace.dependencies)
├── crates/
│   ├── engine-core/       ← node.rs (Node trait + NodeRegistry + as_router() hook), schema.rs
│   │                         (WorkflowSchema/NodeConfig), workflow.rs (Workflow pointer-walk
│   │                         runner + on_progress seam + Router-aware dispatch + new_validated()),
│   │                         routing.rs (Router trait + dispatch_route()), parallel.rs
│   │                         (ParallelNode fan-out/merge), validate.rs (WorkflowValidator graph
│   │                         validator)
│   ├── engine-contract/   ← data-contract serde types (events.rs: EventsRow/NodeRun/
│   │                         NodeRunStatus/Usage; task_context.rs: TaskContext), matching
│   │                         orchestrator data-contract.md v1.0.1 byte-for-byte
│   ├── engine-store/      ← postgres.rs: sqlx::PgPool connect/insert_event/update_event/
│   │                         get_event for the durable `events` record
│   └── engine-serve/      ← bastion serve embedding (EN.1.C): dispatch.rs (Dispatcher — dual
│   │                         workflow_registry/schema_registry lookup by workflow_type,
│   │                         DispatchError::UnknownWorkflowType), live_state.rs (LiveStateStore —
│   │                         in-memory Arc<RwLock<HashMap<RunId, TaskContext>>> record/get/
│   │                         list_active/remove, no-DB-poll read path for the local Console),
│   │                         durable.rs (DurableHandle/spawn_durable_writer/durable_on_progress —
│   │                         mpsc-bridged async writer mapping on_progress TaskContext snapshots to
│   │                         engine_contract::EventsRow, inserting the first PENDING snapshot per
│   │                         run and updating subsequent ones; self-skips Postgres I/O when no
│   │                         pool/DATABASE_URL is configured), http.rs (actix-web surface: POST
│   │                         /events/ with X-API-Key gating dispatch + live-state + durable-write,
│   │                         GET /health, GET /workflows, GET /workflows/{type}/graph)
└── tests/                 ← round-trip + integration fixtures
    (crates/engine-core/tests/workflow_runner.rs — fixture 3-node linear workflow integration test;
    crates/engine-core/tests/parallel.rs — ParallelNode fan-out/merge integration tests;
    crates/engine-core/tests/validator.rs — WorkflowValidator + router-aware Workflow::run
    integration tests (valid/rejected schemas, router back-edge dispatch);
    crates/engine-contract/tests/round_trip.rs — fixture byte-for-byte serde round-trip;
    crates/engine-store/tests/postgres_round_trip.rs — DATABASE_URL-gated live Postgres round-trip;
    crates/engine-serve/tests/dispatch_integration.rs — headline EN.1.C integration test: live-state
    read with no DB query, byte-identical durable EventsRow mapping for a fixture 2-node workflow,
    and 422 for an unregistered workflow_type)
```

## Build & CI

Async runtime + persistence: `tokio` + `sqlx` (postgres, runtime-tokio, tls-rustls) — see
`planning/decisions/D2-async-runtime-choice.md`. `engine-store` carries `sqlx` as a real
dependency for its Postgres layer; `engine-contract` carries `chrono`/`uuid` for the data-contract
types; `engine-core` carries `async-trait` and `futures` as real dependencies (EN.2.0) alongside
`tokio` as a dev-dependency. `engine-serve` (EN.1.C) now carries `chrono`, `sqlx`, `actix-web`, and
`async-trait` (EN.2.0) as real dependencies alongside `tokio` — `actix-web` is the HTTP framework
choice, see `planning/decisions/D3-http-framework-choice.md`.

CI (`.github/workflows/ci.yml`) runs on every push (all branches) and on pull requests, running
the same four gate commands as `planning/harness.json`: `cargo fmt --check`,
`cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.

## Core Types

- `Node` (trait, `engine-core::node`, `#[async_trait::async_trait]`) — `async fn process(&self, ctx:
  TaskContext) -> Result<TaskContext, NodeError>` + `fn name(&self) -> &str`; identity = the
  implementing type's own `name()` string, ported from `orchestrator/app/core/nodes/base.py`.
  Bounded `Send + Sync` so boxed trait objects work across async boundaries (EN.2.0).
- `NodeError` (`engine-core::node`) — a `{ message: String }` struct implementing `Display` +
  `std::error::Error`; carried into the node's `NodeRun.error` on failure.
- `NodeRegistry` (`engine-core::node`) — `HashMap<String, Box<dyn Node>>` keyed by `Node::name()`,
  with `register`/`get`/`contains`/`len`/`is_empty`, so the runner can resolve the next node to
  execute by identity string.
- `WorkflowSchema` / `NodeConfig` (`engine-core::schema`, serde `Serialize`/`Deserialize`) — the
  declarative graph description: `WorkflowSchema { workflow_type, start_node, nodes:
  HashMap<String, NodeConfig> }`; `NodeConfig { identity, connections: Vec<String> }` with a
  `next()` helper returning `connections[0]`. `WorkflowSchema::start()` resolves the start node's
  `NodeConfig`; `next_after(identity)` resolves a node's `connections[0]` next-node identity.
  Plain nodes still walk only `connections[0]`; router nodes (below) select the next node at
  runtime instead, including undeclared back-edges.
- `Router` (trait, `engine-core::routing`, supertrait of `Node`) — `fn route(&self, ctx:
  &TaskContext) -> Option<String>` for runtime next-node selection; `Node::as_router(&self) ->
  Option<&dyn Router>` is a default `None` hook nodes override to be detected by the registry as
  routers. `dispatch_route(&dyn Router, &TaskContext) -> Option<String>` is a thin dispatch
  helper wrapping `router.route(ctx)`.
- `ParallelNode` (`engine-core::parallel`) — fans out over a declared `Vec` of branch nodes via
  `futures::future::join_all` (EN.2.0; polled in-place on the current task, so borrowed
  `&self.branches` needs neither `Send` nor `'static`), deep-copies the `TaskContext` per branch,
  and merges `nodes`/`node_runs` back with deterministic last-write-wins semantics (later branch
  in declared order wins on key collision); the first branch `NodeError` encountered in declared
  order is propagated as the `ParallelNode`'s own error, with no partial merge on branch failure.
- `WorkflowValidator` / `ValidationError` (`engine-core::validate`) — static graph-shape checks
  run before execution: BFS reachability from `start_node`, DFS cycle detection that skips edges
  declared out of router nodes (routers are exempt so runtime back-edges are legal), and a
  fan-out arity guard rejecting non-router nodes with more than one declared connection. Router
  classification is via `NodeRegistry` lookup + `Node::as_router().is_some()`.
- `Workflow` (`engine-core::workflow`) — pointer-walk runner (not a topo-scheduler); pairs a
  `NodeRegistry` with a `WorkflowSchema`. `async fn run(event, on_progress) -> Result<TaskContext,
  WorkflowError>` (EN.2.0) seeds every declared node PENDING, emits the initial snapshot via
  `on_progress`, then walks `current_node` — resolving router nodes via `Router::route(ctx)` and
  plain nodes via `next_after` (`connections[0]`) — until `None`, ported from `workflow.py`;
  `node_context` (the RUNNING → SUCCESS/FAILED envelope) is likewise async, `.await`ing each
  node's `process`. `Workflow::
  new_validated(registry, schema)` is a fallible constructor that runs `WorkflowValidator::
  validate` first and rejects an invalid schema; the existing infallible `Workflow::new` is
  unchanged.
- `OnProgress<'a>` (`engine-core::workflow`, `type OnProgress<'a> = Box<dyn FnMut(&TaskContext) +
  'a>`) — the injected persistence seam invoked at every node boundary (initial seed, RUNNING
  entry, SUCCESS/FAILED exit). This block only defines the signature; EN.1.C wires it to Postgres.
- `WorkflowError` (`engine-core::workflow`) — a `{ message: String }` struct for graph-shape
  failures (e.g. an unresolvable node identity); distinct from `NodeError` — a node's own failure
  is captured in its `NodeRun` and does not short-circuit `run()` with an `Err`.
- `Dispatcher` (`engine-serve::dispatch`) — dual-registry (`workflow_registry` + `schema_registry`)
  lookup keyed by `workflow_type`; `register` takes a boxed `WorkflowFactory` closure
  (`Box<dyn Fn() -> Workflow + Send + Sync>`); `dispatch(workflow_type)` resolves a registered
  `Workflow` or returns `DispatchError::UnknownWorkflowType`.
- `LiveStateStore` (`engine-serve::live_state`) — in-memory `Arc<RwLock<HashMap<RunId, TaskContext>>>`
  (`RunId = uuid::Uuid`, matching `EventsRow.id`) with `record`/`get`/`list_active`/`remove`; the
  local Console's no-DB-poll read path for live run state.
- `DurableHandle` / `spawn_durable_writer` / `durable_on_progress` (`engine-serve::durable`) — an
  mpsc-bridged async durable-write seam mapping `on_progress` `TaskContext` snapshots to
  `engine_contract::EventsRow`: inserts the first (all-PENDING) snapshot per run via
  `engine_store::insert_event`, updates subsequent snapshots via `update_event`/`touch`, and
  self-skips Postgres I/O (does not fail) when no pool/`DATABASE_URL` is configured. The pure
  `message_to_row(message, created_at, updated_at) -> EventsRow` mapping is tested directly for a
  byte-identical contract shape without a live Postgres connection.
- HTTP surface (`engine-serve::http`, actix-web) — `configure(cfg)` registers four routes shared
  by the serve binary and the test harness: `GET /health`, `GET /workflows` (list registered
  workflow types), `GET /workflows/{type}/graph` (schema graph for a type), and `POST /events/`
  (X-API-Key gated; dispatches the event, records live state, and enqueues the durable write).
  `post_events` builds the `OnProgress` box inline and `.await`s `workflow.run` directly (EN.2.0)
  — no `web::block`/thread-pool escape hatch, since actix request futures already run on a
  per-worker single-threaded runtime and `Node::process` is now async.
- `TaskContext` — `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`
  — the preserved data-contract shape (see `orchestrator/docs/data-contract.md` v1.0.1).
- `NodeRun` — `status` (`pending|running|success|failed`), `started_at`/`completed_at`, `error`,
  `input`, `usage` (`{input_tokens, output_tokens, model}` for LLM nodes). Stamped RUNNING →
  SUCCESS/FAILED by the framework-owned `node_context` envelope in `workflow.rs`, not by the node
  itself.

## Data Flow

1. `bastion serve` receives a trigger via the actix-web HTTP surface (`POST /events/`, X-API-Key
   gated) — from local CLI, remote BastionUI over Tailscale, or an orchestrator-equivalent event
   POST.
2. `Dispatcher::dispatch(workflow_type)` resolves the event to a registered `Workflow` via the dual
   registry (`workflow_registry` + `schema_registry`), returning `DispatchError::
   UnknownWorkflowType` (surfaced as HTTP 422) for an unregistered type.
3. `Workflow::run` seeds all nodes declared in the `WorkflowSchema` PENDING in `TaskContext::node_runs`,
   emits the initial in-memory snapshot via `on_progress`, which fans out to both:
   - `LiveStateStore::record` — the in-memory run-state map the local Console reads with no DB poll.
   - `durable_on_progress` — the mpsc-bridged async writer that inserts the first (all-PENDING)
     snapshot as the durable `events` row via `engine_store::insert_event` before the first node runs.
4. The pointer-walk runs each node inside the framework-owned `node_context` envelope (RUNNING →
   SUCCESS/FAILED + `started_at`/`completed_at` timing), following `connections[0]` for plain
   nodes or `Router::route(ctx)` for router nodes (including undeclared runtime back-edges),
   invoking `on_progress` after every transition; a node returning `Err` halts the walk but
   `run()` still returns `Ok(TaskContext)` with the accumulated state. Each subsequent snapshot
   updates `LiveStateStore` and is persisted via `update_event`/`touch` on the durable writer.
5. Local Console reads live state directly via `LiveStateStore::get`/`list_active` (in-memory,
   no DB poll); remote observers (BastionUI) subscribe to serve's `GET /workflows` /
   `GET /workflows/{type}/graph` read-API rather than polling Postgres. The durable writer
   self-skips Postgres I/O (without failing the request) when no pool/`DATABASE_URL` is configured.
