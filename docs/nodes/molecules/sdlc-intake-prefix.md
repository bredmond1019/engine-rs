---
type: Reference
title: "Molecule: SDLC Intake Prefix"
description: The setup-through-triage Node sequence shared literally between SDLC_FLOW and SDLC_TASK — the repo's one existing example of the target reuse pattern working as designed.
doc_id: nodes-molecules-sdlc-intake-prefix
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc, molecule, reuse, setupworktree, taskqueuerouter, proven]
related: [nodes-molecules-index, sdlc-flow-workflow, sdlc-task-workflow, nodes-atoms-workflow-embedded]
---

# Molecule: SDLC Intake Prefix

- **Proven, not a candidate** — this sequence is literally the same Rust Node types, imported
  directly by both workflows
- Sequence: `SetupWorktreeNode → SpecExistsRouterNode → [GenerateTasksNode →] LoadTaskStateNode →
  TaskQueueRouterNode → ImplementTaskNode → TestTaskNode → TriageTaskNode`
- Used by `SDLC_FLOW` and `SDLC_TASK` — [`sdlc-flow.md`](../../workflows/sdlc-flow.md),
  [`sdlc-task.md`](../../workflows/sdlc-task.md)
- Reference this molecule as the shape to imitate before hand-rolling a new intake sequence — see
  [`queue-drain-loop.md`](queue-drain-loop.md) for what happened when a workflow copied only the
  *idiom* instead of importing the type

## Why it's proven, not near

`crates/engine-core/src/workflows/sdlc_task/graph.rs`'s own module doc states: *"Every reused
sdlc_flow node this graph registers is unmodified."* Every Node in the sequence was
**pre-parameterized with builders specifically to enable this** — `with_branch_prefix`,
`with_state_filename`, `with_registry`, `with_policy_resolver`, `with_state_source`, `with_agent` —
so `SDLC_TASK` could import the exact same types rather than fork them.

## Node-by-node

| Node | File | Builder(s) that enable reuse |
|---|---|---|
| `SetupWorktreeNode` | [`sdlc_flow/setup.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/setup.rs) | `with_branch_prefix` |
| `SpecExistsRouterNode` | same | — |
| `GenerateTasksNode` | same | `with_registry` — implements `TransportSlotted`+`Cancellable` correctly, tier via Policy |
| `LoadTaskStateNode` | same | `with_state_filename` |
| `TaskQueueRouterNode` | [`sdlc_flow/task_loop.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/task_loop.rs) | — |
| `ImplementTaskNode` | same | LLM node, correct transport traits, tier via Policy |
| `TestTaskNode` | same | — |
| `TriageTaskNode` | same | LLM node, correct transport traits, tier via Policy |
| `IncrementAttemptNode` | same | — (also reused) |
| `SaveStateNode` | same | — (also reused) |
| `UpdateTaskStatusNode` | same | own doc/error text literally says "every route in both SDLC_FLOW and SDLC_TASK" |
| `CloseBlockNode` | [`sdlc_flow/close_block.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/close_block.rs) | `with_state_source`, `with_agent` |
| `FinalValidationNode` | [`sdlc_flow/final_validation.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/final_validation.rs) | `ValidationScope` enum added specifically for D56's "Reconcile" gate reuse |

## Where the two workflows diverge (correctly)

`SDLC_FLOW` continues past this prefix into `ConsolidatedReviewNode`/`TriageRouterNode`/
`ReviewRouterNode`/`EndReviewNode`/`PatchDocsNode`/`PullRequestNode` — a per-task-diff review,
docs pass, and PR ceremony `SDLC_TASK` deliberately excludes (it's the lean engine; see
[`sdlc-task.md`](../../workflows/sdlc-task.md) for why). `SDLC_TASK` has its own separate
`TaskTriageRouterNode` and `LeanBookkeepNode` rather than reusing `sdlc_flow`'s equivalents — this
is a documented, deliberate scope boundary, not an oversight.

## See also

- [`queue-drain-loop.md`](queue-drain-loop.md) — the queue-drain sub-shape this sequence contains, which `claim_reaffirm` hand-copied instead of importing
- [`../atoms/workflow-embedded.md`](../atoms/workflow-embedded.md) § 1 — the same list, per-node view
