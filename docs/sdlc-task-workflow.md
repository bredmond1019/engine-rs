---
type: Reference
title: SDLC Task Workflow
description: "How the SDLC_TASK workflow graph works: the lean-close-out graph shape, the event schema, the D56 terminal reconcile, the three terminal statuses, and what this block does not yet ship"
doc_id: sdlc-task-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc-task, workflow, graph, reconcile, bookkeep, terminal-status, lean]
related: [sdlc-flow-workflow, sdlc-flow-policy, architecture]
---

# SDLC Task Workflow

`SDLC_TASK` is the lean sibling of [`SDLC_FLOW`](sdlc-flow-workflow.md): implement -> test ->
triage -> (loop) -> a single run-level reconcile -> a lean close-out, with no per-task review,
no docs patch, and no PR ceremony. It is the graph for one small unit of behaviour-changing work
(a `/ticket` or `/chore`) rather than a whole spec's review-and-ship cycle — the
`sdlc-task-ships-no-docs-stage` carryover records what choosing this workflow over `SDLC_FLOW`
costs, and this doc exists so that trade is written down rather than rediscovered.

Source: `crates/engine-core/src/workflows/sdlc_task/` (`mod.rs`, `schema.rs`, `graph.rs`,
`task_triage_router.rs`, `lean_bookkeep.rs`), reusing `crates/engine-core/src/workflows/sdlc_flow/`
(`setup.rs`, `task_loop.rs`, `final_validation.rs`, `close_block.rs`, `schema.rs`) node-for-node
wherever a node needs no SDLC_TASK-specific behaviour.

## Graph shape

```
SetupWorktreeNode -> SpecExistsRouterNode -> { GenerateTasksNode -> LoadTaskStateNode
                                             | LoadTaskStateNode }
  -> TaskQueueRouterNode -> { ImplementTaskNode -> TestTaskNode -> TriageTaskNode
                                -> TaskTriageRouterNode
                                -> { UpdateTaskStatusNode -> SaveStateNode
                                       -> (loop) TaskQueueRouterNode
                                   | IncrementAttemptNode -> ImplementTaskNode
                                   | LeanBookkeepNode }
                            | FinalValidationNode }

FinalValidationNode -> LeanBookkeepNode -> CloseBlockNode -> EmitStateNode   [terminal]
```

This matches `sdlc_task::graph::schema()` node for node; if the two ever disagree, the Rust is
correct and this diagram is stale.

Compared to `SDLC_FLOW`'s graph, six nodes are deliberately absent from the registry —
`ConsolidatedReviewNode`, `ReviewRouterNode`, `EndReviewNode`, `EndReviewRouterNode`,
`PatchDocsNode`, `PullRequestNode` — plus `WrapUpNode`, replaced by `LeanBookkeepNode`. A policy
value that named any of those seven identities would route into an unregistered node and strand
the walk with no terminal state, which is why `TaskTriageRouterNode` has exactly three arms and
never reads `SdlcPolicy::review_mode` at all.

`TaskQueueRouterNode`'s drain (no-pending-tasks) branch hardcodes the identity
`"FinalValidationNode"`. `SDLC_TASK` registers that same identity, but constructed with
`ValidationScope::Reconcile` instead of `SDLC_FLOW`'s default `Full` — the reconcile IS this run's
run-level authoritative gate, playing the role `Full` plays for `SDLC_FLOW`.

## Event schema

`SdlcTaskEventSchema` — `spec_slug` is the only required field; every other field is
`#[serde(default)]`, so `{"spec_slug": "X"}` alone deserializes.

| Field | Type | Default | Notes |
|---|---|---|---|
| `spec_slug` | `String` | — (required) | The only required field |
| `repo` | `Option<String>` | `None` | A `RepoRegistry` slug — never a path |
| `task_range` | `Option<String>` | `None` | Absent = a full run over every task in the spec |
| `resume` | `bool` | `false` | Re-run only the parts a fresh run would repeat |
| `use_worktree` | `bool` | `false` | Run **in place** by default — this is the JS engine's own default, stated explicitly rather than inherited from `SDLC_FLOW` |
| `branch_name` | `Option<String>` | `None` | Overrides the branch-prefix default (`"task/"`) when set |
| `llm_triage` | `bool` | `false` | |
| `policy` | `Option<serde_json::Value>` | `None` | Opaque passthrough — `SdlcTaskPolicy` does not exist until `EN.11.O`; see [sdlc-flow-policy.md](sdlc-flow-policy.md) for the policy-surface shape `SDLC_FLOW` already has |
| `profile` | `Option<String>` | `None` | |

`auto_pr` is the one `SDLCFlowEventSchema` field this schema drops — `SDLC_TASK` ships no PR
ceremony at all, so a PR-gating flag has nothing to gate.

`SDLCState`, `SDLCTask`, `SDLCTaskStatus`, `RunMeta`, `SDLCTelemetry`,
`to_committed_state_json`/`parse_task_range`/`derive_current_task`/`derive_bail_reason` are reused
from `sdlc_flow::schema` as-is, re-exported from `sdlc_task::schema` so callers never reach into
`sdlc_flow::schema` directly.

## The D56 terminal reconcile

Every per-task pass already runs each harness check's authoritative `command` at full depth
(`TestDepth::Full`), so re-running everything again at the end would be a pure double-run. The
reconcile instead runs `select_reconcile_checks` — which keeps a check iff it `gates` **and**
either its `fastCommand` differs from its `command`, or it is explicitly `perTask: false` — and
then runs only that narrowed set, always at authoritative depth (never `fastCommand`).

Two skip conditions make the reconcile a zero-`CommandRunner`-call pass-through that still stamps
a result:

1. **`test_depth == Full`** — every check already ran authoritative on every per-task pass, so
   there is nothing left to reconcile.
2. **`select_reconcile_checks` returns empty** — no check in the harness needed reconciling.

`FinalValidationNode::process` never returns `Err`, on this path or any other: an `Err` halts the
walk with no terminal state. A failing reconcile stamps `all_passed: false` and returns `Ok`;
`LeanBookkeepNode` is what reads that stamp and decides the run's terminal status.

The `fullRun` guard — a partial task-range run never reconciles and never closes the block — is
**not** implemented in `FinalValidationNode`. It belongs to `LeanBookkeepNode`, which derives
`full_run` from the inbound event's `task_range` field before deciding whether to consult the
reconcile at all.

## Terminal statuses

| Status | When | What it leaves behind |
|---|---|---|
| `"done"` | A clean full run, or a clean partial-range run | State committed; block closed via `CloseBlockNode` (full run only) |
| `"blocked"` | A `MAJOR_BAIL` triage verdict, a budget-exhausted `RETRYABLE`, or any unrecognized triage verdict | State committed; per-task commits stand |
| `"reconcile_failed"` | The D56 reconcile ran (full run only) and stamped `all_passed: false` | State still committed; per-task commits stand; the bookkeep **flip is skipped** — `CloseBlockNode` widens its skip predicate to also skip on `"reconcile_failed"`, naming D56 in the skip reason, so a failed reconcile can never close the block |

**`reconcile_failed` is terminal** (base-template D56 CALL 2, decided 2026-08-19) — the chain
stops there. In an `ORCHESTRATION` chain this means nothing downstream of this `SDLC_TASK` block
runs.

`--resume` on a task set where every task already passed re-runs **only** the reconcile — the
task loop is skipped entirely, so zero `ImplementTaskNode` invocations occur.

## What this block does not yet ship

- **Not dispatchable.** The graph runs in-process (`sdlc_task::graph::workflow()`), but
  `engine-serve` registration and `ORCHESTRATION` dispatch wiring are `EN.11.P` — until that
  lands, nothing outside this crate can trigger a run over HTTP or a lane chain.
- **No dedicated policy surface.** `registry_for_policy` takes `sdlc_flow::policy::SdlcPolicy`,
  not a `SdlcTaskPolicy` — that type, its named profiles, and any `SDLC_TASK`-specific knobs are
  `EN.11.O`'s.

A page claiming either capability before its block lands would be worse than no page at all —
this section exists so that claim never gets made by omission.
