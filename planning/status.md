---
type: ProjectStatus
title: engine-rs Status
description: Current state and progress tracker for engine-rs.
doc_id: status
layer: [factory]
status: active
timestamp: "2026-07-03T18:45:05Z"
now: "Paused EN.2.A to decide transport architecture (CLI wrapper vs hybrid) based on SDK audit"
next: "EN.2.A — Claude Code step node; first command /generate-tasks EN.2.A"
blocked: []
keywords: [status, progress tracker, current focus, blocks]
related: [context, master-plan, planning-index, knowledge, memory]
---

# STATUS — Current State & Progress

**Last updated:** 2026-07-03 — Audited Claude SDKs, wrote notes to `planning/claude-sdk/notes.md`, logged D4 transport options to `state.json` carryover, wrote handoff.
**Current focus:** Paused EN.2.A to decide transport architecture (CLI wrapper vs hybrid) based on SDK audits.

---

## How to Read / Update This File

- Status values: `Not started` · `In progress` · `Done` · `Blocked` · `Skipped`
- Keep `Current focus` and `Last updated` accurate; update as work happens.
- This file is **state only**. For what the work means, see `master-plan.md`.
- The **now/next/blocked** frontmatter scalars mirror the `## Momentum` headlines below;
  `/log-work` keeps them in sync. See `agentic-portfolio/docs/planning-conventions.md` (D30).

---

## Momentum

> Working board — keep all five queues live. **Never end a meaningful session with every queue
> empty.** The headlines of **now / next / blocked** mirror the frontmatter scalars above.

- **now** — Phase 1 (Execution Core) fully Done — EN.1.A/EN.1.B/EN.1.C all closed; engine embedded in bastion serve
- **next** — EN.2.A — Claude Code step node; first command `/generate-tasks EN.2.A`
- **blocked** — _nothing yet — each entry names its blocker and the smallest missing answer_
- **improve** — _self-improvement backlog: eval gaps, flaky workflows, repeated failures, missing skills, stale assumptions_
- **recurring** — _schedules, monitors, sweeps, automations_

---

## Metrics

> Cheap, hand-maintained signals (leading + lagging). Do **not** push these into frontmatter —
> they are multi-valued and volatile.

- tasks completed / verified this period; intervention rate; retry rate; regression rate
- reusable assets created since last milestone
- days since last eval improvement; days since last new skill/workflow
- % of runs ending with an explicit next action

---

## Progress Table

### Phase 0 — Foundation
| Block | What | Status | Notes |
|---|---|---|---|
| EN.0.A | Cargo workspace + CI | Done | Workspace skeleton, fmt/clippy/test CI, async-runtime decision (D2) |
| EN.0.B | Data-contract serde types + Postgres round-trip | Done | Byte-for-byte seam types + engine-store Postgres round-trip (self-skips without DATABASE_URL); PASS review |

### Phase 1 — Execution Core
| Block | What | Status | Notes |
|---|---|---|---|
| EN.1.A | Node trait + Workflow runner | Done | `Node` trait + `NodeRegistry`, `WorkflowSchema`, pointer-walk `Workflow::run` with `on_progress` seam; PASS review |
| EN.1.B | Router + parallel nodes + validator | Done | `Router` trait + `dispatch_route`, `ParallelNode` fan-out/merge, `WorkflowValidator` (reachability/cycle/arity) + `Workflow::new_validated`; PASS review |
| EN.1.C | Trigger/dispatch + dual-registry + serve embedding | Done | Dual-registry `Dispatcher`, in-memory `LiveStateStore`, async durable-write seam (`durable.rs`) against `engine-store`, four-endpoint `actix-web` HTTP surface (D3), headline integration test; PASS review |

<!-- Add one sub-table per phase as the plan is fleshed out. -->

---

## Decisions & Deviations Log

*Record deviations from the plan and notable in-flight choices here. Promote durable ones to
`decisions/` via `/log-work`.*

---

## Quick Self-Check

- Is `Current focus` accurate?
- Any `In progress` rows that are actually `Done`?
- Anything `Blocked` that needs surfacing?

---

*State only. For what things mean, see master-plan.md. For orientation, see context.md.*
