---
type: Reference
title: Crash Recovery (Orphan Sweep + Stale-Run Alarm)
description: The metadata.completion marker, the boot sweep that fails crash-stranded runs loudly, the stale-run alarm on aged running/suspended runs, the OrphanLister seam, the OrphanPolicy knobs, and where boot wiring lives (bastion's `serve/mod.rs`, now wired).
doc_id: orphan-recovery
layer: [engine]
project: engine-rs
status: active
keywords: [orphan, crash recovery, completion marker, boot sweep, stale-run alarm, OrphanLister, OrphanPolicy, launchd, KeepAlive]
related: [architecture, data-contract, suspend-resume, operator-payload-contract, D6-cancellation-and-budget-semantics]
---

# Crash Recovery (Orphan Sweep + Stale-Run Alarm)

`EN.9.C` gives engine-rs a way to see runs a crash stranded. The Mac Mini's launchd plists for
`bastion serve` both set `KeepAlive=true` / `ThrottleInterval=10`, so a crashed process is restarted
within ~10s and comes back up healthy — hiding the evidence that a run died mid-walk. This block
closes that gap with three pieces: a persisted completion marker, a boot sweep that fails orphaned
runs loudly, and an age alarm on runs stuck `running`/`suspended`.

## The `metadata.completion` marker

There is no `status` column on the `events` table (contract §4: `id, workflow_type, data,
task_context, created_at, updated_at`), and deriving status from `task_context` alone cannot
distinguish a clean finish from a crash: a run that died after node 1 of 5 has no failure marker
either, so `crate::http::derive_terminal_status` reads it as `"succeeded"`.

`crate::completion::stamp_completion` (`crates/engine-core/src/completion.rs`) closes that gap by
stamping every terminal exit with a marker whose *absence*, not its content, is what the sweep
below keys on:

```jsonc
{ "metadata": { "completion": { "terminal": true, "status": "succeeded", "at": "<rfc3339>" } } }
```

It is written into `crates/engine-serve/src/suspend.rs` at both terminal exits, into the same
`final_ctx` snapshot the durable writer persists (not just the in-memory one — a marker that never
reaches Postgres leaves the orphan query blind), with the status `derive_terminal_status` reports
for that same snapshot: `succeeded|failed|cancelled|budget_halted`. The suspend path never stamps
it — a suspended run is not terminal (`derive_live_status`), and marking it complete would hide it
from this very sweep.

This mirrors the `cancellation`/`budget`/`suspension` precedent exactly (see
[data-contract.md](data-contract.md) § Run-level `metadata` annotations): an engine-rs-only
extension living entirely in the existing free-form `TaskContext::metadata` field, no new
`NodeRunStatus` variant (D6), no canonical contract re-pin.

## The orphan query

`engine-store::postgres::list_orphan_candidates` (re-exported from `lib.rs`) finds crash-stranded
rows by the marker's absence:

```sql
SELECT id, workflow_type, data, task_context, created_at, updated_at
FROM events
WHERE task_context->'metadata'->'completion' IS NULL AND updated_at < $1
ORDER BY updated_at ASC
LIMIT $2
```

`older_than` and `limit` are both caller-supplied — `limit` is a hard bound so a first sweep over a
long-lived database cannot load an unbounded result set into memory. Covered by a case in the
existing `#[ignore]`d live-Postgres binary, `crates/engine-store/tests/postgres_round_trip.rs`.

## The `OrphanLister` seam

`crates/engine-serve/src/orphan.rs` defines `OrphanLister`, a trait over
`list_orphan_candidates`/`persist_reconciled`, so the reconcile logic below is testable with no
database:

- `PgOrphanLister` (built by `orphan_lister_live(pool)`) — the live, `engine-store`-backed
  implementation production wires.
- `RecordingOrphanLister` — an in-memory test double that records what it was asked to persist.

This is the same trait/live-impl/stub shape as the `HttpPost` and `DocMaterializer` seams
documented in [architecture.md](architecture.md) § Injectable Seams, though it lives in
`engine-serve` rather than `crates/engine-core/src/nodes/*` — it lists candidate rows for the boot
sweep rather than dispatching an outbound action.

## The boot sweep

`crate::orphan::reconcile_orphans(lister, policy, now)` — for every candidate the resolved policy
allows:

1. Names the crash: finds the `node_runs` entry (if any) whose status is `Running` at boot and
   builds a reason string naming it, or a generic "no in-flight node recorded" reason if none is
   found.
2. Stamps `metadata.failure` (`crate::http::stamp_failure`) with that reason — the same shape
   `derive_terminal_status` already checks defensively.
3. Stamps `metadata.completion` with `status: "failed"` (`engine_core::stamp_completion`).
4. Persists the row via `lister.persist_reconciled`.

It **never attempts a resume** — a mid-walk crash is unresumable by design (`crate::resume` returns
`None` for any run without a suspension marker, and only `finish_suspended` writes one; an orphan by
definition has none). It is never silent: one line is printed per reconciled run plus a summary
line, because a sweep that reconciles nothing observably would reproduce the exact "comes back up
healthy" failure mode this block exists to end. It returns a `ReconcileSummary` (`scanned`,
`reconciled: Vec<Uuid>`) so a caller can log or assert on it.

No-ops (returns an empty, `scanned: 0` summary) when `policy.reconcile_on_boot` is `false`.
Idempotent: a candidate this call reconciles now carries a `completion` marker, so a second sweep's
`list_orphan_candidates` call — live or stubbed — no longer returns it.

**Hand-verified recipe.** Killing the `:8090` instance mid-run and letting launchd restart it leaves
the run in a terminal `failed` state within one boot cycle — `reconcile_orphans` **is** wired at
boot (see "Boot wiring" below), so this path is live. The run record documents the recipe end to
end.

## The stale-run alarm

Not every stranded run crashes cleanly enough to have no completion marker at all — a run can also
simply get stuck `running` or `suspended` well past when it should have progressed, without the
process dying. `crate::orphan::stale_run_ids` is a pure decision function over the live map's
`(run_id, snapshot, updated_at)` records: given `now` and the resolved `stale_run_alarm_secs`, it
returns the run ids whose live status (`derive_live_status`) is `"running"` or `"suspended"` and
whose age past `updated_at` is at least the threshold. No I/O, no clock reads beyond the `now`
argument — hermetically testable against hand-built fixtures.

`crate::orphan::alarm_stale_runs(live, policy, now)` drives it against a real
`LiveStateStore`, enqueuing exactly one `OperatorQueueItem` per stale run into
`live.operator_queue()`, mirroring `live_state.rs`'s `maybe_enqueue_failure_notification` for
rendering discipline and using `orphan_item_priority` for the item's priority. De-duplication
reuses the same once-per-run shape `mark_terminal`'s `is_first_terminal_transition` established for
terminal notifications (`operator-payload-contract.md`): `LiveStateStore::mark_alarmed` returns
`false` on a run already alarmed, so a second pass over the same stuck run enqueues nothing further
— one stuck run produces exactly one item, not one per tick. A render/validate failure for one
candidate is skipped rather than propagated; the sweep still processes every other candidate and
never panics or blocks the caller's path.

## Policy: `OrphanPolicy`

`crates/engine-core/src/operator/orphan.rs` mirrors `operator/failure.rs`'s shape — `OrphanPolicy` +
`PartialOrphanPolicy`, `baseline()`/`cheap_fast()`/`thorough()`, `profile_by_name`,
`resolve_policy_for_run_from`, `policy_state` — and resolves through the standard four layers
(per-run event `policy` override > named `profile` bundle > `planning/harness.json` defaults >
built-in default):

| Knob | Built-in default | What it does |
|---|---|---|
| `reconcile_on_boot` | `true` | Gates the whole boot sweep. Defaults **enabled** rather than behavior-stable — a knob defaulting off would ship this block's entire purpose behind a flag nobody sets; this is a deliberate, documented exception to the behavior-stable-default half of CLAUDE.md standing rule 6. |
| `stale_run_alarm_secs` | `3600` | How long a run may sit `running`/`suspended` past its `updated_at` before the stale-run alarm enqueues an item for it. |
| `orphan_item_priority` | `0` | The `effective_priority` a reconciled-orphan or stale-run-alarm item enqueues under in the `EN.8.B` `OperatorQueue`, mirroring `run_failure_notification`'s `failure_item_priority` convention. |
| `orphan_scan_limit` | `200` | Bounds how many candidate rows one boot sweep loads from `list_orphan_candidates`. |

All four are set explicitly in `planning/harness.json`'s `baseline`, `cheap-fast`, and `thorough`
profiles. `reconcile_on_boot` stays `true` in every profile — disabling the sweep is a correctness
question, not a cost/quality dial. `cheap-fast` alarms sooner, at a lower priority, and scans fewer
rows per sweep; `thorough` gives a longer alarm grace period, a higher alarm priority, and a wider
scan limit.

## Boot wiring lives in bastion — and is wired

`engine-serve` is a library `bastion serve` mounts, so the call site is a `bastion`-side change.
That change has landed: `reconcile_orphans` runs **once at boot, before the HTTP listener binds**,
at `core/bastion/src/serve/mod.rs:556` (`ticket-orphan-reconcile-wiring` task 1), with the sweep
summary passed through `classify_orphan_sweep` / `log_orphan_sweep`. This spec still owns the
entry point and its hermetic coverage; bastion owns when it fires.

`spawn_schedule_loop` (`EN.ticket.cron-schedule-startup-wiring`) followed the same shape and is
also wired now — though unlike this sweep it does not yet *run*, because what remains for it is
configuration: `BASTION_ENGINE_HARNESS_PATH` is unset and `schedule.entries` is empty.

## Tests

- `crates/engine-core/src/completion.rs` — unit tests for `stamp_completion`/`is_complete`
  (empty metadata, non-object metadata, sibling-annotation preservation, serde round-trip).
- `crates/engine-store/tests/postgres_round_trip.rs` (`#[ignore]`d, live Postgres) — the
  `list_orphan_candidates` case.
- `crates/engine-core/src/operator/orphan.rs` — `OrphanPolicy` layer-resolution unit tests.
- `crates/engine-serve/src/orphan.rs` — hermetic unit tests over `RecordingOrphanLister` and
  hand-built live-record fixtures for both `reconcile_orphans` and `stale_run_ids`/
  `alarm_stale_runs`: reconciles N candidates, idempotent on a second sweep, no-ops when the policy
  disables it, surfaces a lister error rather than swallowing it, a fresh run does not alarm, a run
  past the threshold alarms exactly once, a second pass adds nothing, and a terminal run never
  alarms.

No new integration-test binary was added to any crate — all new tests are unit tests in-module or
cases appended to the existing `#[ignore]`d binary (CLAUDE.md standing rule 8).
