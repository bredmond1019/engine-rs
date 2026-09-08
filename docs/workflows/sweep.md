---
type: Reference
title: The SWEEP workflow
description: SWEEP as an engine workflow — the ported roadmap-sweep pipeline, its single-node shape, how to trigger it, the profile-gated routing, and why the Python script stays the oracle.
doc_id: workflows-sweep
layer: [engine]
project: engine-rs
status: active
keywords: [sweep, roadmap sweep, escalation routing, permission profile, workflows]
related: [workflows-index, workflows-readme, orchestration-workflow]
---

# The `SWEEP` workflow (`EN.15.E`)

**Plain-English summary:** `SWEEP` is the fleet's roadmap-status sweep — the thing that notices a
lane went stale, a cross-repo edit landed, or an operator gate needs attention — reimplemented as
a dispatchable engine workflow. It reads the same live fleet state the standalone
`scripts/roadmap_sweep.py` script reads, diffs it against the last sweep, and routes what changed
(a notification, a lane wake-up, or nothing, depending on the active permission profile).

**The Python script is still the oracle.** This workflow reproduces `roadmap_sweep.py`'s diff and
routing decisions field-for-field and decision-for-decision, but it does not replace, edit, or
schedule that script. `planning/harness.json`'s `schedule.entries` stays empty and
`roadmap_sweep_cron.sh` stays uninstalled — registering `SWEEP` here makes it *dispatchable*, not
*scheduled*. Nothing currently calls it automatically.

## Where it lives

| Piece | Path |
|---|---|
| Pipeline stages (snapshot, diff, route) | `crates/engine-core/src/workflows/sweep/{snapshot,diff,route}.rs` |
| The node, schema, and registry | `crates/engine-core/src/workflows/sweep/mod.rs` |
| Registration into `bastion serve` | `crates/engine-serve/src/workflows.rs` (`register_sweep` / `register_sweep_with`) |
| Golden-replay test against 13 real sweep snapshots | `crates/engine-core/tests/it/sweep_replay.rs` |

## Triggering a run

Same convention as every other workflow (see the [quickstart](README.md#quickstart)) —
`workflow_type: "SWEEP"`, with these fields under `data`:

| Field | Required | What it is |
|---|---|---|
| `root` | yes | Filesystem path to the fleet root to sweep (a string) |
| `roadmap` | yes | The roadmap slug to sweep |
| `profile` | no | `"locked"` \| `"standard"` \| `"unrestricted"` — see [Permission profiles](orchestration.md#permission-profiles-en12c). Defaults to `PermissionProfile::Standard` if omitted |
| `now` | no | An RFC3339 timestamp to replay a specific instant deterministically; defaults to the real current time |

```bash
curl -X POST $ENGINE/events/ \
  -H "X-API-Key: $ENGINE_EVENTS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"workflow_type":"SWEEP","data":{"root":"/path/to/agentic-portfolio","roadmap":"my-roadmap"}}'
```

The graph is a single node, `SweepNode` — both start and terminal, mirroring `RECALL`'s and
`HARVEST_APPROVE`'s micro-workflow shape. The result lands on `ctx.nodes["SweepNode"]` and is also
written to `sweeps/<ts>.json` under the swept root — the same on-disk shape
`roadmap_sweep.py`'s own `load_snapshot` reads back, so a Rust-run sweep and a Python-run sweep
are interchangeable inputs to each other's next diff.

## What one run does

1. **Snapshot** — builds a fresh `RawSnapshot` of the fleet (lane registry, leases, message
   queue, operator gates, carryover, escalations) via the same discovery join `roadmap_status.rs`
   already provides, then projects it into the semantic shapes the diff stage compares.
2. **Diff** — compares that snapshot against the most recent one already on disk (a missing prior
   snapshot — a first sweep — is not an error) and builds a deduplicated escalation history keyed
   by `(ts_utc, repo, lane, kind, gate_id, summary)`.
3. **Route** — decides what to do with each new or re-firing escalation, and at most one bare
   non-escalation drift when nothing else routed:
   - a **cross-repo edit** always routes, regardless of profile;
   - a **notification** route asks over `OperatorTransport`, gated by `GatedAction::Notify`;
   - a **session** route wakes a lane via `LaneWake`, gated by `GatedAction::WakeLane`.

   A route a profile denies is never silently dropped — it is recorded in the written document as
   `suppressed_by_profile`, the same "recorded, never dropped" discipline the two placeholder seams
   below use.

## The two seams have no production implementation yet

`OperatorTransport` and `LaneWake` are injectable seams (see `crates/engine-core/src/operator/`
and `route.rs`'s `LaneWake` trait). `register_sweep`'s bare, default registration wires
`NoopOperatorTransport` and `NoopLaneWake` — placeholders that record every route as
`routed: false` with a reason naming the placeholder, never a false success. Use
`register_sweep_with(dispatcher, transport, waker)` to inject a real pair once one exists (a
`LaneWake` over `crate::coord::write::send`, and an `OperatorTransport` is the standing
`bastion notify` seam other workflows already use).

## See also

- [README.md](README.md) — the full workflow catalogue and trigger quickstart.
- [orchestration.md](orchestration.md#permission-profiles-en12c) — the `PermissionProfile` /
  `GatedAction` grading table `SWEEP`'s routing enforces against.
