---
type: Log
title: engine-rs Development Log
description: Chronological log of work completed for engine-rs.
doc_id: log
layer: [factory]
status: active
timestamp: "2026-07-03T12:24:21Z"
keywords: [work log, session history, development log]
related: [status, context]
---

# Log — engine-rs

*Append-only working log. One dated entry per session. Newest entries at the top.*

---

## 2026-07-03

### Completed EN.1.C-trigger-dispatch-serve-embedding end to end (6 tasks, PASS)
Ran `/sdlc-flow EN.1.C-trigger-dispatch-serve-embedding` to completion across 6 tasks, embedding the execution engine in `bastion serve`. Task 1 added a `Dispatcher` in `crates/engine-serve/src/dispatch.rs` implementing dual-registry (`workflow_registry` + `schema_registry`) dispatch keyed by `workflow_type`, rejecting unregistered types with `DispatchError::UnknownWorkflowType`. Task 2 added an in-memory `LiveStateStore` (`Arc<RwLock<HashMap<RunId, TaskContext>>>`) giving the local Console a no-DB-poll read path for live run state. Task 3 added `crates/engine-serve/src/durable.rs` — an mpsc-bridged async durable-write seam (`DurableHandle`/`spawn_durable_writer`/`durable_on_progress`) mapping `on_progress` snapshots to `engine_contract::EventsRow`, inserting the first (all-PENDING) snapshot and updating subsequent ones via `engine_store`, self-skipping Postgres I/O (not failing) with no `DATABASE_URL`. Task 4 built the four-endpoint `actix-web` HTTP surface (`POST /events/` with `X-API-Key` gating, `GET /health`, `GET /workflows`, `GET /workflows/{type}/graph`), wiring the D3 HTTP-framework decision (recorded earlier this run) into the dispatch/live-state/durable modules from tasks 1–3. Task 5 added the headline integration test (`crates/engine-serve/tests/dispatch_integration.rs`) covering live-state reads with no DB query, byte-identical durable `EventsRow` mapping, and a 422 for an unregistered `workflow_type`. Task 6 confirmed all four validation gates pass clean (fmt, clippy `-D warnings`, `cargo test` — 22+19+3+1+1 tests green across the workspace, `cargo build --release`) with zero further code changes. Review verdict: **PASS** — no findings. Notable decisions: kept `Dispatcher::register` generic via a boxed `WorkflowFactory` closure rather than a `NodeRegistry`-sharing convenience method; used `uuid::Uuid` as the `RunId` type to match `EventsRow.id`'s existing type; built the `OnProgress` trait-object closure inside `web::block`'s blocking closure to keep the outer closure `Send` for actix; did not edit `workflow.rs` in task 3 since no genuine signature gap surfaced (per the spec's own guidance) — no deviations from the spec surfaced across the six tasks. Next: merge `EN.1.C-trigger-dispatch-serve-embedding-flow` into `main` and define the next Phase 1/2 block.

```
2c810dc chore: flow state — docs
c6bd73a docs: update docs for EN.1.C-trigger-dispatch-serve-embedding
a7164db chore: flow state — task 6 passed
e523911 chore: flow state — task 5 passed
96ea35c feat: implement EN.1.C-trigger-dispatch-serve-embedding-task5
b9461a1 chore: flow state — task 4 passed
fb8ae82 feat: implement EN.1.C-trigger-dispatch-serve-embedding-task4
```

---

## 2026-07-03

### Created GitHub remote, merged EN.1.B, cleaned up worktree, reconciled state.json, wrote handoff for EN.1.C
- **What:** Created this repo's first GitHub remote — `bredmond1019/engine-rs` (private) — matching the naming convention of sibling repos (bastion, bella, mev: plain name, private), and pushed `main` plus the feature branch. Ran `/code-review low` on the EN.1.B source diff (tests excluded) — no findings. Merged `EN.1.B-router-parallel-nodes-validator-flow` into `main` via `git merge --ff-only` (commit `43637e2`) and pushed the merge commit to `origin/main`. Removed the worktree and deleted the branch via `/clean-worktree`. Reconciled `planning/state.json`, which had an uncommitted, half-finished edit left over from a prior session (it closed EN.0.A/EN.0.B/EN.1.A but not EN.1.B, and still listed EN.1.B in `focus.next` even though it was now done): closed the EN.1.B block entry, removed it from `focus.next`/`focus.blocked`, and promoted EN.1.C to `focus.next` (no longer blocked by EN.1.B). Wrote a fresh `planning/handoff.md` pointing the next agent at EN.1.C (first command: `/generate-tasks EN.1.C`).
- **Why:** EN.1.B (Router trait + `as_router()` hook + `dispatch_route()`, `ParallelNode` fan-out/merge via `std::thread::scope` with deterministic last-write-wins merge, `WorkflowValidator` with BFS reachability + DFS cycle detection skipping router edges + non-router fan-out arity guard, and `Workflow::run` wired to dispatch through routers plus the new fallible `Workflow::new_validated()`) had just landed via `/sdlc-flow` and needed a clean merge to `main`, a real GitHub remote for the repo, and reconciled planning state before the next block could start.
- **Refs:** `planning/master-plan.md`, `planning/handoff.md`, `planning/state.json`

---

### Completed EN.1.B-router-parallel-nodes-validator end to end (5 tasks, PASS)
Ran `/sdlc-flow EN.1.B-router-parallel-nodes-validator` to completion across 5 tasks. Task 1 added a `Router` trait (supertrait of `Node`) with `route(ctx)` for runtime next-node selection, a `Node::as_router()` registry hook, and a `dispatch_route(&dyn Router, &TaskContext)` dispatch helper in `engine-core`. Task 2 added `ParallelNode` — fan-out over branch nodes via `std::thread::scope`, deep-copying `TaskContext` per branch, with deterministic last-write-wins merge of `nodes`/`node_runs` keyed by declared branch order — plus unit and integration tests. Task 3 added `WorkflowValidator` (BFS reachability from `start_node`, DFS cycle detection that skips edges declared out of router nodes, and a non-router fan-out arity guard) with a `ValidationError` enum and six unit tests covering valid and rejected schemas. Task 4 wired `Workflow::run` to call `Router::route(ctx)` for router nodes (supporting undeclared runtime back-edges) while plain nodes still walk `connections[0]`, and added a new fallible `Workflow::new_validated(registry, schema)` that runs the validator first — `Workflow::new` stayed infallible and unchanged, keeping the EN.1.A `tests/workflow_runner.rs` passing unmodified. Task 5 confirmed `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` all pass clean with no further changes. Review verdict: **PASS** — no findings. Notable decisions: router classification is purely via `NodeRegistry` lookup + `Node::as_router().is_some()`; validation runs arity → reachability → cycles in that order over sorted node keys for reproducible error reporting; DFS cycle detection skips walking connections declared out of a router node entirely (not just back-edges), matching the spec's "skips edges out of router nodes" language. No genuine deviations from the spec surfaced across the five tasks. Next: merge `EN.1.B-router-parallel-nodes-validator-flow` into `main` and define the next Phase 1 block.

```
6dd6ce5 docs: update docs for EN.1.B-router-parallel-nodes-validator
aa39cc0 chore: flow state — task 5 passed
92a0f36 chore: flow state — task 4 passed
45efffe feat: implement EN.1.B-router-parallel-nodes-validator-task4
cff7b3d chore: flow state — task 3 passed
a6180bc feat: implement EN.1.B-router-parallel-nodes-validator-task3
90bac7b chore: flow state — task 2 passed
301d926 feat: implement EN.1.B-router-parallel-nodes-validator-task2
```

---

## 2026-07-03

### Merged EN.1.A-node-trait-workflow-runner, cleaned up worktree, wrote handoff for EN.1.B
- **What:** Ran three SDLC pipeline blocks in sequence: `/sdlc-run EN.0.A-cargo-workspace-ci` (PASS — workspace scaffold, CI, D2 tokio+sqlx decision), `/sdlc-run EN.0.B-data-contract-postgres` (PASS — engine-contract serde types, engine-store Postgres layer), and `/sdlc-flow EN.1.A-node-trait-workflow-runner` (PASS, 5 tasks — `Node` trait, `NodeRegistry`, `WorkflowSchema`/`NodeConfig`, `Workflow` pointer-walk runner with `on_progress` seam). Discussed sqlx vs. Diesel along the way; kept D2 as-is, no new decision needed. Added `/trees` to `.gitignore` (commit `414b353`). Caught and fixed a docs bug before merging: `docs/architecture.md`'s Module Map / Build & CI sections still described `engine-contract`/`engine-store` as stubs even though EN.0.B gave them real types — corrected in the worktree (commit `bc2bd67`, "docs: correct engine-contract/engine-store stub description in architecture.md"). Ran `/code-review low` on the EN.1.A diff (source only) — no findings. Merged `EN.1.A-node-trait-workflow-runner-flow` into `main` via `git merge --no-ff` (merge commit `a7906cc`), deliberately choosing `--no-ff` over the skill's default `--ff-only` because the branch carried meaningful intermediate wrap-up/state commits worth preserving in history. Verified `cargo test --workspace` passes clean on `main` post-merge. Removed the worktree at `trees/EN.1.A-node-trait-workflow-runner-flow` and deleted the branch. Wrote `planning/handoff.md` for the next agent (first command: `/generate-tasks EN.1.B`). Added a `carryover[]` entry to `planning/state.json` (slug `state-json-block-status-stale`, kind `known_issue`): the `tracks[].blocks[]` status fields for EN.0.A/EN.0.B/EN.1.A still read `"open"` even though `planning/status.md`'s Progress Table marks all three Done — flagged so the next agent trusts `status.md` over `state.json`'s per-block status until reconciled.
- **Why:** Continuing the sequential SDLC drive through engine-rs's Phase 0/Phase 1 blocks per `master-plan.md`; the merge, worktree cleanup, and handoff close out EN.1.A cleanly so the next session can pick up EN.1.B with no loose state.
- **Refs:** `planning/master-plan.md`, `planning/handoff.md`, `planning/state.json` (carryover: `state-json-block-status-stale`)

---

## 2026-07-03

Completed EN.1.A-node-trait-workflow-runner end to end (implement → test → review → document → wrap-up) across 5 tasks. Task 1 added the `Node` trait (`process`/`name`, `Send + Sync`) and a `NodeRegistry` (`HashMap<String, Box<dyn Node>>`) in `engine-core`, backed by `engine-contract`'s `TaskContext`. Task 2 added `WorkflowSchema`/`NodeConfig` with helpers to resolve the start node and each node's `connections[0]` next-node. Task 3 added the `Workflow` pointer-walk runner (`crates/engine-core/src/workflow.rs`) that seeds all nodes PENDING before the walk, stamps RUNNING → SUCCESS/FAILED with timing on each `NodeRun`, invokes the `on_progress` persistence seam at every node boundary, and halts on node failure. Task 4 added a fixture 3-node linear integration test (`workflow_runner.rs`) covering full-success transitions, the initial PENDING `on_progress` snapshot, and a middle-node failure halting the walk. Task 5 confirmed all four validation commands (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`) pass clean with no further changes needed. Review verdict: PASS — all acceptance criteria MET, all gating checks green. Notable decisions: `NodeError` is a simple string-wrapping struct rather than an enum (minimal failure carrier for this block's scope); a node's own runtime failure is captured in its `NodeRun` and does not short-circuit `Workflow::run` with an `Err` — only unregistered-node graph-shape issues do. No genuine deviations from the spec — router/parallel-node branching and the acyclic validator remain out of scope for EN.1.B as planned. Next: define and run EN.1.B-router-parallel-nodes-validator (router + parallel nodes + validator).

```
add6862 chore: flow state — docs
c7e473c docs: update docs for EN.1.A-node-trait-workflow-runner
db55e50 chore: flow state — task 5 passed
96df0cd chore: flow state — task 4 passed
d022eec feat: implement EN.1.A-node-trait-workflow-runner-task4
4a169e0 chore: flow state — task 3 passed
67ef4bf feat: implement EN.1.A-node-trait-workflow-runner-task3
5be0e37 chore: flow state — task 2 passed
```

---

## 2026-07-02

Completed EN.0.B-data-contract-postgres end to end (implement → test → review → document → wrap-up). Implemented the preserved data-contract seam in `engine-contract` — `NodeRunStatus` (lowercase `pending|running|success|failed`), `Usage`, `NodeRun` (always-present-but-nullable `started_at`/`completed_at`/`error`/`input`/`usage`), `TaskContext`, and `EventsRow` (`id`, `workflow_type`, `data`, `task_context`, `created_at`, `updated_at`) — matching `orchestrator/docs/data-contract.md` v1.0.1 field-for-field. Added a byte-for-byte round-trip test against a captured Python-shaped fixture plus a Rust-constructed shape assertion, both passing with no field/casing/type drift. Implemented `engine-store`'s Postgres layer (`connect`, `insert_event`, `update_event`, `get_event`) on the D2-pinned `sqlx::PgPool` stack, with a live round-trip test that self-skips (not fails) when `DATABASE_URL` is unset so EN.0.A's Postgres-less CI stays green. Review verdict: PASS — all 6 acceptance criteria MET, all 4 gating checks (fmt, clippy, test, build --release) green, 16 tests total. `docs/architecture.md` was flagged NEEDS_REVIEW (module map / Core Types / Build & CI sections still describe stubs) rather than edited directly, since it's a top-level architecture doc. No genuine deviations from the spec — the always-present-but-null `NodeRun` field serialization was in-scope work needed to satisfy the byte-for-byte acceptance criterion, not a scope change. Next: define and run EN.1.A-node-trait-workflow-runner (Node trait + Workflow runner).

```
9347681 docs: update docs for EN.0.B-data-contract-postgres
a7cbb55 feat: implement EN.0.B-data-contract-postgres
63f6996 chore: add spec for EN.0.B-data-contract-postgres
f2bb90c chore: wrap up EN.0.A-cargo-workspace-ci
9f7f1b8 docs: update docs for EN.0.A-cargo-workspace-ci
```

---

## 2026-07-02

Completed EN.0.A-cargo-workspace-ci end to end (implement → test → review → document → wrap-up). Stood up the `engine-rs` Cargo workspace with four member crates (`engine-core`, `engine-contract`, `engine-store`, `engine-serve`), each carrying a compiling `src/lib.rs` stub with a trivial passing test. Added `.github/workflows/ci.yml` running fmt/clippy/test/build on push and pull_request, matching `planning/harness.json`'s validation gates exactly. Recorded the async-runtime + persistence stack as decision `D2-async-runtime-choice.md` (tokio + sqlx with postgres/runtime-tokio/tls-rustls features), linked from `planning/decisions/index.md`. Review verdict: PASS — all 6 acceptance criteria MET, all 4 gating checks (fmt, clippy, test, build --release) green. `docs/architecture.md` patched with the confirmed Module Map and a new Build & CI section documenting D2 and the CI gates; no NEEDS_REVIEW flags. No genuine deviations from the spec — the async-runtime decision was in-scope work, not a scope change. Next: define and run EN.0.B-data-contract-postgres (data-contract serde types + Postgres round-trip).

```
9f7f1b8 docs: update docs for EN.0.A-cargo-workspace-ci
1a59a44 feat: implement EN.0.A-cargo-workspace-ci
cdc9133 chore: add spec for EN.0.A-cargo-workspace-ci
```

---

## 2026-07-02

Project initialized from `base-template` (commit `7f2cbada68bdb0433133cf213777994030f7b7d6`) via `/new-project`.
Planning infrastructure scaffolded: `planning/context.md`, `planning/status.md`,
`planning/master-plan.md`, `planning/index.md`, `planning/harness.json`, `planning/decisions/`,
and the root `CLAUDE.md` / `README.md`. Concept folders (`planning/<concept>/`) are created on
demand by the SDLC pipeline. Curated SDLC harness (`.claude/`) in place.

Next step: run `/generate-tasks` for the first Phase 0 block to begin the pipeline.

```diff
(no code changes — planning files only)
```
