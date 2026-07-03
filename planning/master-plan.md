---
type: Plan
title: engine-rs Master Plan
description: Strategic roadmap and phase specifications for engine-rs — Bastion's native Rust execution engine.
doc_id: master-plan
layer: [engine, console]
project: engine-rs
status: active
keywords: [master plan, roadmap, blocks, Rust engine, data contract, bastion serve, SDLC flow]
related: [context, status, planning-index, brain:D42-rust-engine-parallel-pilot, core:engine-rs-rewrite, core:wf0-engine-seat]
---

# engine-rs — Master Plan

*Living document. Created 2026-07-02.*

## The Goal, Stated Plainly

`engine-rs` is Bastion's native Rust execution engine: a graph-validated workflow runtime that embeds
directly in the `bastion serve` daemon, holds live run state in-memory, and asynchronously writes the
orchestrator data contract to Postgres as a durable record. It is a greenfield rewrite run **in
parallel** as a pilot (governing decision [D42](../../../docs/decisions/D42-rust-engine-parallel-pilot.md)) —
Rust is the product direction for the Engine layer, but the Python `orchestrator` stays the fast
prototyping tier and the production path until `engine-rs` reaches **data-contract parity**.
Graduation happens per-workflow, not big-bang: the first migrated workload is SDLC-flow (the
highest-volume workload and its intended home).

"Ready" for this first milestone (Phases 0–3 together) means: a real SDLC block ships end-to-end
through the Rust engine, `bastion` observes the run live via serve's in-memory state, and the durable
`events` row it writes is byte-identical to what the Python orchestrator would have written for the
same run — so a direct Postgres reader or a reconnecting remote observer cannot tell which engine
produced it.

## The Destination

The named outcome is **`engine-rs` embedded in `bastion serve`** as the primary execution substrate
for Bastion's agentic SDLC pipeline, with the Python `orchestrator` demoted to legacy/prototyping
duty once parity holds across all seven workflows. The differentiator over the Python path: local
reads are in-memory (no DB poll on the hot path), native cancellation + cost/budget gating that
Python never shipped, and one process for Engine + Console instead of a cross-process poll loop.

## Architecture / Design Overview

**Transport architecture (D42).** A run outlives any single `bastion` CLI invocation, so the engine
lives in the long-running `bastion serve` process, not the CLI:

```
                 ┌─────────────────────── bastion serve (long-running) ───────────────────────┐
                 │                                                                             │
 bastion CLI ───▶│  trigger/dispatch ──▶ engine-core (Node/Workflow/Router/Validator/Parallel)  │
                 │        │                              │                                     │
                 │        │                       in-memory run state (shared/channel)          │
                 │        │                              │                                     │
                 │        └──────────────▶ async durable-write (events row) ──▶ Postgres        │
                 │                                        │                                     │
 local Console ◀─┼──── direct in-memory read (no DB poll)─┘                                     │
                 │                                                                             │
 BastionUI ◀─────┼──── subscribes to serve's event stream / read-API (over Tailscale) ──────────┤
 (remote)        └─────────────────────────────────────────────────────────────────────────────┘
```

- **Local reads: in-memory.** Engine and Console are one language in one process — the local
  Console reads live run state directly (channel / shared state); no DB poll on the hot path.
- **DB demoted to durable record.** The Postgres `events` table is written asynchronously at node
  boundaries for crash-recovery/resume (D28), history, and observer catch-up — not the live IPC bus.
- **Remote observers subscribe to serve.** BastionUI (phone, over Tailscale — can't share memory)
  reads serve's event stream / read-API, not direct Postgres polling — the reserved (not yet built)
  HTTP read-API the current data contract already architects toward.

**The preserved seam (byte-for-byte).** Source of truth: `orchestrator/docs/data-contract.md` v1.0.1.
Any drift here breaks `bastion`:
- `events` table: `id` (uuid1), `workflow_type` (varchar 150), `data` (JSON), `task_context` (JSON),
  `created_at`, `updated_at`.
- `task_context` JSON: `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`.
- `NodeRun`: `status` (lowercase `pending|running|success|failed`), `started_at`/`completed_at`
  (ISO-8601 UTC), `error`, `input`, `usage` (`{input_tokens, output_tokens, model}` or null).
- Node identity = class name (the join key across `nodes`, `node_runs`, and the graph endpoint).
- Seed **all** nodes PENDING and persist the snapshot before the first node runs; re-persist at
  every node boundary.
- HTTP surface: `POST /events/` (+ `X-API-Key`), `GET /health`, `GET /workflows`,
  `GET /workflows/{type}/graph`.
- Dual-registry dispatch: every workflow in both `workflow_registry` and `schema_registry`, or the
  trigger path 422s.

**Core abstractions ported (Python → Rust idiom).** `Node` (trait; identity = type name, replacing
Python's `type[Node]`-as-dict-key with a string/enum node registry); `Workflow::run` (a pointer-walk,
not a topo-scheduler — `while current_node { … }` inside a framework-owned envelope that stamps
RUNNING → SUCCESS/FAILED + timing); routing (`connections[0]` for non-routers, `route(ctx)` at
runtime for routers — retry/back-edges are deliberately undeclared as connections so the acyclic
validator passes: declared-acyclic, runtime-cyclic); `ParallelNode` (deep-copy `TaskContext` per
branch, thread-pool execution, last-write-wins merge); `WorkflowValidator` (BFS reachability + DFS
cycle check skipping router connections); `AgentNode` (LLM node, per-node `model_provider` injected,
never hardcoded — D33 — stamps `NodeRun.usage`).

**Reuse-not-depend (D41 audit).** From `workflow-engine-rs`: the graph validator, `RetryPolicy`,
token/cost types, the multi-transport MCP client (audited "reusable periphery," not the runtime
itself). From `claude-sdk-rs`: the `execute_claude` + `Config` launcher layer, **after** its approved
repair pass (kill-on-drop cancellation, `cost_usd` → `total_cost_usd` fix, drop the removed
`--max-tokens` flag) — its `Message`/`MessageStream`/`SessionManager` layers are reuse-in-name-only
and get rewritten.

---

## The Block Contract

`/generate-tasks` reads **only the target block's section** below — not this overview, not sibling
blocks. Every block section is self-sufficient: **What / Why / Files / Out of scope / Acceptance
criteria**, plus optional **Interfaces / shared surface** and **Depends on**.

**Default ordering — phases sequential, blocks within a phase parallel.** A `Depends on` line
overrides that default only to serialize same-phase blocks that edit the same file.

---

## Phase 0 — Foundation

### EN.0.A — Cargo workspace + CI
- **What:** Stand up the `engine-rs` Cargo workspace (root `Cargo.toml` + member crates per the
  module map in `docs/architecture.md`: `engine-core`, `engine-contract`, `engine-store`,
  `engine-serve`), wire CI to run `cargo fmt --check`, `cargo clippy -- -D warnings`, and
  `cargo test` on every push, and pick the async runtime + persistence stack (tokio + sqlx or
  deadpool — an explicit open question in the source notes; decide here and record the choice as
  a `planning/decisions/` entry).
- **Why:** Every later block needs a compiling, lintable, CI-gated workspace to land into; the
  runtime/persistence choice is load-bearing for `engine-store` (EN.0.B) and must be settled first.
- **Files:**
  - *New* `Cargo.toml` (workspace root), `crates/engine-core/Cargo.toml` (+ `src/lib.rs` stub),
    `crates/engine-contract/Cargo.toml` (+ `src/lib.rs` stub), `crates/engine-store/Cargo.toml`
    (+ `src/lib.rs` stub), `crates/engine-serve/Cargo.toml` (+ `src/lib.rs` stub)
  - *New* `.github/workflows/ci.yml` (or equivalent CI config) running fmt/clippy/test
  - *New* `planning/decisions/D2-async-runtime-choice.md`
  - *Modified* `planning/harness.json` (already seeded with the Rust profile — confirm commands
    match the real workspace layout once crates exist)
- **Out of scope:** Any actual `Node`/`Workflow` logic (EN.1.*); Postgres schema/serde types
  (EN.0.B) beyond an empty `engine-store` crate stub.
- **Acceptance criteria:** `cargo build`, `cargo fmt --check`, `cargo clippy -- -D warnings`, and
  `cargo test` all succeed on a clean checkout; CI runs the same four commands on push; the
  async-runtime decision is recorded in `planning/decisions/`.

### EN.0.B — Data-contract serde types + Postgres round-trip
- **What:** Implement the serde types for the preserved data-contract seam — the `events` row
  (`id`, `workflow_type`, `data`, `task_context`, `created_at`, `updated_at`), `TaskContext`
  (`event`, `nodes`, `metadata`, `node_runs`), and `NodeRun` (`status`, `started_at`,
  `completed_at`, `error`, `input`, `usage`) — plus the Postgres read/write layer in
  `engine-store` that reads/writes the `events` table using the runtime/persistence stack chosen
  in EN.0.A.
- **Why:** This is the seam that must stay byte-for-byte identical to the Python contract
  (`orchestrator/docs/data-contract.md` v1.0.1) or `bastion` breaks; it is the foundation every
  later phase writes through.
- **Files:**
  - *New* `crates/engine-contract/src/events.rs` (`EventsRow` struct + serde impls)
  - *New* `crates/engine-contract/src/task_context.rs` (`TaskContext`, `NodeRun`, `NodeRunStatus`
    enum with lowercase serde renames)
  - *New* `crates/engine-store/src/postgres.rs` (connection pool + `insert_event`/`update_event`
    functions against the existing `events` table schema)
  - *New* `tests/fixtures/python_task_context.json` (a captured Python-emitted fixture for the
    round-trip test)
  - *New* `crates/engine-contract/tests/round_trip.rs`
- **Out of scope:** Any node execution logic that produces a `TaskContext` at runtime (EN.1.*); the
  HTTP surface (`POST /events/`, `GET /health`, etc. — EN.1.C).
- **Depends on:** `A`
- **Acceptance criteria:** A round-trip test serializes a Rust-constructed `TaskContext` /
  `EventsRow` and asserts the JSON is byte-identical to the captured Python fixture (field order
  aside — semantic equality on parsed JSON is acceptable, but no field, casing, or type must
  differ); a live Postgres insert/read round-trip test passes against the existing `events` table
  schema; `cargo test`/`clippy`/`fmt`/`build` all pass.

---

## Phase 1 — Engine core (in `bastion serve`)

### EN.1.A — Node trait + Workflow runner
- **What:** Port the core execution primitives to idiomatic Rust: a `Node` trait (single
  `process(ctx) -> ctx` method, identity = the implementing type's name via a string/enum
  registry key), and the `Workflow` runner as a pointer-walk (build `TaskContext`, parse the
  triggering event against a schema, seed all nodes PENDING + emit the initial snapshot, then
  `while current_node { … }` running each node inside a framework-owned envelope that stamps
  RUNNING → SUCCESS/FAILED + timing).
- **Why:** This is the engine's execution core — every other Phase 1 block (routing, parallel
  nodes, validation, dispatch) builds on `Node` + `Workflow`.
- **Files:**
  - *New* `crates/engine-core/src/node.rs` (`Node` trait, node registry)
  - *New* `crates/engine-core/src/workflow.rs` (`Workflow` struct, pointer-walk `run` method,
    `node_context` envelope helper)
  - *New* `crates/engine-core/src/schema.rs` (`WorkflowSchema`, `NodeConfig`)
  - *New* `crates/engine-core/tests/workflow_runner.rs` (fixture workflow exercising the full
    pointer-walk against `engine-contract` types)
- **Interfaces / shared surface:** Consumes `engine-contract`'s `TaskContext`/`NodeRun` types
  (EN.0.B); the `on_progress` callback is the injected persistence seam that EN.1.C's async
  durable-write hooks into — this block defines the callback signature, EN.1.C implements it.
- **Out of scope:** Router/parallel-node logic (EN.1.B); the acyclic validator (EN.1.B); the
  trigger/dispatch HTTP path and serve embedding (EN.1.C).
- **Depends on:** `B` (of Phase 0 — `EN.0.B`)
- **Acceptance criteria:** A fixture 3-node linear workflow runs end-to-end through `Workflow::run`
  producing a `TaskContext` matching the expected shape at each step; `NodeRun` timestamps/status
  transition correctly (PENDING → RUNNING → SUCCESS or FAILED); `cargo test`/`clippy`/`fmt`/`build`
  pass.

### EN.1.B — Router + parallel nodes + validator
- **What:** Port routing (`connections[0]` for non-routers; `route(ctx)` called at runtime for
  routers — retry/back-edges deliberately undeclared as connections), `ParallelNode` (deep-copy
  `TaskContext` per branch, run on a thread pool, merge `nodes` + `node_runs` back with
  last-write-wins), and `WorkflowValidator` (BFS reachability from `start` + DFS cycle check that
  skips router connections; only routers may have >1 declared connection).
- **Why:** These are the structural correctness guarantees (validator) and the two execution
  patterns (router, parallel) the SDLC-flow port in Phase 3 depends on — SDLC-flow's task loop is
  a router with a runtime retry back-edge.
- **Files:**
  - *New* `crates/engine-core/src/routing.rs` (router dispatch, `route(ctx)` trait method)
  - *New* `crates/engine-core/src/parallel.rs` (`ParallelNode`, thread-pool fan-out/merge)
  - *New* `crates/engine-core/src/validate.rs` (`WorkflowValidator`, BFS/DFS checks)
  - *Modified* `crates/engine-core/src/workflow.rs` (wire validator into `Workflow` construction;
    call router dispatch instead of the plain `connections[0]` path when a node is a router)
  - *New* `crates/engine-core/tests/validator.rs`, `crates/engine-core/tests/parallel.rs`
- **Out of scope:** The trigger/dispatch HTTP path, dual-registry, and serve embedding (EN.1.C);
  the Claude Code step node (EN.2.A).
- **Depends on:** `A` (of this phase — `EN.1.A`)
- **Acceptance criteria:** A fixture workflow with a router + an undeclared retry back-edge passes
  validation and executes the retry path correctly at runtime; a fixture `ParallelNode` branch
  fan-out/merge test confirms last-write-wins semantics; a fixture cyclic (non-router) workflow is
  rejected by the validator; `cargo test`/`clippy`/`fmt`/`build` pass.

### EN.1.C — Trigger/dispatch path + dual-registry + serve embedding
- **What:** Embed the engine in `bastion serve`: the dual-registry dispatch (`workflow_registry` +
  `schema_registry`, keyed by `workflow_type`), the HTTP surface (`POST /events/` with
  `X-API-Key`, `GET /health`, `GET /workflows`, `GET /workflows/{type}/graph`), in-memory live run
  state that the local Console reads directly (channel/shared state, no DB poll), and the async
  durable-write implementing the `on_progress` seam from EN.1.A against `engine-store` (EN.0.B).
- **Why:** This is what makes `engine-rs` a *running* engine inside `bastion serve` rather than a
  library — the transport architecture from D42 (local in-memory reads, DB as durable record,
  serve as the seam remote observers subscribe to).
- **Files:**
  - *New* `crates/engine-serve/src/dispatch.rs` (dual-registry, trigger resolution, 422 on
    unregistered `workflow_type`)
  - *New* `crates/engine-serve/src/http.rs` (the four HTTP endpoints)
  - *New* `crates/engine-serve/src/live_state.rs` (in-memory run-state store + local read API)
  - *Modified* `crates/engine-core/src/workflow.rs` (`on_progress` now calls into
    `engine-store`'s async durable-write, seeding all nodes PENDING and persisting before the
    first node runs, re-persisting at every boundary)
- **Interfaces / shared surface:** Consumes `engine-store`'s `insert_event`/`update_event`
  (EN.0.B) and `engine-core`'s `Node`/`Workflow`/validator (EN.1.A/EN.1.B). Exposes the HTTP
  surface later phases (and BastionUI, out of repo) integrate against.
- **Out of scope:** The reserved event-stream / subscribe-style read-API for remote observers
  (BastionUI) — noted as a future data-contract version bump in the source notes, not built here;
  the Claude Code step node (EN.2.A).
- **Depends on:** `B` (of this phase — `EN.1.B`)
- **Acceptance criteria:** A local integration test triggers a fixture workflow via the dispatch
  path, confirms the local Console-equivalent read sees live in-memory state with no DB poll, and
  confirms the durable `events` row written is byte-identical (per EN.0.B's round-trip test) to
  what the Python orchestrator would write for an equivalent run; an unregistered `workflow_type`
  returns 422; `cargo test`/`clippy`/`fmt`/`build` pass.

---

## Phase 2 — Claude Code step + control loop

### EN.2.0 — Async `Node` trait
- **What:** Make `engine-core`'s `Node::process` an `async fn` (via the `async-trait` crate, since
  `Box<dyn Node>` is load-bearing across `NodeRegistry`, `ParallelNode.branches`, and the
  `Router: Node` supertrait). Convert `Workflow::run`/`node_context` to `async`, rewrite
  `ParallelNode`'s fan-out from `std::thread::scope` to `futures::future::join_all` (polls in-place —
  no `Send`/`'static` forced on branches), and drop the `web::block` wrapper in
  `engine-serve`'s `post_events` (actix request futures run on a per-worker single-threaded runtime,
  so the non-`Send` `OnProgress` box can be awaited directly). `Router::route` and the `OnProgress`
  seam stay **synchronous** (pure in-memory, no I/O). No behavior change.
- **Why:** Spawning a Claude Code session (EN.2.A) is inherently async; a synchronous `Node::process`
  would force `ClaudeCodeStep` to block a whole OS thread for the session's duration — the same
  ceiling Python has. Making the trait async first is the load-bearing lever that lets concurrent
  runs and I/O-bound branches share the Tokio runtime instead of each claiming a dedicated thread.
  Done as its own pre-block so the workspace-wide refactor's diff stays separate from EN.2.A's
  new-feature diff. See `planning/async-node/notes.md` for the full comparison + blast radius.
- **Files:**
  - *Modified* `crates/engine-core/src/node.rs` (`Node` trait → `#[async_trait] async fn process`),
    `crates/engine-core/src/workflow.rs` (`run`/`node_context` async; `OnProgress` unchanged),
    `crates/engine-core/src/parallel.rs` (`join_all` fan-out), `crates/engine-serve/src/http.rs`
    (drop `web::block`), `crates/engine-core/Cargo.toml` + root `Cargo.toml` (`async-trait`,
    `futures`)
  - *Modified* all 19 test-fixture `impl Node` sites (mechanical `#[async_trait]` + `async fn` +
    `#[tokio::test]`/`.await` on callers)
  - *New* `planning/decisions/D5-async-node-trait.md`
- **Interfaces / shared surface:** Touches the `Node` trait signature every node implementation
  overrides — a workspace-wide change, but mechanical. `WorkflowValidator`, `Dispatcher`, and
  `durable.rs` are untouched (they never call `process`).
- **Out of scope:** Any new node type or feature (EN.2.A onward); making `Router::route` or
  `OnProgress` async; changing `ParallelNode`'s merge semantics.
- **Depends on:** Phase 1 (`EN.1.C`) — done.
- **Acceptance criteria:** The workspace compiles and every existing test passes with identical
  behavior; `web::block` is gone from `http.rs`; `crates/engine-serve/tests/dispatch_integration.rs`
  still passes under the async runner; the async-trait choice is recorded in D5;
  `cargo fmt`/`clippy -D warnings`/`test`/`build --release` all green.

### EN.2.A — Claude Code step node
- **What:** Implement a `ClaudeCodeStep` node that spawns a Claude Code session and returns its
  result into `TaskContext`, depending on the new **`core/claude-code-rs`** SDK's async
  `execute` (kill-on-drop cancellation, current-schema `total_cost_usd`/`usage` parsing,
  subscription via isolated `CLAUDE_CONFIG_DIR`). The SDK — not this block — owns the subprocess
  transport; this block wires it into a `Node` and maps its result into `NodeRun`/`TaskContext`.
  The tmux/file-drop `bastion ask` seam is **dropped** (superseded by the clean native transport we
  now own; recorded in D4).
- **Why:** SDLC-flow (Phase 3) is Claude-Code-heavy — its task loop (implement → test → triage →
  review) is built on repeated Claude Code invocations; this node is the shared primitive Phase 3
  ports the loop onto. Building the SDK fresh (rather than repairing the heavy/dated
  `portfolio/claude-sdk-rs`) gives a lean, purpose-built transport we control and dogfood.
- **Files:**
  - *New* `crates/engine-core/src/nodes/claude_code_step.rs` (`ClaudeCodeStep` node
    implementation)
  - *New* `planning/decisions/D4-claude-code-transport-choice.md` (native `core/claude-code-rs`
    subprocess SDK; subscription via isolated `CLAUDE_CONFIG_DIR`; `bastion ask` fallback dropped)
  - *Modified* workspace `Cargo.toml` (path dep on `../claude-code-rs`)
  - *New* `crates/engine-core/tests/claude_code_step.rs`
- **Interfaces / shared surface:** Depends on `EN.2.0` (async `Node`) landing and on the
  `core/claude-code-rs` SDK's milestone 1 (`execute` + credential isolation) — the SDK is a
  standalone `core/` repo with its own planning, built in parallel with `EN.2.0`, not in this block.
- **Out of scope:** Cancellation-token plumbing through the run loop and the abort endpoint
  (EN.2.B); cost/budget accounting beyond capturing a single node's `usage` (EN.2.B does the
  budget gate).
- **Acceptance criteria:** A `ClaudeCodeStep` node run against a real Claude Code session produces
  a `NodeRun` with populated `usage` (`input_tokens`, `output_tokens`, `model`) and a correctly
  JSON-serializable `output`; the chosen transport is documented in a decision file;
  `cargo test`/`clippy`/`fmt`/`build` pass.

### EN.2.B — Cancellation + abort endpoint + cost/token budget gate
- **What:** Add a cancellation token threaded through the run loop (checked between node
  boundaries and inside `ClaudeCodeStep`'s spawn/await), an authenticated abort endpoint that
  stamps the run's terminal state and propagates the cancellation token, and per-node cost/token
  accounting that can gate execution against a configured budget — the "OR.I features Python
  never shipped" named in the source notes.
- **Why:** These are engine-rs's differentiators over the Python path and are required before
  SDLC-flow (a long-running, potentially costly pipeline) can be trusted to run unattended.
- **Files:**
  - *New* `crates/engine-core/src/cancellation.rs` (cancellation token type + run-loop checks)
  - *New* `crates/engine-serve/src/abort.rs` (authenticated abort HTTP endpoint)
  - *New* `crates/engine-core/src/budget.rs` (per-node cost/token accounting + budget gate)
  - *Modified* `crates/engine-core/src/workflow.rs` (check cancellation token at each node
    boundary; consult the budget gate before dispatching a node)
  - *New* `crates/engine-core/tests/cancellation.rs`, `crates/engine-core/tests/budget.rs`
- **Out of scope:** Any UI for triggering cancel/budget config (BastionUI-side, out of repo).
- **Depends on:** `A` (of this phase — `EN.2.A`)
- **Acceptance criteria:** A running fixture workflow, when its cancellation token is triggered
  mid-run, stops at the next node boundary and the run's terminal state reflects "cancelled" (not
  "failed"); the abort endpoint requires authentication and returns the correct status for an
  unknown run id; a fixture workflow configured with a low budget halts before exceeding it and
  records why; `cargo test`/`clippy`/`fmt`/`build` pass.

---

## Phase 3 — SDLC-flow parity (the payload)

*Forward-looking — authored with the full skeleton while the architecture is fresh. Expect the
Files lists to need refinement once this phase is next, after the Python
`sdlc_flow_workflow*` source is re-read against whatever Phase 0–2 landed.*

### EN.3.A — SDLC-flow setup + task loop port
- **What:** Port the first half of the 16-node SDLC pipeline
  (`orchestrator/app/workflows/sdlc_flow_workflow*`): setup → generate/load tasks → the task loop
  (implement → test → triage → review, with the runtime retry back-edge from EN.1.B) using the
  `ClaudeCodeStep` node from EN.2.A.
- **Why:** The task loop is the highest-volume, most-repeated part of SDLC-flow and the part most
  exercised by cancellation/budget (Phase 2) — proving it first de-risks the rest of the port.
- **Files:**
  - *New* `crates/engine-core/src/workflows/sdlc_flow/setup.rs`,
    `crates/engine-core/src/workflows/sdlc_flow/task_loop.rs` (implement/test/triage/review
    nodes + the router driving the retry back-edge)
  - *New* `crates/engine-core/src/workflows/sdlc_flow/schema.rs` (event schema, registered in
    both `workflow_registry` and `schema_registry` from EN.1.C)
  - *New* `crates/engine-core/tests/sdlc_flow_task_loop.rs`
- **Out of scope:** Docs/wrap-up/PR nodes (EN.3.B); the six remaining Python workflows (later,
  one at a time, out of this milestone).
- **Depends on:** `B` (of Phase 2 — `EN.2.B`)
- **Acceptance criteria:** A fixture task spec runs through setup → generate/load tasks → the full
  task loop including at least one triggered retry back-edge, producing `NodeRun`/`TaskContext`
  state matching the Python workflow's shape for an equivalent fixture; registered in both
  registries; `cargo test`/`clippy`/`fmt`/`build` pass.

### EN.3.B — SDLC-flow docs/wrap-up/PR port + parity acceptance
- **What:** Port the remaining SDLC-flow nodes (docs → wrap-up → PR) and run the full pipeline
  end-to-end through the Rust engine against a real block, observed live by `bastion` — the
  milestone's capstone acceptance check.
- **Why:** This is the named "ready" checkpoint for the whole first milestone: a real block
  shipping through `engine-rs` with parity against the Python path (D44/D45 contract).
- **Files:**
  - *New* `crates/engine-core/src/workflows/sdlc_flow/docs.rs`,
    `crates/engine-core/src/workflows/sdlc_flow/wrap_up.rs`,
    `crates/engine-core/src/workflows/sdlc_flow/pr.rs`
  - *New* `crates/engine-core/tests/sdlc_flow_e2e.rs` (or an equivalent integration harness run
    against a real repo/block)
- **Out of scope:** Graduating any workflow other than SDLC-flow; decommissioning the Python
  `orchestrator`'s SDLC-flow path (a separate, later cutover decision).
- **Depends on:** `A` (of this phase — `EN.3.A`)
- **Acceptance criteria:** A real block ships end-to-end through the Rust SDLC workflow with
  parity vs. the Python path (per the D44/D45 contract); `bastion` observes the run live via
  serve's in-memory state with no DB poll; the durable `events` row is byte-identical to what the
  same run would have produced through the Python orchestrator; `cargo test`/`clippy`/`fmt`/`build`
  pass.

---

*Later, out of this milestone: the remaining six Python workflows graduate to `engine-rs` one at a
time, each behind its own parity acceptance check; Brain/RAG stays Python (it touches the framework
only through `Node.process`/`TaskContext` + Postgres, so it is cleanly separable and out of scope for
every phase above).*

---

## Quick Reference Sequence Table

| Phase | Block | What | Why | Role in destination |
|---|---|---|---|---|
| 0 | A | Cargo workspace + CI | Compiling, lintable, CI-gated foundation | Everything else lands into this |
| 0 | B | Data-contract serde types + Postgres round-trip | Preserve the byte-for-byte seam | The seam every later phase writes through |
| 1 | A | Node trait + Workflow runner | Port the execution core | The engine's central abstraction |
| 1 | B | Router + parallel nodes + validator | Port the two execution patterns + correctness guard | What SDLC-flow's task loop needs |
| 1 | C | Trigger/dispatch + dual-registry + serve embedding | Make it a running engine inside `bastion serve` | Realizes the D42 transport architecture |
| 2 | 0 | Async `Node` trait | Unlock true async I/O at the node level | Lets `ClaudeCodeStep` await a subprocess without blocking a thread |
| 2 | A | Claude Code step node (on `core/claude-code-rs`) | Shared Claude Code primitive | What SDLC-flow's task loop calls repeatedly |
| 2 | B | Cancellation + abort + budget gate | Ship the features Python never shipped | Required before trusting unattended runs |
| 3 | A | SDLC-flow setup + task loop port | Highest-volume, most-repeated part | De-risks the rest of the port |
| 3 | B | SDLC-flow docs/wrap-up/PR + parity acceptance | Capstone: real block ships end-to-end | The named "ready" checkpoint for the milestone |

---

*Sequenced by dependency and competence, not calendar. When life gets in the way, pick up where you
left off.*
