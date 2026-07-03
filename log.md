---
type: Log
title: engine-rs Development Log
description: Chronological log of work completed for engine-rs.
doc_id: log
layer: [factory]
status: active
timestamp: "2026-07-02"
keywords: [work log, session history, development log]
related: [status, context]
---

# Log — engine-rs

*Append-only working log. One dated entry per session. Newest entries at the top.*

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
