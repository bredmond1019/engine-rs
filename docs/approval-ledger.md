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

The ledger is the **writer** half only. Rendering it is a different repo's block
(`bastion-web:BW.ticket.approval-ledger-view`), and this module deliberately ships no HTTP surface.

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

## See also

- [operator-payload-contract.md](operator-payload-contract.md) — the payload the digest is computed
  over and the queue that delivers it (`EN.8.A`/`EN.8.B`).
- [harvest-gate.md](harvest-gate.md) — the gate whose decisions this ledger records.
