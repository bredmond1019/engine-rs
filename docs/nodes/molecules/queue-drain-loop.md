---
type: Reference
title: "Molecule: Queue-Drain Loop"
description: The router/save/back-edge queue-drain shape, shared code in SDLC_FLOW/SDLC_TASK but hand-copied as an idiom into CLAIM_REAFFIRM — the concrete generic-combinator refactor.
doc_id: nodes-molecules-queue-drain-loop
layer: [engine]
project: engine-rs
status: active
keywords: [queue drain, router, loop, claim reaffirm, sdlc, near-molecule]
related: [nodes-molecules-index, nodes-molecules-sdlc-intake-prefix, claim-reaffirm-workflow, sdlc-flow-workflow]
---

# Molecule: Queue-Drain Loop

- **Near-molecule — real duplication, refactor named below**
- Shape: `{QueueRouterNode} → body → {non-router SaveNode} → (back-edge) QueueRouterNode`, exit
  branch when the queue drains
- Shared **code** in `sdlc_flow`/`sdlc_task` (see [`sdlc-intake-prefix.md`](sdlc-intake-prefix.md)),
  but **independently reimplemented** in `claim_reaffirm`

## The two instances

| Instance | Router | Save node | Status |
|---|---|---|---|
| `sdlc_flow` / `sdlc_task` | `TaskQueueRouterNode` | `SaveStateNode` | Genuinely shared code — same Rust types, see [sdlc-intake-prefix.md](sdlc-intake-prefix.md) |
| `claim_reaffirm` | `ClaimQueueRouterNode` ([`claim_reaffirm/queue_router.rs`](../../../crates/engine-core/src/workflows/claim_reaffirm/queue_router.rs)) | `SaveVerdictNode` ([`claim_reaffirm/save_verdict.rs`](../../../crates/engine-core/src/workflows/claim_reaffirm/save_verdict.rs)) | Hand-copied idiom — its own module doc **admits the copy**: "exactly the shape `sdlc_flow::task_loop`'s `SaveStateNode → TaskQueueRouterNode` back-edge takes (this workflow's copied idiom...)" |

`ClaimRecallNode` ([`claim_reaffirm/judge.rs`](../../../crates/engine-core/src/workflows/claim_reaffirm/judge.rs))
is a thin wrapper over the generic `nodes::RecallNode` living inside this same loop body — see
[`../atoms/workflow-embedded.md`](../atoms/workflow-embedded.md).

## The refactor

Extract a generic `QueueDrainRouter<Item, Status>` combinator (parallel to
[`loop_combinator::build_loop`](../../../crates/engine-core/src/loop_combinator.rs), which already
generalizes the *critic/revise* loop shape — see [`critic-revise-loop.md`](critic-revise-loop.md)).
Once it exists, `claim_reaffirm::ClaimQueueRouterNode`/`SaveVerdictNode` should migrate onto it, and
`sdlc_flow`/`sdlc_task`'s `TaskQueueRouterNode`/`SaveStateNode` become its first real consumer
rather than the thing being informally copied by hand.

## Why this matters

This is the clearest evidence in the whole audit that **an idiom copied by eye instead of imported
as code drifts** — `claim_reaffirm`'s copy exists because nobody extracted the shared shape into
something importable, so the next workflow needing a drain loop will likely copy it a third time
instead of finding a combinator.

## See also

- [`sdlc-intake-prefix.md`](sdlc-intake-prefix.md) — the proven half of this shape
- [`critic-revise-loop.md`](critic-revise-loop.md) — the sibling loop shape that already has a generic combinator, just not fully adopted
- [`../../workflows/claim-reaffirm.md`](../../workflows/claim-reaffirm.md)
