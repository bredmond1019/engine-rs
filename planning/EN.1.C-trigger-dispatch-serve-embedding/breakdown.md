---
type: Plan
title: "Task Breakdown — EN.1.C Task 4 (HTTP surface + X-API-Key + framework decision)"
description: Atomic sub-step breakdown of EN.1.C task 4 — the actix-web HTTP surface, X-API-Key auth, and the D3 framework decision.
doc_id: en-1-c-breakdown
layer: [engine, console]
project: engine-rs
status: draft
keywords: [breakdown, actix-web, http surface, x-api-key, dispatch, decision]
related: [en-1-c-tasks, master-plan]
---

# Task Breakdown — EN.1.C Task 4 (HTTP surface + X-API-Key + framework decision)

## Source Spec
`planning/EN.1.C-trigger-dispatch-serve-embedding/tasks.md` (task 4 of 6)

> **Scope:** this breakdown decomposes **only task 4**, as requested. Tasks 1–3 (`dispatch.rs`,
> `live_state.rs`, `durable.rs`) are prerequisites (`dependsOn: [1,2,3]`) and are referenced by the
> interfaces they will expose per the spec — implement them first (or in the same `/sdlc-flow` run,
> which runs tasks in dependency order).

## Goal
Record the HTTP-framework choice (actix-web) as decision D3, add the dependency, and implement the
four contract endpoints in `crates/engine-serve/src/http.rs` — `POST /events/` (X-API-Key gated,
dispatch-backed, 422 on unregistered type), `GET /health`, `GET /workflows`,
`GET /workflows/{type}/graph` — with actix-web `test`-harness endpoint tests.

## How to Use
Work top to bottom. Each sub-step is a single atomic action. Run the inline **Verify** checks as you
go — do not batch them at the end. Each check must pass before continuing.

---

## Steps

### Step 4: HTTP surface + X-API-Key + framework decision

#### 4.1 Add `actix-web` to the workspace dependency table
**File:** `Cargo.toml` (workspace root)
**Action:** edit — under `[workspace.dependencies]`, append after the existing `uuid` line:
```toml
actix-web = "4"
```
Leave the existing `tokio`/`sqlx`/`serde`/`serde_json`/`chrono`/`uuid` entries unchanged.

#### 4.2 Add `actix-web` to `engine-serve`'s manifest
**File:** `crates/engine-serve/Cargo.toml`
**Action:** edit — under `[dependencies]`, add:
```toml
actix-web = { workspace = true }
```
Then add a `[dev-dependencies]` section (if not present) for the test harness — actix's `test`
module is behind the crate's default features, so no extra dev-dep is strictly required, but pin
`tokio`'s test macros are already available via the workspace `tokio` `full` features. No new
dev-dep needed; confirm `actix-web` is the only addition.

**Verify:** `cargo build -p engine-serve` → compiles (actix-web resolves; no code uses it yet).

#### 4.3 Write the D3 framework decision record
**File:** `planning/decisions/D3-http-framework-choice.md` (new)
**Action:** create — mirror the D2 OKF frontmatter + structure. Frontmatter:
```yaml
---
type: Decision
title: "D3: HTTP Framework for engine-serve"
description: Standardizes engine-serve's HTTP surface on actix-web, consistent with the sibling rag-engine-rs service and the D2 tokio runtime.
doc_id: D3-http-framework-choice
layer: [engine, console]
project: engine-rs
status: active
keywords: [actix-web, http, engine-serve, framework, bastion serve, api]
related: [decisions-index, master-plan, D2-async-runtime-choice]
---
```
Body sections (match D2's shape): `**Decided:** 2026-07-03` / `**Status:** Accepted`; a
`## Decision` naming **actix-web 4** for `engine-serve`'s four-endpoint surface
(`POST /events/`, `GET /health`, `GET /workflows`, `GET /workflows/{type}/graph`); a
`## Rationale` capturing: (a) actix-web runs on tokio (no runtime conflict with D2 / the
`bastion serve` host), (b) it is the same framework the sibling `rag-engine-rs` service already
uses — operator familiarity and cross-service consistency over adopting a second framework, and
(c) its built-in `actix_web::test` harness (`init_service` + `TestRequest` + `call_service`) keeps
endpoint tests in-process and cheap. Note the reserved future event-stream read-API (BastionUI) is
out of scope here (per the block's Out-of-scope) but actix-web's SSE/streaming support keeps that
path open.

#### 4.4 Link D3 in the decisions index
**File:** `planning/decisions/index.md`
**Action:** edit — under `## Decisions`, after the D2 bullet, add:
```md
- [D3: HTTP Framework for engine-serve](./D3-http-framework-choice.md) — Standardizes
  `engine-serve`'s HTTP surface on `actix-web`, consistent with `rag-engine-rs` and the D2
  tokio runtime.
```

**Verify:** `ls planning/decisions/D3-http-framework-choice.md && grep -q D3 planning/decisions/index.md && echo OK` → `OK`

#### 4.5 Create `http.rs` — shared state + `configure` + health handler
**File:** `crates/engine-serve/src/http.rs` (new)
**Action:** create. Define the shared application state and the route wiring:
- `use actix_web::{web, App, HttpServer, HttpRequest, HttpResponse, Responder};` (+ `get`/`post` macros as used).
- **`pub struct AppState`** holding what the handlers need (wrap in `web::Data<AppState>`):
  - `dispatcher: std::sync::Arc<crate::dispatch::Dispatcher>` — the dual-registry from task 1.
  - `live: crate::live_state::LiveStateStore` — the in-memory store from task 2 (its handle is `Clone`/`Arc`-backed).
  - `durable: crate::durable::DurableHandle` — the durable-writer handle/sender from task 3 (may be `Option`, disabled when no `DATABASE_URL`).
  - `api_key: String` — the expected `X-API-Key` value.
  > If the exact type names from tasks 1–3 differ once implemented, match them here — these are the spec-declared surfaces.
- **`pub fn configure(cfg: &mut web::ServiceConfig)`** — registers all four routes so both the serve binary and the tests share one wiring:
  ```rust
  cfg.route("/health", web::get().to(health))
     .route("/workflows", web::get().to(list_workflows))
     .route("/workflows/{workflow_type}/graph", web::get().to(workflow_graph))
     .route("/events/", web::post().to(post_events));
  ```
- **`async fn health() -> impl Responder`** → `HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))`.

#### 4.6 Add the `list_workflows` and `workflow_graph` handlers
**File:** `crates/engine-serve/src/http.rs`
**Action:** add two handlers:
- **`async fn list_workflows(state: web::Data<AppState>) -> impl Responder`** — returns the
  registered workflow types (a `Vec<String>`) from the dispatcher's `schema_registry` keys, as
  `HttpResponse::Ok().json(...)`. Sort the keys for a deterministic response.
- **`async fn workflow_graph(path: web::Path<String>, state: web::Data<AppState>) -> impl Responder`**
  — resolve the `WorkflowSchema` for `path.into_inner()` from the dispatcher; on hit return
  `HttpResponse::Ok().json(schema)` (the `WorkflowSchema` derives `Serialize`); on miss return
  `HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown workflow_type" }))`.

#### 4.7 Add the X-API-Key check and the `post_events` handler
**File:** `crates/engine-serve/src/http.rs`
**Action:** add:
- **`fn check_api_key(req: &HttpRequest, expected: &str) -> bool`** — read `req.headers().get("X-API-Key")`, compare its `to_str()` value to `expected`; return `false` when absent/unparsable/mismatched.
- **`async fn post_events(req: HttpRequest, body: web::Json<TriggerBody>, state: web::Data<AppState>) -> impl Responder`** where **`#[derive(serde::Deserialize)] struct TriggerBody { workflow_type: String, #[serde(default)] data: serde_json::Value }`** (accept the triggering event under `data`/`event`; match the contract's `POST /events/` body):
  1. If `!check_api_key(&req, &state.api_key)` → `HttpResponse::Unauthorized().finish()` (401).
  2. Resolve `state.dispatcher.dispatch(&body.workflow_type)`; on `Err(DispatchError::UnknownWorkflowType)` → `HttpResponse::UnprocessableEntity().json(...)` (**422**).
  3. On success, build the `OnProgress` closure combining task 2 + task 3: it records the snapshot into `state.live` (keyed by a freshly-minted run id, `uuid::Uuid::new_v4`) and sends it to `state.durable`. Run the (synchronous, blocking) `Workflow::run(body.data.clone(), on_progress)` off the async worker via `actix_web::web::block(move || …)` (or `spawn_blocking`) so the reactor isn't blocked.
  4. Return `HttpResponse::Accepted().json(serde_json::json!({ "run_id": run_id }))` (202) with the run id the local Console reads live state by.
  > Keep the run-execution wiring minimal — the end-to-end assertions live in task 5's integration test. This handler only needs to reach dispatch + kick the run + return the id.

#### 4.8 Register the module
**File:** `crates/engine-serve/src/lib.rs`
**Action:** edit (append-only) — add `pub mod http;` alongside the `pub mod dispatch;`,
`pub mod live_state;`, `pub mod durable;` lines added by tasks 1–3. Do not remove the existing
`crate_name()` stub in this task.

**Verify:** `cargo build -p engine-serve` → compiles with all four modules wired.

#### 4.9 Endpoint tests
**File:** `crates/engine-serve/src/http.rs`
**Action:** add a `#[cfg(test)] mod tests` using actix's in-process harness. Build a test app with a
small fixture: a `Dispatcher` with one registered fixture workflow (reuse the `SuccessNode`-style
2-node linear pattern from `engine-core`'s tests) and a known `api_key = "test-key"`.
Helper: `fn test_app_state() -> AppState { … }` and
`let app = test::init_service(App::new().app_data(web::Data::new(state)).configure(configure)).await;`
Tests (each `#[actix_web::test]`):
- **`health_returns_200`** — `TestRequest::get().uri("/health")` → assert `status() == 200`.
- **`post_events_without_key_is_rejected`** — `TestRequest::post().uri("/events/")` with a valid
  body but no `X-API-Key` header → assert `status() == 401`.
- **`post_events_unknown_workflow_type_returns_422`** — `TestRequest::post().uri("/events/")`
  with header `("X-API-Key", "test-key")` and body `{"workflow_type":"nope","data":{}}` → assert
  `status() == 422`.
- **`workflow_graph_unknown_type_returns_404`** — `TestRequest::get().uri("/workflows/nope/graph")`
  → assert `status() == 404`.
- **`list_workflows_lists_registered`** — `TestRequest::get().uri("/workflows")` → assert 200 and
  the JSON body contains the fixture workflow type.

**Verify:** `cargo test -p engine-serve http::` → the five endpoint tests pass.

---

**Verify (whole step):**
```
cargo fmt --check && cargo clippy -- -D warnings && cargo test -p engine-serve && cargo build --release
```
→ all four gate commands exit 0.

---

## Acceptance Criteria
- A dual-registry (`workflow_registry` + `schema_registry`) dispatch resolves a fixture `workflow_type` to a runnable `Workflow`; an unregistered `workflow_type` is rejected with a 422-equivalent typed error (surfaced as HTTP 422 by the `POST /events/` endpoint).
- The four HTTP endpoints exist and behave: `POST /events/` (requires a valid `X-API-Key`; triggers dispatch; 422 on unregistered type; 401/403 on missing/bad key), `GET /health` (200), `GET /workflows` (lists registered workflow types), `GET /workflows/{type}/graph` (returns the schema/graph for a registered type, 404 for an unknown one).
- The HTTP-framework choice is recorded in a new `planning/decisions/` file and linked from the index.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass clean.

*(The live-state, durable-write, and end-to-end criteria in `tasks.md` are covered by tasks 2, 3, and 5 respectively; this breakdown scopes only task 4's slice.)*

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

## Notes
- **Shared file `crates/engine-serve/src/lib.rs`.** Tasks 1–4 each append one `pub mod …;` line to
  this file; the spec serializes them via a `dependsOn` chain (1→2→3→4) and `/sdlc-flow` runs tasks
  in order in one worktree, so the appends never collide. Keep sub-step 4.8 **append-only** — do not
  rewrite existing `pub mod` lines or the `crate_name()` stub.
- **Blocking `Workflow::run` inside an async handler.** `Workflow::run` is synchronous and blocking;
  call it via `web::block`/`spawn_blocking` in `post_events` (sub-step 4.7) so the actix reactor
  isn't stalled. The `OnProgress` closure must be `Send` for this — snapshots are cloned and handed
  to the live store + durable channel, both of which are `Send`/`Arc`-backed by design.
- **Interfaces are spec-declared, not yet built.** `Dispatcher`, `DispatchError::UnknownWorkflowType`,
  `LiveStateStore`, and `DurableHandle` are the surfaces tasks 1–3 will expose. If their final names
  differ at implementation time, adjust 4.5–4.7 to match — the behavior (dispatch → 422, live record,
  durable send) is the contract, not the exact type names.
- **`WorkflowSchema` already derives `Serialize`/`Deserialize`** (`crates/engine-core/src/schema.rs`),
  so the `GET /workflows/{type}/graph` handler can return it directly as JSON with no wrapper type.
- **CLAUDE.md rules honored:** every sub-step lands tests (rule 1); D3 is a new append-only decision
  file linked from `index.md` (rule 4 + the index-update rule); OKF frontmatter is specified for the
  new `.md` files (rule 2).
