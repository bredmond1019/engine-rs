---
type: Reference
title: The Approval Ledger
description: engine-core::operator::ledger (EN.8.C) — the append-only {digest, decision, who, timestamp, rendered diff} record for every operator gate decision, its injectable ApprovalLedger seam and JSONL default impl, the digest-mismatch-forces-requeue enforcement, and the time-to-approval / decisions-per-day queries
doc_id: approval-ledger
layer: [engine]
project: engine-rs
status: active
keywords: [approval-ledger, operator, digest, provenance, time-to-approval, jsonl, EN.8.C]
related: [operator-payload-contract, harvest-gate, architecture, docs-index]
---

# The Approval Ledger (`EN.8.C`)

`crates/engine-core/src/operator/ledger/` persists one row per operator gate decision, from day
one, so that "it pauses and waits for your approval before it acts" is a claim with evidence behind
it rather than a mechanism nobody can point at. Time-to-approval falls out as a number.

This module (`engine-core`) is the **writer** half. `engine-serve` now ships the HTTP **read**
surface over it (`EN.ticket.approval-ledger-read-endpoint`) — see
["Reading the ledger over HTTP"](#reading-the-ledger-over-http) below. Rendering it is still a
different repo's block (`bastion-web:BW.ticket.approval-ledger-view`), which consumes that surface.

## The row (`ledger::record`)

`ApprovalLedgerRow` carries the five contract fields plus the two that make time-to-approval
derivable:

| Field | What it is |
|---|---|
| `item_id` | Which queued item the decision is about |
| `digest` | The digest the payload was **delivered** under |
| `decision` | `LedgerDecision` — `Approved`, `Skipped`, `RoutedToSession`, `Requeued` |
| `who` | The identity the decision arrived with — an opaque string, not a user account |
| `rendered_diff` | The rendered summary that was delivered, byte-identical |
| `delivered_at` / `decided_at` | The two timestamps `time_to_approval` subtracts |

The type is a plain data carrier: no I/O, no clock access. Both timestamps are constructor inputs,
so tests control them exactly — the same discipline `bastion`'s `BlockedEdgeRecord::new` follows.

`Requeued` is a decision variant rather than an error because a digest mismatch is a **recorded
outcome**, not a failure to record.

## Why a file, not a Postgres table

`crates/engine-core/src/cron/store.rs` already states this repo's reasoning for `EN.6.M`'s durable
cron state and it applies unchanged here: the `events` table is the orchestrator's
externally-provisioned schema (data contract §4), **there is no migration tooling in this repo to
add a table alongside it**, and CI has no live Postgres — every test that needs one is `#[ignore]`d
and therefore never actually runs. `bastion`'s `BA.18.A` blocked-edge sink reached the same shape
independently for the same reason.

So: an **append-only JSON-Lines file**, one row per line, opened in append mode per write, behind an
injectable trait. If a future block does want Postgres, it implements `ApprovalLedger` and nothing
above the seam changes. Reversing this needs a data-contract version bump (see `CLAUDE.md`'s update
protocol).

## The seam (`ledger::store`)

- `ApprovalLedger` — the trait: `append`, `read_all`, `rows_for(item_id)`.
- `FileApprovalLedger` — the JSONL impl. Two decisions on one item are two lines; nothing is ever
  overwritten or coalesced. A malformed line (a partial write from a killed process) is **skipped on
  read**, never a panic — one torn line must not make the whole ledger unreadable.
- `InMemoryApprovalLedger` — the test stub, so the gated suite writes no real files.
- `default_ledger_path(xdg_state_home, home)` — pure over its two arguments:
  `$XDG_STATE_HOME/engine-rs/approval-ledger.jsonl`, falling back to
  `$HOME/.local/state/engine-rs/approval-ledger.jsonl`, `None` when neither is set.

## The enforcement (`ledger::record_decision`)

The digest check lives **in the function, not at the call site**. `record_decision` takes both the
digest the payload was delivered under and the digest presented at decision time; when they differ,
the row written is always `Requeued` — the requested decision is discarded — and
`RecordDecisionOutcome::should_execute` is `false`.

It is therefore impossible to write an `Approved` row for a mismatched digest through this function,
whatever the caller passes. `should_execute` is true only for a matched digest *and* an `Approved`
outcome.

`rendered_diff` is **copied from the delivered payload by the caller and never re-derived here** —
the same no-re-derivation guarantee `PersistToBrainNode`/`HarvestApproveNode` hold for the harvest
payload ([harvest-gate.md](harvest-gate.md)). Re-deriving it would mean the stored evidence is not
the thing the operator actually saw.

`HarvestApproveNode` carries an optional ledger, `None` by default, so a deployment that configures
nothing behaves exactly as it did before (`CLAUDE.md` rule 6's behavior-stable-default requirement).

## The queries (`ledger::query`)

Pure functions over row slices — no I/O, no clock:

- `time_to_approval(row)` — `decided_at - delivered_at`.
- `time_to_approval_stats(rows)` — count, median, max, **ignoring `Requeued` rows**. A re-queue is
  not an approval and must not flatter the number.
- `decisions_per_day(rows)` — rows bucketed by UTC date. This exists for a specific bar: the
  `operator-surface` roadmap defines "operated" as **decision rows on at least 10 of any rolling 14
  days**, and that gate needs a query rather than a manual read.

## Reading the ledger over HTTP (`crates/engine-serve/src/approvals.rs`)

`EN.ticket.approval-ledger-read-endpoint` adds two authenticated GET routes to the shared route
table (`crate::http::configure`, so both the `engine-serve` binary and any embedding host — today,
`bastion` — pick them up automatically):

### `GET /approvals/ledger`

Query params: `item_id` (optional exact filter, delegating to `ApprovalLedger::rows_for`), `limit`
(default **100**, clamped to **1000** — a request for more is served the clamp, never rejected),
`offset` (default 0).

`ApprovalLedger::read_all`/`rows_for` return rows **oldest-first**. This endpoint reverses them, so
the HTTP response is **newest-first**. `total` counts the rows matching the `item_id` filter
**before** `limit`/`offset` are applied; paging past the end returns 200 with an empty `rows` array,
never an error. An absent or empty ledger file also yields 200 with an empty `rows` array — never a
404.

```json
{
  "rows": [ /* ApprovalLedgerRow, newest decided_at first */ ],
  "total": 12,
  "limit": 100,
  "offset": 0
}
```

### `GET /approvals/ledger/stats`

Delegates to `engine_core::operator::ledger::{time_to_approval_stats, decisions_per_day}` — neither
statistic is re-derived here. Note the asymmetry those two functions already encode, unchanged by
this endpoint: `time_to_approval_stats` **excludes** `Requeued` rows (a re-queue is not an approval),
while `decisions_per_day` **includes** them.

```json
{
  "time_to_approval": { "count": 0, "median_seconds": null, "max_seconds": null },
  "decisions_per_day": { "2026-08-17": 3 }
}
```

`median_seconds`/`max_seconds` are `null` **exactly when** `count` is `0`, matching
`time_to_approval_stats`'s `Option<Duration>` returns.

### Auth and blocking I/O

Both handlers call `crate::http::check_api_key` first, like every other handler in this crate — a
missing or wrong `X-API-Key` is **401**. The ledger's file read happens inside `web::block`, never
directly on the async worker.

### The 503-until-wired contract (additive seam, D15)

Both handlers take the ledger as `Option<web::Data<Arc<dyn ApprovalLedger>>>`, not as a required
`AppState` field — `AppState` is public and struct-literal-constructed in `bastion` and in five
`engine-serve` test files, so a required field would be a cross-repo breaking change. The routes are
registered **unconditionally**. When no ledger is registered, both return **503** with a stable JSON
body (`{"error": "approval ledger not configured"}` or equivalent — identical between the two
routes), never 500, never a panic. This lets the routes exist and self-describe before any host wires
them. See `planning/decisions/D15-additive-seams-over-appstate-fields.md` for the full rationale.

### Wiring it up: the one line a host owes

The routes do nothing useful until the embedding host hands them a ledger. **Reader and writer must
share the same `Arc`** — a second, independently-constructed `FileApprovalLedger` would resolve
`default_ledger_path` a second time and silently read an empty file while the writer appends
elsewhere, with **neither side erroring**; the reader would just render nothing. `bastion` already
builds the writer's `Arc<FileApprovalLedger>` at `src/serve/mod.rs:587` — that exact `Arc` is what
must be registered:

```rust
// in bastion's serve boot, alongside the existing engine_serve::http::configure(..) call
let ledger: std::sync::Arc<dyn engine_core::operator::ledger::ApprovalLedger> = ledger_arc.clone();
app_data(actix_web::web::Data::new(ledger))
```

This wiring is tracked as carryover `approval-ledger-reader-unwired-in-bastion` in
`planning/state.json`, alongside `approve-and-run-seams-unwired-in-bastion` and
`orphan-reconcile-unwired-in-bastion` — three engine seams now waiting on the same bastion file.
Whoever next works there should take all three together.

## See also

- [operator-payload-contract.md](operator-payload-contract.md) — the payload the digest is computed
  over and the queue that delivers it (`EN.8.A`/`EN.8.B`).
- [harvest-gate.md](harvest-gate.md) — the gate whose decisions this ledger records.
