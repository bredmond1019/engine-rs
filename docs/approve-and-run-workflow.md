---
type: Reference
title: APPROVE_AND_RUN
description: The APPROVE_AND_RUN micro-workflow (EN.8.D) — drains pending-harvest records into the operator queue, resolves a tapped verdict into one ledger row plus an authorized execution, and the two seams (lookup_pending/resolve_verdict) bastion's transport composes over
doc_id: approve-and-run-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [approve-and-run, harvest-gate, operator-queue, approval-ledger, drain, verdict, gate-id, seams, digest-mismatch, engine-serve]
related: [architecture, harvest-gate, operator-payload-contract, approval-ledger, docs-index]
---

# `APPROVE_AND_RUN`

`crates/engine-core/src/workflows/approve_and_run/` (`EN.8.D`) closes the loop the other
`EN.8.*` blocks opened: `HARVEST_APPROVE`'s `pending` records (`EN.7.C`) reach the operator
through the payload contract (`EN.8.A`), the depth-limited queue (`EN.8.B`), and the approval
ledger (`EN.8.C`) — `APPROVE_AND_RUN` is the workflow that drains those pending records onto the
queue, and turns a tapped verdict into exactly one ledger row plus, on approval, one execution
against the exact payload the operator reviewed. It composes those four existing primitives; it
invents no new persistence layer of its own.

## Module layout

| Module | Role |
|---|---|
| [`render`] | Pure `PendingHarvestRecord -> OperatorPayload` step. `render_and_validate` runs the render through `operator::validate`, producing a `ValidatedOperatorPayload` or an `OperatorValidationError` for the caller to route to session. `gate_id_for(artifact_id)` derives the deterministic `format!("approve-and-run:{artifact_id}")` id. Exactly three fixed response options: `OPTION_APPROVE` / `OPTION_SKIP` / `OPTION_OPEN_SESSION` — only the summary is truncated to fit `OperatorPayloadLimits`, never the option labels. |
| [`policy`] / [`profiles`] | `ApproveAndRunPolicy` (`drain_batch_max`, `harvest_item_priority`, `session_fallback_slug`) resolved through the standard four-layer precedence under `harness.json`'s `approve_and_run` section — see below. |
| [`drain`] | `drain(records, &mut queue, &limits, &policy, enqueued_at)` renders + validates a batch, enqueues conforming records as `ItemSource::GateApproval` items, routes non-conforming ones to `session-<slug>` (never dropped, never degraded to `notification`), and makes exactly one `next_deliverable` call. `DrainReport` reports `considered`/`skipped`/`truncated`/`delivered`/`session_routed`. |
| [`verdict`] | `decide(...)` resolves one delivered item's tapped option into exactly one `operator::ledger::record_decision` row. A digest mismatch re-queues the item (answered off the open slot, pushed back onto `pending`) and never authorizes execution. A matched `Approved` verdict returns `ExecutionAuthorization { item_id, url, payload }` — the `url`/`payload` copied verbatim from the stored pending-harvest record, never re-derived from the rendered summary. |
| [`graph`] | The declared `WorkflowSchema`/`NodeRegistry`/`Workflow` (`APPROVE_AND_RUN_WORKFLOW_TYPE`, start node `ApproveAndRunExecuteNode` = `APPROVE_AND_RUN_NODE_NAME`). `ApproveAndRunExecuteNode` composes (wraps, does not reimplement) `nodes::harvest_approve::HarvestApproveNode` over the injectable `HttpPost` seam, driven only when `ctx.event`'s `authorized` flag is `true` (built via `execution_event(auth)`), and stamps the resolved policy into its own `ctx.nodes` result. `registry()`/`workflow()` build the live-default graph; `registry_with(http_post, policy)` injects both for tests and for `engine-serve`'s per-event policy resolution. |
| `ApproveAndRunSeams` (top-level, `mod.rs`) | The two seams `bastion:BA.18.B` left open — see below. |

## Policy (`EN.8.D` task 2)

Three knobs, no model tier (this workflow drives no `ClaudeCodeStep`):

| Knob | Default | Meaning |
|---|---|---|
| `drain_batch_max` | `60` | Records one drain pass considers before reporting `truncated`. `60` covers the §7.5 Invariant-3 storm scenario (a 60-item pending-harvest set) in one pass. |
| `harvest_item_priority` | `0` | Uniform `effective_priority` a drained harvest item enqueues under — pending-harvest records carry no priority of their own, so ordering falls back to `operator::queue::compare_items`'s `enqueued_at`/`item_id` secondary keys. |
| `session_fallback_slug` | `"harvest-review"` | The `session-<slug>` a non-conforming record routes to instead of `notification`. |

Resolved through the standard four layers (event `policy` override > named `profile` >
`planning/harness.json`'s `approve_and_run.policy` defaults > built-in default), with
`baseline`/`cheap-fast`/`thorough` profiles documented in `planning/harness.json`.

## Seams (`ApproveAndRunSeams`, task 6)

`ApproveAndRunSeams` is the composed, `Send + Sync`, no-database/no-network object
`engine-serve` and `bastion` build against:

- **`lookup_pending(gate_id) -> Option<ValidatedOperatorPayload>`** — bastion's `PendingLookup`
  shape. Resolves `gate_id` to whatever is currently open on the queue via
  `OperatorQueue::open_item` (new accessor, `EN.8.D` task 6), re-validating the already-validated
  payload (idempotent) to reconstruct the `ValidatedOperatorPayload` wrapper.
- **`resolve_verdict(ApproveAndRunVerdict) -> Result<ApproveAndRunVerdictResolution, ApproveAndRunSeamError>`**
  — bastion's `VerdictSink` shape (modulo argument type: this crate defines its own
  `ApproveAndRunVerdict { gate_id, presented_digest, option_key, who, decided_at }` rather than
  naming bastion's `telegram::ResponseVerdict`, since engine-core has no dependency on
  `core/bastion`). Runs `verdict::decide` against whatever is open for `gate_id`, and — only on a
  matched `Approved` authorization — drives `graph::ApproveAndRunExecuteNode` against the stored
  record. Errors: `UnknownGate` (nothing open, and/or no stored record), `UnknownOption` (a tapped
  key outside the three stable option keys), `Execution` (the execute node returned a
  `NodeError`).

`engine-core` does not touch `core/bastion` — the final `run_server` wiring that hands these
seams to bastion's transport, and converts a real `telegram::ResponseVerdict` into
`ApproveAndRunVerdict`, is a bastion-side block.

## Registration (`engine-serve`)

`register_approve_and_run` (`crates/engine-serve/src/workflows.rs`) populates both the
`workflow_registry` and `schema_registry` for `APPROVE_AND_RUN`, part of
`register_builtin_workflows`. Unlike `HARVEST_APPROVE`/`OPPORTUNITY_*`, it does carry a policy
surface: the factory resolves `ApproveAndRunPolicy` per event via `resolve_policy_for_run_from`
against `PolicyConfigSource::Builtin` (channel/API-triggered, no repo checkout at dispatch time),
reading the event's optional `profile` (named-bundle selection) and `policy` (top-precedence
inline override) fields — the same two-field convention every other policy-aware factory in that
module reads — and seeds the resolved policy into the run.

## Testing

`crates/engine-core/tests/it/approve_and_run.rs` drives the full loop hermetically through
`ApproveAndRunSeams`' public API: the single-record approve path, a 60-item drain storm, a
digest-mismatch requeue, and a non-conforming record routed to session — no real network or
database, `FileApprovalLedger` rooted in a fresh tempdir plus a stub `HttpPost`.
