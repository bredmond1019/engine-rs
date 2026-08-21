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

## What it actually invokes — read this before extending it

`ORCHESTRATION` calls the **native Rust `SDLC_FLOW` workflow in-process**. It does not open a Claude
Code session and type `/sdlc-flow`, and it does not shell out to the JS engines under
`.claude/workflows/`.

Per step, `execute.rs` resolves the step's repo slug through the injected `RepoRegistry` to an
absolute path, builds a **fresh** `SDLC_FLOW` `Workflow` (policy-aware registry + schema, registered
with that same registry so `SetupWorktreeNode` resolves `event.repo` too), seeds `event.repo` on the
dispatched event, and runs it to completion. Nothing is kept alive or reused between steps.

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

## Only `Flow` runs today — `Task` is authored but unsupported

`EngineKind` has two variants, but **only `EngineKind::Flow` is runnable**, because only `SDLC_FLOW`
has been ported to this engine. There is no Rust `SDLC_TASK` workflow. A block authored
`sdlc_workflow: "task"` fails loudly with `ExecuteError::UnsupportedEngine` naming the block and repo
— it does not silently fall through to `Flow`, and it does not panic.

This is a real gap between "the workflow exists" and "the workflow can drive your lane." Concretely:
of the nine blocks the `engine` lane closed on 2026-08-18, **four were authored `task`**
(`EN.9.F`, `EN.10.C`, and two adopted tickets), so `ORCHESTRATION` as it stands could have driven 5
of 9. Check the `sdlc_workflow` field of every block in a chain before assuming it is runnable.

## What happens per block

| Stage | Module | What it does |
|---|---|---|
| Resolve | `chain.rs` | Turns a (roadmap, lane) pair or an explicit block list into ordered `(repo, block_id)` steps. Reads mev's structured `HELD-UNTIL` / `BUDGET` / `EXCLUSIVE-REPOS` directives and `planning/lane-segments.json` — it does not re-derive segments. |
| Gate | `gates.rs` | Resolves every `depends_on` edge against the **live graph** (backed by `corpus_gates.rs`, which reads each repo's real `planning/state.json` through `okf_core::load_state`) and refuses to start a block with an unmet edge, naming the edge and its repo. `DependencyEdge` is an enum — `Block` · `Operator { slug }` · `Approval { slug }` · `External { what }` — so an **operator gate is always unmet while present** and clears only by removal from the corpus (mev is the single writer); the engine can never self-clear one. Also reads mev's `lane-frontier.json` for lane-head startability, but `startable: true` never short-circuits the per-edge check. Then consults admission control: **at capacity the run waits** — it does not proceed and does not fail, and a block parked on an operator hold releases its permit rather than starving the ceiling. |
| Execute | `execute.rs` | Builds and runs a **fresh in-process Rust `SDLC_FLOW` `Workflow`** with the repo resolved through `RepoRegistry` and `event.repo` seeded. Selects the engine from the block's own authored `sdlc_workflow` field — `Flow` only; `Task` errors (see above). |
| Integrate | `integrate.rs` | Verifies the state write after the engine returns and **fails the run loudly on a mismatch** — including a `status: "done"` run whose `final_validation.all_passed` is `false`, and a state file whose `block_id` does not match the executed block. Appends exactly one `lane-log.jsonl` line per step in the on-disk contract shape `{ts, lane, repo, block, status, note}` with `status` a typed `closed` \| `bailed` \| `held`; a **failed** step appends a `bailed` line before the error propagates, so an attempted block is never silent. An operator hold pauses and resumes without re-running completed blocks, under a deadline rather than an unbounded poll. |

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
