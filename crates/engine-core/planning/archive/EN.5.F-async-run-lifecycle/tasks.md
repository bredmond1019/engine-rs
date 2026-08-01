---
type: Plan
title: "Task Spec — EN.5.F Async run lifecycle: non-blocking trigger, run readback, progress stream"
description: Decomposed task spec for EN.5.F — spawn the run instead of awaiting it, add GET /events/{event_id} readback and an SSE progress stream, retain completed runs in bounded live state, and set a default run budget on the HTTP path.
doc_id: en-5-f-async-run-lifecycle-tasks
layer: [engine]
project: engine-rs
status: archived
keywords: [async-lifecycle, sse, run-readback, actix, budget, live-state, data-contract]
related: [en-5-e-composition-primitives-tasks, en-6-a-egress-dispatch]
---

> Archived 2026-08-01 — residue distilled into knowledge.md/memory.md

# Task Spec — Phase 5, Block F

**Status:** Not started · **Last run:** never

## Goal

Make a triggered run observable instead of awaited: spawn the run and return `202 {run_id, event_id}`
immediately, add the canonical `GET /events/{event_id}` readback plus an SSE progress stream over a
`tokio::sync::broadcast` tee inside the existing `on_progress` seam, and set a default `Budget` on the
HTTP path that today passes `budget: None`.

## Context Pointers

**Plan section:** `planning/master-plan.md` → *EN.5.F — Async run lifecycle: non-blocking trigger,
run readback, progress stream*. Every Phase 6 channel adapter is blocked on this (Slack needs a ~3s
ACK; a pipeline run takes minutes), as are `EN.6.A`'s `WorkflowTriggerDispatch`, `bastion`'s
`BA.11.N`, `bastion-web`'s `BW.3.C`, and `bastion-ui`'s `BU.5.A`.

**Decisions taken at spec time** (the block left these unspecified; confirmed with the repo owner
2026-07-26 — treat as settled, not re-litigable):

- **SSE route:** `GET /events/{event_id}/stream`, nesting under the run it streams, mirroring the
  existing `POST /events/{run_id}/abort` convention. Engine-rs-only — the canonical contract has no
  SSE route.
- **Default budget:** env-configurable, `ENGINE_RUN_MAX_COST_USD` (default `5.0`) and
  `ENGINE_RUN_MAX_TOKENS` (default unset).
- **Completed-run retention:** count-based — the most recent `100` completed runs held in a bounded
  ring, oldest evicted past the cap.

**Repo surfaces this block touches:**

- `crates/engine-serve/src/http.rs` — `post_events` currently `.await`s `workflow.run_with(...)`
  inside the handler and only then returns `202 {"run_id"}`, mapping a run failure to a `500`.
  `configure` is the shared route table both the serve binary and the test harness mount.
- **`OnProgress` is not `Send`.** `engine_core::workflow::OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>`
  (`crates/engine-core/src/workflow.rs:69`) carries no `Send` bound, and `post_events`'s existing
  comment notes actix request futures run on a per-worker single-threaded runtime. A `tokio::spawn`
  requires `Send + 'static`, so it will **not** compile here. Use `actix_web::rt::spawn` (current-thread
  arbiter, `'static` only). Adding a `Send` bound to `OnProgress` would ripple through every
  `engine-core` caller and is **outside this block's named files** — do not do it.
- **`AppState` must not gain a public field.** `bastion` constructs `engine_serve::http::AppState`
  as a **struct literal** at `core/bastion/src/serve/mod.rs:278` over an unpinned path dependency
  (`engine-serve = { path = "../engine-rs/crates/engine-serve" }`), so any added field is an
  immediate cross-repo compile break for zero gain. Read the default budget from the environment
  inside `http.rs` instead (a memoized `OnceLock`-backed helper), which delivers the same
  configurability with no signature change. See Notes.
- `crates/engine-serve/src/live_state.rs` — `LiveStateStore` is an `Arc<RwLock<HashMap<RunId, TaskContext>>>`
  with `record`/`get`/`list_active`/`remove`. **`get(run_id) -> Option<TaskContext>` is consumed by
  `bastion`'s `GET /api/runs/{id}` projection and must keep its current signature and semantics.**
  It stores only a `TaskContext`, which does **not** carry `workflow_type`, `created_at`, or
  `updated_at` — all three are required by the readback response, so retention must record them
  alongside the snapshot.
- `crates/engine-serve/src/abort.rs` — `RunRegistry` register/deregister. `post_events` deregisters
  after the awaited run today; once the run is spawned, deregistration moves into the spawned task.
- `crates/engine-serve/src/durable.rs` — `spawn_durable_writer` already runs on its own
  `tokio::spawn`ed background task fed by an **unbounded** `mpsc`, and `durable_on_progress` returns
  an `impl FnMut(&TaskContext) + Send + 'static`. It likely needs no change; the block lists it as
  *Modified* defensively. Verify rather than assume.
- `crates/engine-serve/Cargo.toml` — has `actix-web`, `tokio`, `serde`, `uuid`, `chrono`, `sqlx`,
  `async-trait`. It does **not** depend on `futures`, which the SSE body stream needs. `futures` is
  already a workspace dependency (`futures = "0.3"`); prefer it over adding a new third-party crate
  (`futures::stream::unfold` over a `broadcast::Receiver` avoids pulling in `tokio-stream`).
- `docs/data-contract.md` — **Pinned Contract Version 1.3.0**. The § *HTTP surface parity* table
  lists five ported routes and states that the canonical v1.2.0 `GET /events/{event_id}` route and
  the `event_id` field on `POST /events/`'s 202 body are "**not** in `engine-serve`" and that
  porting them is "future work". This block is that work. Canonical readback shape, quoted there:
  `200 {event_id, workflow_type, status, created_at, updated_at, task_context}` with `status`
  derived server-side.
- `crates/engine-core/src/budget.rs` — `Budget { max_total_tokens: Option<u64>, max_cost_usd: Option<f64> }`.
  There is **no** `Default` impl carrying values; the default is constructed explicitly.
- `crates/engine-serve/tests/{abort_integration,dispatch_integration}.rs` — the existing in-process
  actix harness style (a `WaitNode` fixture is already used by `abort_integration.rs`) to model the
  new `async_lifecycle.rs` on.

**CLAUDE.md rules that apply:** every task ships tests (standing rule 1); OKF frontmatter on new
`docs/`/`planning/` markdown (rule 2); decisions are append-only in `planning/decisions/` (rule 4);
the four gated checks in `planning/harness.json` must pass.

## Step-by-Step Tasks

See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria

- `POST /events/` returns `202 {run_id, event_id}` in **under ~100ms** for a workflow whose first
  node sleeps well beyond that, proving the response no longer awaits the run. `event_id` equals
  `run_id` (both are the `events.id` primary key).
- `GET /events/{event_id}` returns a **running** status while the run is in flight and a **terminal**
  status for the same run after it completes, with the canonical body
  `{event_id, workflow_type, status, created_at, updated_at, task_context}`. It is `X-API-Key` gated
  (401 without) and returns 404 for an unknown or malformed id.
- An SSE client on `GET /events/{event_id}/stream` receives **one frame per node transition** and a
  **terminal frame**, then the stream ends.
- Aborting a **spawned** run via `POST /events/{run_id}/abort` still stamps its terminal state — the
  cancelled marker lands in `metadata` and the run reads back terminal.
- A run exceeding the default HTTP budget halts with the **budget marker in `metadata`**; the
  default is read from `ENGINE_RUN_MAX_COST_USD` (default `5.0`) and `ENGINE_RUN_MAX_TOKENS`
  (default unset).
- `LiveStateStore` retains the most recent **100** completed runs and evicts the oldest past that
  cap; a live run is never evicted by the cap; `get(run_id) -> Option<TaskContext>` keeps its current
  signature and semantics.
- **`engine_serve::http::AppState` gains no new public field**, and `bastion` compiles unchanged
  (`cargo check` in `../bastion` passes) with `GET /api/runs/{id}` still projecting live state.
- `docs/data-contract.md`'s § *HTTP surface parity* records `GET /events/{event_id}` and the
  `event_id` field as **ported**, adds `GET /events/{event_id}/stream` as an engine-rs-only
  extension, and carries a dated changelog row. The **Pinned Contract Version stays 1.3.0** — this
  ports an already-canonical route; it does not bump the canonical contract.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all
  pass.

## Out of Scope

Carried verbatim from the block definition — these are a hard boundary:

- Per-channel inbound webhook routes and their signature verification (`EN.6.B`–`EN.6.E`).
- Unifying the three coexisting auth schemes (`Bearer` serve token, `X-API-Key` engine,
  unauthenticated) — noted, not fixed here.
- Suspend/resume (`EN.6.F`).
- Retry/queue durability for failed runs.

Additionally out of scope by derivation: adding a `Send` bound to `engine_core`'s `OnProgress`, and
any Postgres-backed readback fallback (the readback serves from retained live state only — CI has no
`DATABASE_URL`).

## Validation Commands

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

<!-- The four gated checks from planning/harness.json. `cargo test` is authoritative for the verdict. -->

Spec-specific spot checks (run alongside, not in place of, the above):

```
cargo test -p engine-serve --test async_lifecycle
cargo test -p engine-serve --test abort_integration
cargo test -p engine-serve
cargo check --manifest-path ../bastion/Cargo.toml
```

## Notes

- **Why the default budget is env-read, not an `AppState` field.** The block says "set a default
  `Budget` on the HTTP path" and the owner chose env-configurable. The obvious shape —
  `AppState { ..., default_budget }` — is a cross-repo compile break, because `bastion` builds that
  struct with a literal (`core/bastion/src/serve/mod.rs:278`) over an unpinned path dep. A memoized
  `default_budget_from_env()` in `http.rs` gets the same configurability with a zero-width public
  surface change. If an implementer finds a hard reason to put it on `AppState` anyway, that is an
  Amendment-Log deviation and `bastion` must be updated in the same pass.
- **The 500 path disappears.** Today a failed run yields `500 {error, run_id}`. Once the run is
  spawned, the response is already sent before the run can fail, so failure must surface through the
  readback (`status: "failed"`) and the SSE terminal frame instead. This is an intended semantic
  change; call it out in the docs pass.
- **`durable.rs` and `abort.rs` may end up untouched.** Both are listed *Modified* in the block, but
  `spawn_durable_writer` is already background+unbounded and `RunRegistry` is already
  `Arc`-shareable. Verify against the spawned-run lifetime; do not manufacture edits to match the
  block's file list.
- **Terminal-state ownership.** Registry deregistration, live-state terminal marking, and the SSE
  terminal frame all have to happen on **every** exit path of the spawned task — success, node
  error, cancellation, and budget halt. That single cleanup point is where leaks hide; it is the
  reason this block is a `flow` with a consolidated review.

## Amendment Log

<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
