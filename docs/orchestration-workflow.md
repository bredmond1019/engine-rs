---
type: Reference
title: The ORCHESTRATION workflow
description: How engine-rs sequences SDLC_FLOW and SDLC_TASK runs, plus dispatched in-process workflows, across repos from a lane chain — the gates it applies before each step, the policy it resolves, and the closed engine type that bounds what it can invoke.
doc_id: orchestration-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [orchestration, lane chain, admission control, operator hold, sanctioned engines, sdlc flow, sdlc task, dispatch step]
related: [architecture, sdlc-flow-workflow, sdlc-flow-policy, terminal-crates, orphan-recovery]
---

# The ORCHESTRATION workflow

`ORCHESTRATION` takes a **lane chain** — a roadmap plus a lane name, or an explicit block list — and
drives one run per block, in order, across more than one repo. Each block's own authored
`sdlc_workflow` field selects whether that run is `SDLC_FLOW` or `SDLC_TASK` — a chain can freely mix
both (see "A chain may mix `task` and `flow` blocks" below).

It exists because the mechanical half of driving a lane is a set of predicates over a graph: resolve
the chain, check dependencies, pick the engine, run one block at a time, verify the state write,
append to the lane log. None of that needs judgement. What needs judgement is what to do when one of
those predicates fails, and that stays with a human.

**Workflow type:** `ORCHESTRATION` · **Node:** `OrchestrationRunNode` ·
**Source:** `crates/engine-core/src/workflows/orchestration/`

## What it actually invokes — read this before extending it

`ORCHESTRATION` calls **native Rust workflows in-process** — `SDLC_FLOW` for a `flow` block,
`SDLC_TASK` for a `task` block. It does not open a Claude Code session and type `/sdlc-flow` or
`/sdlc-task`, and it does not shell out to the JS engines under `.claude/workflows/`.

Per step, `execute.rs` resolves the step's repo slug through the injected `RepoRegistry` to an
absolute path, builds a **fresh** `Workflow` for that block's own engine (policy-aware registry +
schema, registered with that same registry so `SetupWorktreeNode` resolves `event.repo` too), seeds
`event.repo` on the dispatched event, and runs it to completion. Nothing is kept alive or reused
between steps.

Claude Code sessions *do* happen — one layer down. `SDLC_FLOW`'s own model-bearing nodes
(`ClaudeCodeStep` -> `claude_code_rs::execute`, per `D4`) spawn them for the implement, review and
docs stages. So the call stack is:

```
ORCHESTRATION (Rust)
  └─ per block: SDLC_FLOW (Rust, fresh instance, cwd = that block's repo)
       └─ per stage: ClaudeCodeStep -> a Claude Code session
```

Both layers read a repo's harness and `CLAUDE.md` from the **working directory**, which is why a
session cannot span repos but a workflow can. That is what removes the driver as the ceiling on lane
length: a twelve-block chain across four repos is the same shape as a two-block chain in one.

## A chain may mix `task` and `flow` blocks

`EngineKind` has two variants, `Flow` and `Task`, and **both are runnable** (`EN.11.P`). The engine
is resolved per block, from that block's own authored `sdlc_workflow` field, via
`EngineKind::from_sdlc_workflow` — never fixed for the whole chain. A chain can freely interleave
`task` and `flow` blocks; each step in `execute.rs` dispatches to whichever engine its own block
declares.

A block whose authored `sdlc_workflow` falls outside the closed `{task, flow}` vocabulary — absent,
a typo, or a value like `sdlc-run`/`sdlc-block` that this engine deliberately does not support —
still fails loudly with `ExecuteError::UnsupportedEngine` naming the block and repo. It does not
silently fall through to `Flow`, and it does not panic. Check the `sdlc_workflow` field of every
block in a chain if you need to know in advance whether it is runnable — a missing or unsupported
value is the only thing that still stops a block.

Each engine writes and is verified against its own state file, so a `task` step and a `flow` step
against the same `planning/<slug>/sdlc/` directory never collide: `sdlc_flow::DEFAULT_STATE_FILENAME`
(`sdlc-flow-state.json`) for `Flow`, `sdlc_task::DEFAULT_STATE_FILENAME` (`sdlc-task-state.json`) for
`Task`. `integrate.rs`'s `state_path_for` selects the filename from the step's own `EngineKind`
before reading the state write back.

A `task` block whose reconcile failed is a distinct, **terminal** case: `sdlc_task::lean_bookkeep`
writes `status: "reconcile_failed"` to its state file and deliberately skips the block-status flip,
so the block is genuinely not closed. Integrating that as a success would be exactly the silent
unreliability this module's state-write verification exists to prevent, so it instead surfaces
`IntegrateError::ReconcileFailed` and **stops the chain** — the same terminal treatment as any other
integration failure, not a warning that lets the chain continue past an unclosed block.

**Scope actually exercised so far** (`EN.11.P`, tests added `crates/engine-core/tests/it/sdlc_task_e2e.rs`):
a two-block chain of one `task` block plus one `flow` block completing with `steps_integrated == 2`;
a `task` block with a failed reconcile stopping the chain; a `flow`-only chain unchanged. All three
run against **tempdir fixture repos** — see "Status: not yet exercised on a real chain" below, which
still applies to a *mixed* chain exactly as it does to a flow-only one. No corpus-wide percentage of
"how much is now drivable" is restated here; the figure would need to be re-measured against the
current corpus at the time it's cited, and that re-measurement is out of scope for this rewrite.

## A chain may also mix `block` and `dispatch` steps (`EN.12.E`)

Every step in a chain also carries a `kind`, separate from the `EngineKind` question above.
`kind` answers "is this step an SDLC spec at all, or a registered in-process workflow?" — while
`EngineKind` (previous section) only ever answers "which SDLC engine" for a step that already is
one. A `ChainStep`'s `kind` is `StepKind::Block` by default, so an existing chain with no `kind`
field behaves exactly as before this feature; `StepKind::Dispatch` opts one step into the
behavior below (`StepKind::Command` is reserved for a future block).

- **A `block` step** is everything described above — one `SDLC_FLOW`/`SDLC_TASK` run against a
  corpus block, gated, executed, and integrated the normal way.
- **A `dispatch` step** runs a **registered in-process workflow** instead — one of the workflows
  already registered with the same `Dispatcher` `engine-serve::workflows` populates (for example
  `RESEARCH_AGENT` or `CONTENT_PIPELINE`; see `docs/architecture.md`'s `Dispatcher` entry). It
  never opens a Claude Code session, never selects an `EngineKind`, and never falls through to a
  block invocation — those are separate code paths (`dispatch.rs`'s `execute_dispatch_step`, not
  `execute.rs`'s `execute_step`).

A dispatch step reuses `ChainStep::block_id` as the `Dispatcher` registry key (`workflow_type`,
e.g. `"RESEARCH_AGENT"`) rather than adding a new field — `dispatch.rs`'s `workflow_key` accessor
names that reuse so a reader never has to infer it from the field name. An unregistered key stops
the chain loudly with `DispatchStepError::UnknownWorkflowKey`, naming the step's `block_id` and
the key it resolved to; it never silently proceeds to the next step or falls back to a block run.

**A dispatch step's outcome is recorded in the journal, not `lane-log.jsonl`.** `integrate.rs`
routes a `Dispatch` step to `execute_dispatch_step` and records the result as a `JournalRow` (the
same `StepIntegrated`/`StepBailed` decision kinds a block step's integration uses — see
`docs/architecture.md`'s Journal entry) through a new opt-in entry point,
`integrate_chain_with_dispatch`. It does not push an `ExecutionOutcome`, write a checkpoint entry,
or call `step_observer` — there is no SDLC state write to make resumable and no `ExecutionOutcome`
shape to fabricate for a workflow that was never an SDLC run. Calling a chain with a dispatch step
through the older `integrate_chain`/`integrate_chain_with_journal` entry points (no `Dispatcher`
supplied) fails loudly with `IntegrateError::NoDispatcherConfigured` rather than silently skipping
the step.

## What happens per block

| Stage | Module | What it does |
|---|---|---|
| Resolve | `chain.rs` | Turns a (roadmap, lane) pair or an explicit block list into ordered `(repo, block_id)` steps. Reads mev's structured `HELD-UNTIL` / `BUDGET` / `EXCLUSIVE-REPOS` directives and `planning/lane-segments.json` — it does not re-derive segments. |
| Gate | `gates.rs` | Resolves every `depends_on` edge against the **live graph** (backed by `corpus_gates.rs`, which reads each repo's real `planning/state.json` through `okf_core::load_state`) and refuses to start a block with an unmet edge, naming the edge and its repo. `DependencyEdge` is an enum — `Block` · `Operator { slug }` · `Approval { slug }` · `External { what }` — so an **operator gate is always unmet while present** and clears only by removal from the corpus (mev is the single writer); the engine can never self-clear one. Also reads mev's `lane-frontier.json` for lane-head startability, but `startable: true` never short-circuits the per-edge check. Then consults admission control: **at capacity the run waits** — it does not proceed and does not fail, and a block parked on an operator hold releases its permit rather than starving the ceiling. |
| Execute | `execute.rs` | For a `block`-kind step: builds and runs a **fresh in-process Rust `Workflow`** for whichever engine the block's own authored `sdlc_workflow` field names — `SDLC_FLOW` or `SDLC_TASK` — with the repo resolved through `RepoRegistry` and `event.repo` seeded. An unsupported or absent `sdlc_workflow` value errors (see above). A non-`Block` `kind` reaching `execute_step` is refused with `ExecuteError::WrongStepKind` — a `dispatch` step is routed elsewhere (below), never through this path. |
| Dispatch | `dispatch.rs` | For a `dispatch`-kind step (`EN.12.E`, see "A chain may also mix `block` and `dispatch` steps" below): resolves the step's `block_id` as a `Dispatcher` registry key and runs the registered in-process workflow to completion, never selecting an `EngineKind`. |
| Integrate | `integrate.rs` | Verifies the state write after a `block` step's engine returns and **fails the run loudly on a mismatch** — including a `status: "done"` run whose `final_validation.all_passed` is `false`, and a state file whose `block_id` does not match the executed block. Before each block, re-checks the run's `cancellation_token` and a campaign-scoped `CampaignLedger` against an optional `campaign_budget` ceiling (`EN.11.F`) — both checked again at every block boundary, not just once at the start. Appends exactly one `lane-log.jsonl` line per `block` step in the on-disk contract shape `{ts, lane, repo, block, status, note}` with `status` a typed `closed` \| `bailed` \| `held` \| `cancelled` \| `budget_halted`; a **failed** step appends a `bailed` line before the error propagates, so an attempted block is never silent. A `dispatch` step's outcome is journaled instead, not appended to `lane-log.jsonl` (see below). An operator hold pauses and resumes without re-running completed blocks, under a deadline rather than an unbounded poll. |

Readiness always comes from the graph, never from a roadmap's hand-written wave table. A roadmap is
an authored snapshot and has been wrong; the `depends_on` edges are the fact.

## The lane-log contract

Exactly one line per integrated block — not zero, not two. The log is the cross-lane channel, so a
missing or duplicated line is how a sibling lane reads the wrong state. The roadmap directory is
resolved by the two-location rule (`planning/roadmaps/<slug>/` first, then legacy `planning/<slug>/`;
a slug present in both is an error, never a silent preference).

A clean abort, a budget halt, and a node/state-write failure are three distinguishable terminal
states in that log, not one undifferentiated stop (`EN.11.F`): a chain halted by
`POST /campaigns/{id}/abort` (an explicit human request) appends a `cancelled` line; a chain
halted because the campaign-scoped `CampaignLedger` tripped its `Budget` ceiling appends a
`budget_halted` line naming the tripped cap; either way, blocks already integrated keep their
`closed` line and no block still running or not yet started is touched. See "Campaign identity"
below for how a campaign's abort token and cost/token ceiling are threaded through a chain.

## Campaign identity

Every `ORCHESTRATION` run resolves a `campaign_id` (`EN.11.E`) — the event's own `campaign_id` when
present (so a resumed or operator-restarted chain rejoins the same campaign instead of minting a
new identity indistinguishable from a fresh one), else a fresh v4 UUID minted at run start. Each
`execute.rs` step threads that same `campaign_id` onto the child `SDLC_FLOW` event it dispatches
(`event.campaign_id`), and stamps it back onto its own step record so the step is attributable to
its campaign without re-reading the child's `TaskContext`. The parent run additionally stamps
`campaign_members` — the per-step roster — into `ctx.nodes[OrchestrationRunNode]`, next to the
existing `steps_integrated`/`blocks`/`policy`/`cancellation` fields.

`GET /campaigns/{id}` (`engine-serve`, task 5) reads this identity back: it resolves every run —
live or completed — carrying the given `campaign_id` (via `LiveStateStore::list_campaign_runs`) and
rolls up their cost/tokens from the parent run's `campaign_members` entry. See `docs/architecture.md`
(HTTP surface, `LiveStateStore`) for the endpoint and store shape, and `docs/data-contract.md` §8 for
the canonical wire shape.

`POST /campaigns/{id}/abort` (`engine-serve`, `EN.11.F` task 2) gives a human a way to stop a whole
campaign — every block in the chain, not just the block currently running. A campaign-scoped
`CancellationToken`, registered in `CampaignRegistry` under the campaign's id, is what
`integrate.rs`'s per-boundary check (above) observes. See `docs/architecture.md`'s "Campaign abort
endpoint" entry for the route contract.

## Policy

One knob today, resolved through the standard four layers
(per-run event override > named profile > `planning/harness.json` > built-in default):

| Knob | Default | What it trades |
|---|---|---|
| `hold_poll_interval_ms` | `2000` | How often a paused run checks whether an operator hold has cleared. Lower notices a clearance sooner at the cost of more wake-ups. |

Named profiles (`crates/engine-core/src/workflows/orchestration/graph.rs`):

- **`baseline`** — `2000ms`. Spelled out explicitly rather than left empty, so selecting it is a
  legible, self-documenting no-op against the built-in default.
- **`cheap-fast`** — `10000ms`. Fewer wake-ups; a cleared hold is noticed later.
- **`thorough`** — `500ms`. A cleared hold is noticed almost immediately; more wake-ups.

Defaults are also written into `planning/harness.json` under `orchestration`, so the knob is
discoverable without reading the Rust.

## Only sanctioned engines are reachable

The block-execution seam takes a **closed two-variant type**, `EngineKind::{Task, Flow}` — not a
command string, not a validated `&str`. Any other runner is *structurally unrepresentable*.

A block whose authored `sdlc_workflow` falls outside `{task, flow}` produces a diagnostic and does
not run. It never silently defaults. `sdlc-run` and `sdlc-block` are deliberately unsupported here:
they have different isolation and merge semantics than a chain can safely assume.

This is enforced as code rather than convention because the failure it prevents is invisible. A block
built outside the engines has no spec, no gate, no review and no honest state write — and the chain's
own verification still looks fine, because the state write looks fine. A guard test scans every file
in the module and fails if a string-typed runner is reintroduced anywhere in it.

> **Note on that guard's history.** It originally scanned only its own file, so an escape added to
> `execute.rs` — the actual block-execution seam — passed clean. It now covers the whole module, with
> a per-file allowlist of legitimate string-taking entry points. The lesson generalises: a gate must
> be shown failing *for the surface its criterion names*, not merely shown failing.

## Status: not yet exercised on a real chain

Every acceptance criterion is covered by integration tests against **tempdir fixture repos**. As of
2026-08-18 `ORCHESTRATION` has never sequenced a real block in a real repo.

Treat the first real run as a test, not as routine — the same posture the brain root's `CLAUDE.md`
prescribes for the first `/orchestrate` run in HQ, and with more force here because this one drives
other engines. A short **two-block, single-repo** chain where a failure is cheap to unwind is the
right first target; a cross-repo lane is not.
