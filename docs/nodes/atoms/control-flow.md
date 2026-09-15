---
type: Reference
title: Control-Flow Atom Nodes
description: Nodes that shape the graph itself rather than doing external work — fan-out, aggregate, suspend, generic routers, and the bounded-loop builder.
doc_id: nodes-atoms-control-flow
layer: [engine]
project: engine-rs
status: active
keywords: [fanout, aggregate, suspend, router, loop combinator, control flow]
related: [nodes-atoms-index, nodes-molecules-queue-drain-loop, nodes-molecules-critic-revise-loop]
---

# Control-Flow Atom Nodes

- Nodes that shape the graph itself — parallelism, joining, pausing, routing — rather than calling
  a model or an external service → table below
- The generic bounded-loop builder that should replace most hand-rolled critic/revise or
  queue-drain routers lives here too → §2
- **Most of these have zero production callers yet** — designed to be generic, not yet adopted

## 1 — Nodes

| Node | File | Verdict | Evidence |
|---|---|---|---|
| `FanOutNode` | [`nodes/fan_out.rs`](../../../crates/engine-core/src/nodes/fan_out.rs) | **atom** | `identity`/`base_name`/`count`/`builder: Box<dyn Fn(usize)->Box<dyn Node>>` all constructor params, no hardcoded type/workflow. Zero production callers today (only a test stub in `engine-serve/tests/schedule.rs`) — adoption lag, not a design defect |
| `AggregateNode` | [`nodes/aggregate.rs`](../../../crates/engine-core/src/nodes/aggregate.rs) | **atom** | `identity`/`source_identities`/`output_key`/`missing_source` all config. Same zero-production-caller picture as `FanOutNode` |
| `SuspendNode` | [`nodes/suspend.rs`](../../../crates/engine-core/src/nodes/suspend.rs) | **atom** | `identity`/`enabled`/`predicate`/`reason_label` builder-set; `enabled:false` is a verified in-place no-op. No production workflow adopts it yet per its own module doc. See [`../../suspend-resume.md`](../../suspend-resume.md) for the pause/resume machinery it feeds |
| `ProposalReviewRouterNode` | [`proposal_generator/review_router.rs`](../../../crates/engine-core/src/workflows/proposal_generator/review_router.rs) | **atom** | Zero-field router; target identities resolved via `InputBinding::bound(...)` rather than literals — cleanly generic already, worth using as the reference shape for a new router |

`impl Node for Box<dyn Node>` (`nodes/fan_out.rs`) is a forwarding shim so a boxed trait object
satisfies `NodeExt`'s `Sized` bound for `FanOutNode`'s builder output — not a real unit of work,
excluded from the table.

## 2 — The generic loop builder

| What | File | Notes |
|---|---|---|
| `build_loop` / `LoopSpec` / `LoopCluster` | [`loop_combinator.rs`](../../../crates/engine-core/src/loop_combinator.rs) | Reusable bounded-loop builder (`{guard router, increment node, back-edge}`). `proposal_generator` already uses it. `content_pipeline` and `linkedin_post` each hand-roll their own critic-router loop instead — see [the critic/revise-loop molecule](../molecules/critic-revise-loop.md) for the concrete migration |

## Near-atom routers found inside workflows

These router-shaped Nodes are generic *in structure* (fold string→route, fail-closed default) but
every literal inside them is workflow-specific — a real refactor is named, not a rewrite:

| Node | File | Refactor |
|---|---|---|
| `TaskTriageRouterNode` | [`sdlc_task/task_triage_router.rs`](../../../crates/engine-core/src/workflows/sdlc_task/task_triage_router.rs) | Extract a generic `VerdictRouter{source_node, field, arms, default}` into `nodes/`; the budget-exhausted special case stays local |

`sdlc_flow`'s own `TriageRouterNode`/`ReviewRouterNode`/`EndReviewRouterNode` and
`research_agent::ResearchModeRouterNode` are `specific` — each hardcodes target identities intrinsic
to its own graph's shape, and deliberately isn't shared with a sibling workflow that has its own
separate router for the same decision point (documented in each module's own doc comment).

## See also

- [`../molecules/queue-drain-loop.md`](../molecules/queue-drain-loop.md) — the queue-drain-and-stamp shape, hand-copied once
- [`../molecules/critic-revise-loop.md`](../molecules/critic-revise-loop.md) — the bounded critic/revise loop, hand-rolled three times
