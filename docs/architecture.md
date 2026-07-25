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
│   │                         runner + on_progress seam + Router-aware dispatch + new_validated() +
│   │                         run_with() cancellation/budget-aware entry point, EN.2.B),
│   │                         routing.rs (Router trait + dispatch_route()), parallel.rs
│   │                         (ParallelNode fan-out/merge), validate.rs (WorkflowValidator graph
│   │                         validator), cancellation.rs (CancellationToken, watch-backed,
│   │                         + stamp_cancelled(), EN.2.B), budget.rs (Budget config + BudgetLedger
│   │                         + pre-dispatch check() gate, EN.2.B), nodes/ (claude_code_step.rs —
│   │                         ClaudeCodeStep, a reusable Node wrapping core/claude-code-rs's
│   │                         execute(), EN.2.A; now cancellation-aware via
│   │                         with_cancellation_token(), EN.2.B; http_post.rs — the injectable
                         `HttpPost` trait seam + `reqwest`-backed live impl + `StubHttpPost` test
                         double, EN.4.C, used by `proposal_generator::PersistToBrainNode` to POST
                         a finished artifact to Synapse's brain-ingest endpoint)
│   ├── engine-contract/   ← data-contract serde types (events.rs: EventsRow/NodeRun/
│   │                         NodeRunStatus/Usage; task_context.rs: TaskContext), matching
│   │                         orchestrator data-contract.md v1.1.0 byte-for-byte (see
│   │                         docs/data-contract.md for the full pin)
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
│   │                         GET /health, GET /workflows, GET /workflows/{type}/graph), abort.rs
│   │                         (POST /events/{run_id}/abort, X-API-Key gated, EN.2.B — backed by a
│   │                         per-run CancellationToken RunRegistry minted/registered/deregistered
│   │                         around each post_events run_with call)
└── tests/                 ← round-trip + integration fixtures
    (crates/engine-core/tests/workflow_runner.rs — fixture 3-node linear workflow integration test;
    crates/engine-core/tests/parallel.rs — ParallelNode fan-out/merge integration tests;
    crates/engine-core/tests/validator.rs — WorkflowValidator + router-aware Workflow::run
    integration tests (valid/rejected schemas, router back-edge dispatch);
    crates/engine-contract/tests/round_trip.rs — fixture byte-for-byte serde round-trip;
    crates/engine-store/tests/postgres_round_trip.rs — `#[ignore]`d live Postgres round-trip (CI has
    no Postgres, per EN.0.A); run explicitly with `cargo test -p engine-store -- --ignored` and
    `DATABASE_URL` set — an unset `DATABASE_URL` at that point is a hard failure, not a silent skip;
    crates/engine-serve/tests/dispatch_integration.rs — headline EN.1.C integration test: live-state
    read with no DB query, byte-identical durable EventsRow mapping for a fixture 2-node workflow,
    and 422 for an unregistered workflow_type)
```

## Build & CI

Async runtime + persistence: `tokio` + `sqlx` (postgres, runtime-tokio, tls-rustls) — see
`planning/decisions/D2-async-runtime-choice.md`. `engine-store` carries `sqlx` as a real
dependency for its Postgres layer; `engine-contract` carries `chrono`/`uuid` for the data-contract
types; `engine-core` carries `async-trait` and `futures` as real dependencies (EN.2.0), and `tokio` as a real dependency
(promoted from dev-only in EN.2.B — `CancellationToken::cancelled()` is public async API, not
test-only code; see `planning/decisions/D6-cancellation-and-budget-semantics.md`).
`engine-serve` (EN.1.C) now carries `chrono`, `sqlx`, `actix-web`, and
`async-trait` (EN.2.0) as real dependencies alongside `tokio` — `actix-web` is the HTTP framework
choice, see `planning/decisions/D3-http-framework-choice.md`. `engine-core` also carries
`claude-code-rs` (a workspace path dependency on the sibling `core/claude-code-rs`) as a real
dependency (EN.2.A) — see `planning/decisions/D4-claude-code-transport-choice.md`.

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
  unchanged. `async fn run_with(event, on_progress, RunOptions) -> Result<TaskContext,
  WorkflowError>` (EN.2.B) is the cancellation/budget-aware entry point `run()` now delegates to:
  at each node boundary, before dispatch, it checks an optional `CancellationToken` and consults an
  optional `Budget` ledger, halting the walk (nodes not yet reached stay Pending) and stamping the
  reason into `TaskContext::metadata` — via `cancellation::stamp_cancelled` for a cancel, or the
  private `stamp_budget_halt` (keyed `BUDGET_METADATA_KEY = "budget"`) for a budget halt — while
  still returning `Ok(TaskContext)` with the accumulated state, mirroring how a node's own `Err`
  is handled. `RunOptions { cancellation_token: Option<CancellationToken>, budget: Option<Budget>
  }` (`#[derive(Default)]`) carries the two optional gates; `run()` itself is unchanged for
  existing callers (`engine-serve/http.rs` and pre-EN.2.B tests).
- `CancellationToken` (`engine-core::cancellation`) — a `tokio::sync::watch`-backed cooperative
  cancel signal (not `AtomicBool`+`Notify`): `cancel()` calls `tx.send_replace(true)` rather than
  `tx.send(true)`, since `watch::Sender::send` silently no-ops with zero live receivers (the case
  right after `new()`) — `send_replace` updates the retained value unconditionally, so a cancel
  issued before any `cancelled()` waiter subscribes is still observed. `async fn cancelled(&self)`
  awaits the first `true` value via `Receiver::changed`, race-free against the
  check-then-subscribe-then-await pattern. `stamp_cancelled(&mut Value)` merges a `"cancellation"`
  key into `TaskContext::metadata` (preserving other metadata keys) rather than overwriting it.
  Promoted `tokio` from a dev-dependency to a real `engine-core` dependency (EN.2.B) since
  `cancelled()` is now public async API, not test-only code — see
  `planning/decisions/D6-cancellation-and-budget-semantics.md`.
- `Budget` / `BudgetLedger` (`engine-core::budget`, EN.2.B) — `Budget` is a config struct (token
  and/or cost caps); `BudgetLedger` accumulates spend from each completed node's `NodeRun.usage`
  (tokens) plus an optional per-call `cost_usd`, folded in separately since `engine_contract::Usage`
  carries no cost field per the data contract. **EN.4.0:** `Workflow::run_with` now supplies that
  `cost_usd` itself — after each node completes, `node_cost_usd(&ctx, &identity)` (`workflow.rs`)
  reads the node's own `ctx.nodes[identity]["cost_usd"]` (the same field shape `ClaudeCodeStep`
  writes, and that `policy::telemetry::total_cost_usd` reads for SDLC's cost-bearing stages) and
  folds it into `ledger.record(...)` alongside token usage, so `Budget::max_cost_usd` actually
  gates a run the same way `max_total_tokens` already did. A node with no `cost_usd` in its output
  contributes `None` (token-only accounting), so behavior is unchanged when no cost cap is set.
  `check()` is the pre-dispatch gate `Workflow::run_with` calls before each node: returns
  `Allow` or `Halt(BudgetHaltReason)` when accumulated spend is *reached* (`>=`) the configured
  cap — a cap hit exactly by the last completed node stops the walk before the node that would
  exceed it. `BudgetHaltReason::to_json()` renders `{cap, spent, limit}` for the metadata stamp;
  `budget.rs` itself never mutates a `TaskContext` — `Workflow::run_with` owns the write.
  Absent `Budget` config, `check()` always allows.
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
  `post_events` builds the `OnProgress` box inline and `.await`s `workflow.run_with` directly
  (EN.2.B; previously `workflow.run`, EN.2.0) — no `web::block`/thread-pool escape hatch, since
  actix request futures already run on a per-worker single-threaded runtime and `Node::process` is
  now async. `post_events` mints a `CancellationToken` per run, registers it in `RunRegistry`
  keyed by `run_id` for the duration of the run, and deregisters it unconditionally after
  `run_with` returns (`Ok` or `Err`) — so a finished run's `run_id` 404s on a later abort call
  rather than staying abort-able forever.
- Abort endpoint (`engine-serve::abort`, EN.2.B) — `POST /events/{run_id}/abort`, gated by the
  same `check_api_key` (widened from private to `pub(crate)`) as `/events/`, backed by
  `RunRegistry` (a registry of live per-run `CancellationToken`s). Looks up `run_id`: `401` on a
  missing/invalid API key, `404` if the run isn't currently registered (unknown or already
  finished), `202 Accepted` on success (matching `post_events`'s existing `202` convention) —
  calls `token.cancel()`, which `Workflow::run_with` observes at the next node boundary.
- `TaskContext` — `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`
  — the preserved data-contract shape (see `docs/data-contract.md`, pinned to canonical v1.1.0).
- `NodeRun` — `status` (`pending|running|success|failed`), `started_at`/`completed_at`, `error`,
  `input`, `usage` (`{input_tokens, output_tokens, model}` for LLM nodes). Stamped RUNNING →
  SUCCESS/FAILED by the framework-owned `node_context` envelope in `workflow.rs`, not by the node
  itself.
- `ClaudeCodeStep` (`engine-core::nodes::claude_code_step`, EN.2.A) — a reusable `Node` that spawns
  a Claude Code session via `claude_code_rs::execute` and maps its `Outcome` into the node's
  `TaskContext::nodes` output (`{content, cost_usd, model, structured}` — `structured` is
  `outcome.structured_output`, the SDK's parsed JSON when the caller set `config.json_schema` and
  the model's reply matched it, else `null`) and `NodeRun.usage`. Constructed with a
  fixed prompt (`new`) or a prompt built fresh from the live `TaskContext` on each call
  (`with_prompt_builder`); its subprocess call goes through an injectable `Transport` closure
  (`with_transport`) so the gated test suite stubs it instead of spawning a real `claude` process.
  Per `planning/decisions/D4-claude-code-transport-choice.md`, this node owns none of the
  subprocess/argv/parse logic — that surface belongs entirely to `core/claude-code-rs`.
  Also carries an optional `CancellationToken` (`with_cancellation_token`, EN.2.B): `process()`
  races it against the awaited transport future via `tokio::select!`, and on a cancellation win
  drops the in-flight future and returns `Ok(ctx)` unchanged (rather than a `NodeError`) — a `Node`
  never touches its own `NodeRun` status, so this lets `node_context` mark the node `Success` and
  defers the actual cancelled-terminal-state stamping to `Workflow::run_with`'s per-boundary
  cancellation check before the next node dispatch.

  **Model attribution (as of the 2026-07-16 SDK fix, claude-code-rs D2).** The `claude` CLI has no
  top-level `model` field; it reports a *map* of models (`modelUsage`), since one call can bill
  several. `content` comes from the SDK's `text`, and `model` from
  `Outcome::primary_model()` — an SDK-side heuristic (cost, then output tokens, then key order) that
  returns `None` when no model ran. Because `engine_contract::Usage::model` is a required `String`
  (the orchestrator data contract's shape, §6), this node supplies the literal `"unknown"`
  when the SDK reports none. That fallback lives here, at the seam, rather than loosening a contract
  type that `bastion` also reads — see `docs/data-contract.md` §6 and D20.

  In practice `"unknown"` is a defensive backstop on the default transport: `modelUsage` is empty
  only on the CLI's error envelope, and `claude_code_rs::execute` now returns `Err(Error::Api)` for
  that case, so the node fails before stamping usage. It remains reachable via a custom `Transport`,
  and would become reachable by default if the CLI ever emitted a success envelope with no
  `modelUsage`.

## Data Flow

1. `bastion serve` receives a trigger via the actix-web HTTP surface (`POST /events/`, X-API-Key
   gated) — from local CLI, remote BastionUI over Tailscale, or an orchestrator-equivalent event
   POST. A live run can be cancelled mid-flight via `POST /events/{run_id}/abort` (EN.2.B, same
   API-key gate), which resolves the run's registered `CancellationToken` and calls `cancel()`.
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
   `run()`/`run_with()` still return `Ok(TaskContext)` with the accumulated state. Each subsequent
   snapshot updates `LiveStateStore` and is persisted via `update_event`/`touch` on the durable
   writer. Before each node dispatch, `run_with` (EN.2.B) also checks the run's optional
   `CancellationToken` and optional `Budget` ledger, halting the walk the same way a node error
   does (still `Ok`, nodes not yet reached stay Pending) and stamping the reason
   (`metadata.cancellation` or `metadata.budget`) into `TaskContext::metadata`.
5. Local Console reads live state directly via `LiveStateStore::get`/`list_active` (in-memory,
   no DB poll); remote observers (BastionUI) subscribe to serve's `GET /workflows` /
   `GET /workflows/{type}/graph` read-API rather than polling Postgres. The durable writer
   self-skips Postgres I/O (without failing the request) when no pool/`DATABASE_URL` is configured.
