---
type: Reference
title: The Pending Run Queue
description: The queue-not-run HTTP ingress (POST/GET /events/pending) — an authenticated route pair that durably records a workflow run request without dispatching it, for an untrusted caller (e.g. a public form) that must never trigger an unauthenticated run as a side effect of submitting.
doc_id: pending-run-queue
layer: [engine]
project: engine-rs
status: active
keywords: [pending-run-queue, events-pending, queue-not-run, PendingRunQueue, additive-seam]
related: [architecture, approval-ledger, docs-index]
---

# The Pending Run Queue (`EN.ticket.queue-not-run-event-ingress`)

`crates/engine-serve/src/pending.rs` adds two authenticated routes, `POST /events/pending` and
`GET /events/pending`, that durably record a workflow run request and return **202 without starting
anything**. This exists because `POST /events/` spawns the run before it returns — fine for a
trusted caller, wrong for an untrusted public form that should only ever be able to queue work for
a human to approve, never trigger an unauthenticated run as a side effect of submitting.

## The seam

`PendingRunQueue` is a plain trait — `append(workflow_type, data) -> pending_id` and
`list_open() -> Vec<PendingRun>` — extracted as `Option<web::Data<Arc<dyn PendingRunQueue>>>`,
**deliberately not a required [`crate::http::AppState`] field.** `AppState` is struct-literal-
constructed in `bastion` (`../bastion/src/serve/mod.rs`) and in five `crates/engine-serve/tests/*.rs`
files; a required `queue` field would be a cross-repo breaking change for a surface `bastion` is not
yet ready to wire. The `Option<web::Data<..>>` extractor is additive instead: `bastion` compiles
untouched, and both routes exist and answer 503 until one `.app_data(...)` line registers a real
queue. See `planning/decisions/D15-additive-seams-over-appstate-fields.md` (not linked — `planning/` is a
vault symlink excluded from the public repo, per `write-okf-markdown`'s cross-`planning/` rule).

No implementation ships in this crate yet — a durable, file- or database-backed `PendingRunQueue`
and the approve-and-execute join back into a real run are a separate follow-on block, out of scope
here by design.

## Routes

| Route | Behavior |
|---|---|
| `POST /events/pending` | 401 without a valid `X-API-Key`; 422 for a `workflow_type` unknown to the dispatcher (same body shape as `POST /events/`'s own 422); 503 with `{"error": "pending run queue not configured"}` when no queue is registered; otherwise appends to the queue and returns 202 `{pending_id, status: "pending"}` |
| `GET /events/pending` | 401 without a valid `X-API-Key`; 503 (same body) when no queue is registered; otherwise 200 `{"runs": [...]}`, newest-first |

Checks run in order — API key, then `workflow_type` validation, then append — so a rejected request
never reaches the store.

**Registration order matters.** Both routes are registered in `crate::http::configure` strictly
before `/events/{event_id}`: actix-web resolves routes first-registration-wins, so the literal
`"pending"` path segment would otherwise be swallowed by the `{event_id}` UUID extractor. The same
trap and fix pattern already exists for `/events/suspended` — see
[suspend-resume.md](suspend-resume.md).

## What a `pending_id` is not

A `pending_id` never resolves as a `run_id` — `GET /events/{pending_id}` returns 404, and queuing a
request leaves the live run registry and run records empty. This is the whole point of the route:
proving that submission and dispatch stay genuinely decoupled, not just that the 202 response looks
right.

## See also

- [architecture.md](architecture.md) — the engine's other injectable seams (`HttpPost`,
  `ChannelTransport`, `DocMaterializer`, the orphan lister) follow the same trait/live-impl/stub
  shape.
- [suspend-resume.md](suspend-resume.md) — the sibling literal-path-before-`{event_id}` route
  ordering trap, and its own runtime-inversion test.
