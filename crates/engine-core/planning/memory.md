---
type: Reference
title: engine-core (crate-local) Memory
description: Repo-scoped durable memory for the stray crate-local planning root at crates/engine-core/planning — episodic notes, gotchas, and superseded facts for the blocks executed here.
doc_id: memory
layer: [engine]
project: engine-rs
status: active
keywords: [memory, episodic, gotchas, durable, engine-core, crate-local-planning]
related: [knowledge, archive-index]
---

# Memory — crates/engine-core/planning (crate-local)

Episodic notes and gotchas for the three SDLC blocks that used this crate subdirectory as their
planning root instead of the repo's canonical `planning/` (symlink into `core/_planning/engine-rs/`).

## Notes

_Dated episodic entries — what was tried, what was decided in-flight, what to remember next time._

- **This directory (`crates/engine-core/planning/`) is a stray, non-canonical planning root.** It is a real, tracked, non-`.gitignore`d directory inside `engine-rs`'s own git repo — unlike the repo-root `planning/` (a symlink into the company brain's `core/_planning/engine-rs/`, gitignored per `engine-rs/CLAUDE.md`). Three blocks (`EN.5.D-policy-dispatch-seam`, `EN.5.F-async-run-lifecycle`, `EN.6.A-egress-dispatch`) ran their SDLC flow with this nested crate directory as cwd/planning-root instead of the repo root, so their `tasks.md`/`sdlc/worklog.md`/`sdlc-flow-state.json` landed here and got committed directly into `engine-rs`'s own history rather than the brain-managed vault. All three blocks' outcomes were independently narrated in `core/_planning/engine-rs/status.md` (the canonical root) at the time, so no execution knowledge was lost — but the raw task/worklog artifacts sat orphaned here, outside the brain's index, until this `/archive` pass. **Guardrail candidate:** an SDLC-flow precondition check that refuses to scaffold a fresh `planning/<spec>/` inside a nested crate directory when a repo-root `planning/` symlink already exists, to prevent this recurring.
  source: crates/engine-core/planning/{EN.5.D-policy-dispatch-seam,EN.5.F-async-run-lifecycle,EN.6.A-egress-dispatch} · date: 2026-08-01 · supersedes: — · freshness: 2026-08-01
- **`core/_planning/engine-rs/knowledge.md` cites source paths under `core/_planning/engine-rs/EN.5.F-async-run-lifecycle/...` and `.../EN.7.A-materialize-doc-node/...` for content that actually originates from this crate-local location (`EN.5.F`) or is otherwise fine (`EN.7.A` does exist at top level).** The `EN.5.F` citation path in the top-level `knowledge.md` is stale/incorrect — the real source is `crates/engine-core/planning/EN.5.F-async-run-lifecycle/tasks.md` (this repo's crate-local tree), since no `EN.5.F-async-run-lifecycle` directory ever existed under `core/_planning/engine-rs/`. Left uncorrected by this archive pass (out of scope — that file belongs to a different planning root), but flagged here so a future brain-hygiene sweep can fix the citation.
  source: core/_planning/engine-rs/knowledge.md (Conventions section, "AppState/config structs..." entry) · date: 2026-08-01 · supersedes: — · freshness: 2026-08-01
- **Fire-and-forget in-process dispatch must go through `spawn_blocking` + a fresh runtime, not a direct `.await`.** `Workflow::run`'s `OnProgress` closure is `!Send`, so its future cannot cross an await point inside a `Send`-bound `#[async_trait]` method (`ChannelTransport::send`) — discovered while wiring `WorkflowTriggerDispatch`'s in-process `Dispatcher` preference. This mirrors `EN.5.F`'s own non-blocking `/events/` contract (the receipt reflects the handoff, not the child run's outcome).
  source: EN.6.A-egress-dispatch/sdlc/worklog.md (task 2) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **A test asserting an env-var-driven, process-memoized default (`OnceLock`) must not override the env var per-test.** `EN.5.F`'s budget-halt test instead drove the failure via a fixture node reporting `cost_usd=10.0` (exceeding the existing $5 default), because `default_budget_from_env()` memoizes on first call and all tests in the integration binary share one process — a per-test env override would race whichever test calls it first. Reuse this pattern (drive the *effect*, not the *env var*) for any future test against a `OnceLock`-memoized config default.
  source: EN.5.F-async-run-lifecycle/sdlc/worklog.md (task 5) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01

## Preferences

_Project-specific preferences (tooling, style, workflow) the operator has expressed._

_None specific to this crate-local planning root beyond what's already recorded in
`core/_planning/engine-rs/memory.md`._
