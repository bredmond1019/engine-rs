---
type: Reference
title: The ORCHESTRATION workflow
description: How engine-rs sequences SDLC_FLOW runs across repos from a lane chain — the gates it applies before each block, the policy it resolves, and the closed engine type that bounds what it can invoke.
doc_id: orchestration-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [orchestration, lane chain, admission control, operator hold, sanctioned engines, sdlc flow]
related: [architecture, sdlc-flow-workflow, sdlc-flow-policy, terminal-crates, orphan-recovery]
---

# The ORCHESTRATION workflow

`ORCHESTRATION` takes a **lane chain** — a roadmap plus a lane name, or an explicit block list — and
drives one `SDLC_FLOW` run per block, in order, across more than one repo.

It exists because the mechanical half of driving a lane is a set of predicates over a graph: resolve
the chain, check dependencies, pick the engine, run one block at a time, verify the state write,
append to the lane log. None of that needs judgement. What needs judgement is what to do when one of
those predicates fails, and that stays with a human.

**Workflow type:** `ORCHESTRATION` · **Node:** `OrchestrationRunNode` ·
**Source:** `crates/engine-core/src/workflows/orchestration/`

## Why a workflow and not a long-lived session

Claude Code reads its rules and harness from its working directory, so **a session cannot span
repos** — but a workflow can. `ORCHESTRATION` spawns a short-lived, cwd-scoped run per block, each
pointed at that block's own repo. The practical consequence: lane length stops being a property of
the driver. A twelve-block chain across four repos is the same shape as a two-block chain in one.

## What happens per block

| Stage | Module | What it does |
|---|---|---|
| Resolve | `chain.rs` | Turns a (roadmap, lane) pair or an explicit block list into ordered `(repo, block_id)` steps. Reads mev's structured `HELD-UNTIL` / `BUDGET` / `EXCLUSIVE-REPOS` directives and `planning/lane-segments.json` — it does not re-derive segments. |
| Gate | `gates.rs` | Resolves every `depends_on` edge against the **live graph** and refuses to start a block with an unmet edge, naming the edge and its repo. Then consults admission control: **at capacity the run waits** — it does not proceed and does not fail. |
| Execute | `execute.rs` | Invokes the existing `SDLC_FLOW` workflow in a short-lived run with `cwd` set to that block's repo. Selects the engine from the block's own authored `sdlc_workflow` field. |
| Integrate | `integrate.rs` | Verifies the state write after the engine returns and **fails the run loudly on a mismatch**. Appends exactly one line to the roadmap's `lane-log.jsonl`. An operator hold pauses and resumes without re-running completed blocks. |

Readiness always comes from the graph, never from a roadmap's hand-written wave table. A roadmap is
an authored snapshot and has been wrong; the `depends_on` edges are the fact.

## The lane-log contract

Exactly one line per integrated block — not zero, not two. The log is the cross-lane channel, so a
missing or duplicated line is how a sibling lane reads the wrong state. The roadmap directory is
resolved by the two-location rule (`planning/roadmaps/<slug>/` first, then legacy `planning/<slug>/`;
a slug present in both is an error, never a silent preference).

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
