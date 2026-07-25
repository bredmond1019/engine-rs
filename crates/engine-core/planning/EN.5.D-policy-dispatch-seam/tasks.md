---
type: Plan
title: "Task Spec — EN.5.D Policy/telemetry productionization"
description: Decomposed task spec for making the EN.4.0 policy framework reachable from the served path and making telemetry record fact rather than intent.
doc_id: en-5-d-policy-dispatch-seam-tasks
layer: [engine]
project: engine-rs
status: active
keywords: [policy, dispatch, telemetry, local model tier, overlay, profiles]
related: [master-plan, D7-shared-policy-framework, D4-claude-code-transport-choice]
---

# Task Spec — Phase 5, Block D — Policy/telemetry productionization

**Status:** Not started · **Last run:** never

## Goal

Close the gap between `EN.4.0`'s policy framework and what a *served* run actually does: make
`WorkflowFactory` policy-aware, resolve policy once per run at dispatch, fail loudly instead of
silently defaulting, decouple config lookup from a worktree, collapse the duplicated merge trio into
a shared `Overlay`, and stamp/harvest the model tier a stage *actually called* on every run.

## Context Pointers

- **Block definition:** `planning/master-plan.md` → *Phase 5* → `### EN.5.D — Policy/telemetry
  productionization (make the local-model swap reachable)` (the six numbered changes, the Files
  list, and the Out-of-scope boundary are authoritative).
- **Framework being productionized:** `crates/engine-core/src/policy/{mod,resolve,profiles,tier,
  telemetry,aggregate,shaping}.rs` (`EN.4.0`), and `planning/decisions/D7-shared-policy-framework.md`.
- **The duplicated trio to collapse:** `crates/engine-core/src/workflows/{sdlc_flow,research_agent,
  diagnostic_intake,proposal_generator}/policy.rs` — each defines its own private
  `merge_opt`/`merge_local`/`apply_override` (`sdlc_flow/policy.rs:176,199,245`;
  `proposal_generator/policy.rs:123,146,171`; `research_agent/policy.rs:95,109,134`;
  `diagnostic_intake/policy.rs:95,106,131`).
- **The zero-argument factory:** `crates/engine-serve/src/dispatch.rs:19`
  (`pub type WorkflowFactory = Box<dyn Fn() -> Workflow + Send + Sync>`), its registrations in
  `crates/engine-serve/src/workflows.rs`, and the trigger path in
  `crates/engine-serve/src/http.rs:110` (`post_events` → `dispatcher.dispatch(&body.workflow_type)`,
  with the event payload sitting unread in `body.data`).
- **The policy-aware registries that no served run reaches today:**
  `workflows/{sdlc_flow,research_agent,diagnostic_intake,proposal_generator}/graph.rs`'s
  `registry_for_policy` — every caller is a unit test or an `#[ignore]`d experiment.
- **Per-node re-resolution being replaced:** `resolve_policy_for_run(&ctx, &worktree)` called inside
  `process()` in `research_agent/{company_research,prospecting}.rs`,
  `diagnostic_intake/extract.rs`, `proposal_generator/{company_research,writer,...}.rs`, plus the
  worktree-derivation helper `worktree_path(ctx)` duplicated in those modules, and the stamp lane
  `crate::policy::{stamp_resolved_policy, resolved_policy, RESOLVED_POLICY_IDENTITY}`
  (`policy/profiles.rs:24-47`).
- **Telemetry:** `crates/engine-core/src/policy/telemetry.rs` (`RunTelemetry`,
  `RunTelemetryInputs.model_tier_used` — currently caller-derived from the *resolved* policy), and
  the run loop that would emit it, `crates/engine-core/src/workflow.rs:149` (`run_with`).
- **Transports that must stamp what they called:** `crates/engine-core/src/nodes/claude_code_step.rs`
  (writes `{content, cost_usd, model, structured}` at `:193`) and
  `crates/engine-core/src/nodes/openai_compat_transport.rs` (falls back to cloud silently when the
  local endpoint is unreachable — the fallback is exactly the case telemetry must record).
- **Standing rules:** `CLAUDE.md` — every block ships tests; decisions are append-only new files in
  `planning/decisions/` (next free id is `D11`); every new `.md` under `docs/`/`planning/` opens with
  OKF frontmatter and its directory `index.md` is updated.
- **Validation suite:** `planning/harness.json` → `validation.checks[]` (all four gate).

## Step-by-Step Tasks

See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria

- Triggering `PROPOSAL_GENERATOR` through `POST /events/` with a body whose `data` carries a
  local-routing profile (e.g. `{"profile": "local-judgment"}`) produces a run whose judgment stages
  hit the local OpenAI-compat endpoint — an assertion that fails against `main` today.
- An unknown profile name sent over `POST /events/` returns an error response (4xx) rather than
  silently resolving to builtin defaults; the error names the offending profile.
- A workflow with no worktree path (no `SetupWorktreeNode` output, no repo) resolves policy
  successfully through the worktree-free config source rather than falling back to
  `std::env::current_dir()`.
- No `crates/engine-core/src/workflows/*/policy.rs` contains a hand-written
  `merge_opt`, `merge_local`, or `apply_override` — all four delegate to the shared
  `policy::overlay` surface (assertable by grep and by the four workflows' existing policy tests
  still passing unchanged).
- Policy is resolved **once per run** at dispatch and stamped through `RESOLVED_POLICY_IDENTITY`;
  no node calls `resolve_policy_for_run` inside `process()`, and reading an absent/unparsable stamp
  is an error rather than a silent `Default`.
- `model_tier_used` is derived from the transport's stamped output, asserted by a test in which the
  resolved tier and the endpoint actually called disagree (the `openai_compat_transport` cloud
  fallback path).
- Every completed run's `TaskContext.metadata` carries a `RunTelemetry` block — not only runs under
  the `#[ignore]`d experiment harnesses.
- The four-layer `builtin < harness < profile < event` precedence is preserved, and the
  `TaskContext`/`EventsRow` wire shape is unchanged (round-trip tests still pass).
- The existing `#[ignore]`d profile-ranking experiments (`sdlc_flow_experiment.rs`,
  `proposal_generator_e2e.rs`'s gated harness) still compile and pass unchanged.
- `planning/decisions/D11-policy-dispatch-seam.md` exists with OKF frontmatter and is listed in
  `planning/decisions/index.md`.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release` all pass.

## Validation Commands

```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

## Notes

- `bastion` consumes `engine-serve` through an **unpinned path dep**, so the `WorkflowFactory`
  signature change is a cross-repo break by construction. Keep `Dispatcher::register`'s ergonomics
  close to today's and prefer adding a sibling entry point over gratuitously renaming
  `dispatch`/`resolve_schema`.
- `openai_compat_transport`'s silent cloud fallback is deliberate (a down local server must not fail
  a run). That is exactly why `model_tier_used` must come from the transport's stamp rather than the
  resolved policy — the fallback is invisible in intent-derived telemetry.
- Config-source and stamped-policy changes should land **additively first** (new fallible/`_from`
  entry points alongside the existing ones), with the lenient originals deleted in the migration
  task, so the tree compiles at every task boundary.

## Amendment Log

<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
