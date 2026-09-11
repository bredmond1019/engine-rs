---
type: Reference
title: Coordination HTTP Surface
description: "The /api/coordination/* read and write routes a script or health check uses directly, and the HELD_SESSION micro-workflow — the parts of the coordination layer with no other doc."
doc_id: coordination
layer: [engine]
project: engine-rs
status: active
keywords: [coordination, lease, heartbeat, register, held-session, api]
related: [orchestration-workflow, commander-workflow, workflows-sweep, consolidate-workflow, orchestration-test-ladder]
---

# Coordination HTTP surface

**What this is:** the two read routes and eight write verbs a script (or a health check, or a
`bastion coord` call under the hood) uses to see and change fleet coordination state directly over
HTTP — distinct from the three coordination-native *workflows* ([`COMMANDER`](commander.md),
[`SWEEP`](sweep.md), [`CONSOLIDATE`](consolidate.md)), which each have their own doc already. This
page covers the plain HTTP surface and `HELD_SESSION`, the one registered workflow with no doc of
its own until now.

An `ORCHESTRATION` chain already takes a lease and heartbeats it for you — see
[orchestration.md § Fleet coordination](orchestration.md#fleet-coordination-a-rust-driven-chain-is-now-visible-to-bastion-coord-status)
for why. Read this page when you want to touch the same state **without** driving a chain: a
one-off script, a manual test, or diagnosing what a lane currently holds.

## Quickstart

```bash
# Read the fleet's whole joined coordination view (no auth needed)
curl -sf "$BASTION_SERVE_ADDR/api/coordination" | jq

# Claim an identity and take a lease, the same two calls an ORCHESTRATION chain makes for itself
curl -sf -X POST "$BASTION_SERVE_ADDR/api/coordination/register" \
  -H 'Content-Type: application/json' \
  -d '{"agent_name":"my-script","repo":"engine-rs","lane":"my-lane","roadmap":"my-roadmap"}'
curl -sf -X POST "$BASTION_SERVE_ADDR/api/coordination/lease" \
  -H 'Content-Type: application/json' \
  -d '{"repo":"engine-rs","lane":"my-lane","agent":"my-script","kind":"exclusive"}'
```

## The read routes

| Route | What it returns |
|---|---|
| `GET /api/coordination` | The fleet's whole joined view — registry claims, leases, fleet-concurrency slots, inter-lane messages, heartbeats, escalations, run records — read fresh on every request via `engine_core::coord::read_coordination_view`. Always `200`; a **degraded** view (an inconsistency it found) is a normal answer, not a fault — only a failure to resolve the brain root at all is a `5xx`. |
| `GET /api/roadmaps/{slug}/status` | The same join `/roadmap-status --roadmap <slug>` produces: `lane-log.jsonl`, per-repo orchestration-run records, per-spec SDLC state, each repo's `state.json` operator edges/carryover, plus a corpus-wide `validate-brain --state`. Always `200` for a resolved roadmap — a malformed `lane-log.jsonl` line is reported in `malformed_lines`, not a fault; `404` for an unknown or ambiguous slug. |

## The eight write verbs

`POST /api/coordination/{register,heartbeat,release,lease,unlease,send,drain,complete}` — all onto
one seam, `engine_core::coord::write` (schema-validate the body, stamp `host` from
`ENGINE_COORD_HOST` when set, snapshot the previous file to `.prev/`, then write).

| Verb | Purpose | Response shape |
|---|---|---|
| `register` | Claim a lane-agent identity (`agent_name`, `repo`, `lane`, `roadmap`) in the registry | `200` with the claim; `409` if the registry is at capacity for that repo/category |
| `heartbeat` | Re-stamp a claim's or lease's liveness timestamp | `200`; `404` if `agent_name` names no existing claim |
| `lease` | Take a repo lease (`repo`, `lane`, `agent`, `kind`: `exclusive` for a lane that will commit) | `200` with the lease; `400` on a validation refusal (e.g. out-of-window) |
| `unlease` | Release a repo lease | `200` with `{"removed": bool}` — idempotent, never errors on an already-absent lease |
| `release` | Release a registry claim | `200` with `{"removed": bool}` — idempotent |
| `send` | Drop one message into another lane's inbox queue | `200`; `400` if the envelope carries a disallowed field (e.g. a `priority` key) |
| `drain` | Read and remove every message in the caller's own inbox | `200`, always — an empty drain is a normal answer |
| `complete` | Mark one drained message as processed (writes a receipt) | `200`, always — completing something already gone is a no-op, not an error |

**No `X-API-Key` gate on any of the ten routes above.** They sit behind the reverse-proxy's own key
check when deployed inside `bastion serve`; a second in-process gate here would only let the two
auth layers drift. These are the same verbs `bastion coord register`/`lease`/`release`/`heartbeat`
(the CLI) call — see the `begin-orchestration` skill for the CLI form a lane agent uses day to day.
Reach for the HTTP form above when there's no `bastion` binary on PATH (a plain script, or a
health-check probe).

## `HELD_SESSION` — a tmux session that survives a workflow's own gaps

Registered alongside every other builtin workflow (`register_held_session`, inside
`register_builtin_workflows_with_registry`) but easy to miss in the catalogue because it holds a
terminal resource, not fleet coordination state, and has no per-workflow doc file of its own
elsewhere. A chain that needs the SAME tmux session alive across an arbitrarily long gap (an
operator thinking, a cross-repo lane running for hours) spawns a background renewal loop the first
time it asks for a session; every later node boundary that asks for the same session finds that
loop already keeping the lease alive, rather than every intervening node re-acquiring a
node-sized lease that lapses and reaps between calls — indistinguishable from a fresh session once
that happens.

```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
  -H 'Content-Type: application/json' -d '{"workflow_type":"HELD_SESSION","data":{}}'
```

Classifies an external kill or a dead tmux server as `session_lost` within a bounded time, rather
than hanging. Covered by four real-tmux integration tests (not headless-CI-safe):

```bash
cargo nextest run -p engine-core --test it held_session --no-capture
```

- `real_tmux_two_consecutive_nodes_reuse_one_session_with_identical_id`
- `real_tmux_held_session_renews_its_lease_before_expiry`
- `real_tmux_abandoned_lease_is_fail_closed_then_reconciled_via_steal_after`
- `real_tmux_external_kill_surfaces_a_node_error_within_a_bounded_time_not_a_hang`

## See also

- [orchestration.md](orchestration.md) — why an `ORCHESTRATION` chain takes a lease, escalations, the verification ledger, permission profiles
- [commander.md](commander.md) / [sweep.md](sweep.md) / [consolidate.md](consolidate.md) — the three coordination-native workflows, each with its own trigger example
- [orchestration-test-ladder.md](orchestration-test-ladder.md) — manual test recipes, including Tier 4 for coordination under concurrency
