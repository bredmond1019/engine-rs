---
type: Reference
title: Suspend / Resume (Operator Pause + Human-in-the-Loop Approval)
description: The metadata.suspension marker, the two suspension origins (operator pause, SuspendNode), the run_from rehydration path, the three HTTP routes, and the granularity/atomicity limits of pausing a graph walk.
doc_id: suspend-resume
layer: [engine]
project: engine-rs
status: active
keywords: [suspend, resume, pause, walk-pointer, rehydration, approval-gate]
related: [architecture, data-contract, D6-cancellation-and-budget-semantics]
---

# Suspend / Resume

`EN.6.F` makes a run stoppable at a node boundary and continuable later, from either an
**operator** signal (`POST /events/{run_id}/pause`) or a **workflow-authored** `SuspendNode`. Both
origins converge on one durable marker (`metadata.suspension`) and one entry point
(`Workflow::run_from`) that starts the walk somewhere other than `schema.start_node`.

## The `metadata.suspension` marker

Written by `engine_core::suspend::stamp_suspended` into `TaskContext.metadata["suspension"]`,
mirroring how `stamp_cancelled` writes `metadata["cancellation"]` (D6: new run outcomes are spelled
as `metadata` keys, never as a new `NodeRunStatus` variant).

```json
{
  "suspension": {
    "suspended": true,
    "at": "2026-07-30T12:00:00Z",
    "resume_at": "ReviewNode",
    "reason": "operator_pause",
    "origin_identity": "SomeNode",
    "ledger": { "total_tokens": 1234, "total_cost_usd": 0.56 },
    "resume_count": 0,
    "requested": false
  }
}
```

| Field | Type | Meaning |
|---|---|---|
| `suspended` | `bool` | `true` while the run is stopped; flipped back to `false` by `stamp_resumed` on resume. The key itself is **never deleted** — a stable shape across suspended/resumed/never-suspended states is what makes a resumed run's final `EventsRow` round-trip identical in shape to an uninterrupted run's. |
| `at` | ISO-8601 string | When this suspension was stamped. |
| `resume_at` | `string \| null` | The node identity `Workflow::run_from` starts the walk at on resume — the durable walk pointer. |
| `reason` | `"operator_pause" \| "suspend_node" \| null` | Which of the two origins produced this marker (`SuspendReason`). |
| `origin_identity` | `string \| null` | The identity of the node whose successor became `resume_at` (the node that just finished when the walk stopped). |
| `ledger` | `{total_tokens, total_cost_usd} \| null` | A `LedgerSnapshot` of the running `BudgetLedger` totals at the moment of suspension — what lets a resume continue spending from the pre-suspend totals instead of a fresh allowance. |
| `resume_count` | `u32` | Carried forward across suspend/resume cycles; only incremented by `stamp_resumed`. |
| `requested` | `bool` | Set by `SuspendNode`/`request_suspension` to ask the walk to stop after the current node; reset to `false` by both a fresh `stamp_suspended` and by `stamp_resumed`. |

## The two origins, one marker

1. **Operator pause** — `POST /events/{run_id}/pause` sets the run's `PauseSignal` (a clearable,
   `tokio::sync::watch`-backed two-way flag; see below for why it is deliberately not
   `CancellationToken`). `Workflow::walk` checks the signal at each node boundary and, if paused,
   finalizes the suspend itself: it picks `resume_at` as the *next* node in the walk, snapshots the
   `BudgetLedger`, and calls `stamp_suspended` with `reason: OperatorPause`.
2. **`SuspendNode`** (`engine_core::nodes::SuspendNode`) — a `Node` cannot stop the walk itself
   (`Node::process` only transforms a `TaskContext`), so its `process` only *requests* suspension
   via `crate::suspend::request_suspension`, flipping `metadata.suspension.requested = true`.
   `Workflow::walk` sees the request after the node returns and finalizes it exactly like the
   operator-pause path, with `reason: SuspendNode`. `SuspendNode` defaults to `enabled: false` (an
   in-place no-op, patterned on `MaterializeDocNode::with_enabled` — the node stays in the declared
   graph at every setting, so the node set never varies by policy) and always stamps its resolved
   `enabled` value into `ctx.nodes[identity]` for telemetry attribution.

Both paths land on the same `stamp_suspended` call, so a resume never needs to know which origin
produced the marker it is rehydrating from — `resume_at`, `reason`, and the ledger snapshot are all
it reads.

## `PauseSignal` vs. `CancellationToken`

`PauseSignal` is a **separate type**, not a reuse of `CancellationToken`. `CancellationToken::cancel`
is a documented one-way idempotent latch (D6) — abort semantics must not become resettable. A pause,
by contrast, must be clearable: a resumed run gets a fresh `PauseSignal`, and a signal that outlived
its run must not leak a stuck "paused" state onto a new one. Both are backed by the same
`tokio::sync::watch` shape (cheaply cloneable, `Send + Sync`, observed from any clone).

## Two known limits

**The loop-top granularity limit.** Pause never interrupts an in-flight node — the walk only checks
`PauseSignal`/`suspension.requested` at node *boundaries*. Pause latency therefore equals the
remaining duration of whatever node is running when the signal is set. For a long-running node (an
SDLC `ClaudeCodeStep`, for example) that can be minutes, not milliseconds. This is why the surface
needs a `pausing` status distinct from `suspended`: `pausing` means the signal is set but the walk
hasn't reached a boundary yet; `suspended` means it has and the marker is stamped.

**The `ParallelNode` atomicity limit.** A `ParallelNode` fan-out is one indivisible step in the
walk — its branch nodes get no framework `NodeRun` envelope of their own, so there is no boundary
*inside* a fan-out for the walk to stop at. A resume pointer can therefore never land inside a
`ParallelNode`'s branches; the earliest a pause can take effect around one is before it starts or
after the whole fan-out/merge completes.

## Resume does not re-resolve policy

A resume rebuilds the `Workflow` from the run's *original* trigger payload via the registered
`WorkflowFactory` (`dispatch_with_event`), then calls `.without_seeded_nodes()` on the result. The
rehydrated `TaskContext` already carries the original run's resolved policy in `ctx.nodes`/
`ctx.metadata` — re-seeding from a freshly-resolved policy would silently overwrite it. A workflow
rebuilt under a *different* seeded policy at resume time (e.g. a changed environment variable) still
runs under the policy stamped in the rehydrated context, not the freshly-resolved one.

**`SDLC_FLOW` caveat.** Its factory resolves policy from `std::env::current_dir()`, so a resume
attempted from a different working directory than the one the run started in can `422` at the
"policy resolution failed" step. This is unavoidable without caching a non-serializable factory
output — resume from the same working directory the run was triggered from.

## Suspended runs never expire

There is no auto-expiry for a suspended run — that is explicitly out of scope for this block. The
only backstop is the bounded FIFO suspended-run index (`engine-serve`'s `suspend.rs`, sized to
`live_state::COMPLETED_RUN_RETENTION`): when the index is full, inserting a new suspended run evicts
the oldest one, and the eviction path stamps `metadata.cancellation` into its retained snapshot and
marks it terminal via `live.mark_terminal` — the same shape a real abort produces — so it stops
looking live or suspended forever rather than leaking silently. Do not assume a suspended run is
durable indefinitely; a long-lived server process with many concurrent suspensions can still evict
one.

## Resume is at-least-once per node, but only across a crash

Pause itself never re-runs a completed node: `Workflow::walk` only stops at a boundary, and every
node up to `resume_at` already has a `Success` `NodeRun` with its `ctx.nodes` output intact before
the marker is stamped. Resume continues strictly forward from `resume_at`. The "at-least-once"
framing applies to a different failure mode — a node interrupted by a process *crash* (not a pause)
would, on a hypothetical future crash-recovery path, need to re-run since it never reached a
recorded boundary. That is not what this block implements; it is called out here only so the two
failure modes (pause vs. crash) are not conflated.

## The three routes

| Method | Path | Behavior |
|---|---|---|
| `POST` | `/events/{run_id}/pause` | `401` without a valid `X-API-Key`; `404` for a `run_id` that is neither live nor suspended; `409` if already suspended; otherwise sets the run's `PauseSignal` and returns `202 {run_id, status: "pausing"}`. Idempotent against a repeat call while still pausing. |
| `POST` | `/events/{event_id}/resume` | `401` without a valid key; `404` for an unknown or non-suspended run; `409` for a concurrent resume already in flight; `422` for a policy-resolution failure or an unresolvable `resume_at` (the resume point no longer exists in the rebuilt graph); otherwise `202 {run_id, event_id, status: "resuming", resume_at}`. No request body — an operator `{"at": ...}` override to pick a different resume point is a deliberate non-goal. |
| `GET` | `/events/suspended` | `401` without a valid key; `200 [{run_id, workflow_type, created_at, suspended_at, resume_at, reason}]`, newest first. Registered ahead of `{event_id}` in `configure` — actix-web resolves routes first-registration-wins, so the literal path must not be shadowed by the uuid extractor. |

Resume's double-resume guard (`take_for_resume`/`clear_resuming` in `engine-serve`'s `suspend.rs`)
reads and sets the entry's `resuming` flag under one write-lock acquisition — a check-then-act split
would itself be the double-resume the routes forbid. Every failure path from ledger rebuild onward
calls `clear_resuming` so a transient failure (a bad policy resolution, a stale `resume_at`) leaves
the run retryable rather than permanently bricked.

Resume works with **no `DATABASE_URL`**: the in-memory `SuspendedEntry` (trigger payload + last
`TaskContext` snapshot) is what makes the readback DB-free, matching the rest of the run-readback
path (`GET /events/{event_id}`). Postgres is a fallback only — `resume_run` rehydrates from the
durable `events` row when a run is not found in the in-memory index (e.g. after a server restart),
via `engine_store::get_event`, only reachable when a pool is configured.

## Consuming UI

The Pause/Stop/Resume UI is `bastion-web`'s `BW.8.O`, not this block. This spec ships the three
routes and the status vocabulary (`pausing` → `suspended` → `running`), plus the pre-existing
`POST /events/{run_id}/abort` for Stop. Note also: `core/bastion`'s `derive_run_status` degrades a
suspended run to `Pending` today — that is deferred, not a bug; `bastion` has not yet been taught
the `suspended` status this block introduces.

## See also

- [architecture.md](architecture.md) — module map entries for `engine-core/src/suspend.rs`,
  `engine-core/src/nodes/suspend.rs`, `engine-serve/src/suspend.rs`, `engine-serve/src/resume.rs`.
- [data-contract.md](data-contract.md) — the three routes' HTTP-surface-parity entries and the
  `metadata.suspension` changelog row.
