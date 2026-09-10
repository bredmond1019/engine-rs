---
type: Reference
title: Trigger & Monitor — Rationale
description: Read on a Friday review, not daily. Why this runbook exists, what it's replacing, what's actually verified vs. still a gap, and the cost math behind bothering with it.
doc_id: trigger-and-monitor-rationale
layer: [engine, meta]
project: engine-rs
status: active
keywords: [orchestration, conductor, cost analysis, unattended, rationale]
related: [trigger-and-monitor-runbook, orchestration-workflow, brain:orchestration-cost-analysis-context]
---

# Trigger & monitor — rationale

## What this replaces

`agentic-portfolio/planning/open-work/orchestration-runs/cost-analysis/` measured five real
sessions: a long-lived Claude Code chat that drives a chain turn-by-turn, re-reading its own
growing history every turn, cost $200–400 in cache-read tokens alone — larger than the actual
engine work it was coordinating, in every case. That's structural: the API's prompt cache is a
prefix match over the whole request, so cost grows with turn count squared, not with work done.

The `curl` in the runbook does the same job — start a chain, watch it, react if needed — as a
handful of short, stateless HTTP calls instead of one conversation that never resets. No call
depends on the one before it holding context; each is a fresh read.

## What this is not

It is not the base-template `orchestration-runbook.md` pattern (`/begin-orchestration` inside a
live Claude Code session, one session per repo, the sweep/commander watching them). That pattern
is real and still the right tool for a hand-driven multi-repo roadmap. This runbook is for the
other case: a single-repo chain the engine picks itself, with nobody driving turn by turn.

## What's actually verified vs. proposed

Built and confirmed by reading the source (not just docs) as of 2026-09-08:

- `CONDUCTOR` (`EN.12.F`) is closed and wired into production registration — a live dispatch with
  no `blocks`/`roadmap` reaches it, not a hard refusal.
- Cost caps are real: `campaign_max_cost_usd_cents` (default $50, `cheap-fast` profile $25) is
  wired into `integrate_chain`'s budget check at every block boundary, not just at start.
- `EmitStateNode`'s self-exemption from its own lane's lease shipped
  (`EN.ticket.emit-state-node-must-self-exempt-its-own-lease`, closed) — but a *related*, newer
  ticket (`EN.ticket.emit-state-agent-knob-has-no-production-caller`) is still open per
  `status.md`'s `next` list, naming a path where the knob still has no production caller. If a
  triggered run dies at its own terminal write with `E_QUIESCE_LEASE_HELD`, this is why — check
  that ticket's status before assuming it's a new defect.
- `ORCHESTRATION` has sequenced a real chain in a real repo exactly once: two fixture specs,
  single-repo, inside engine-rs, 2026-09-02 (`orchestration.md § Status`). A cross-repo chain, and
  a chain over real (non-fixture) corpus blocks driven by `CONDUCTOR` specifically, are both still
  unexercised. `EN.ticket.first-real-orchestration-run` is open for exactly this reason.

Not built, and not claimed by this runbook: a scheduler that triggers this for you (deliberately
undone, Fork 4), a premise-re-derivation node, capped/visible retry loops, and baked-in
run-id-to-transcript joining. See `cost-analysis/notes.md`'s seven suggested directions for the
full list and which of them exist.

## The honest cost estimate

No end-to-end unattended chain has been billed and measured yet, so there is no verified number.
Reasoning from what's confirmed: a watcher that polls `/events/{id}` and `/campaigns/{id}` a
handful of times over a multi-hour run, with no growing conversation behind it, pays only for
those small reads — call it low-to-mid tens of dollars for the actual engine work (bounded by the
$25–50 campaign ceiling above), plus whatever a human's own occasional `curl` costs, which is
effectively nothing. That is the entire point of routing coordination through HTTP calls instead
of a chat session: nothing here re-pays for its own history.

## When to revisit this file

When `EN.15.I`/`EN.15.J`/`EN.15.L` close (attach, operator-gate, verification ledger), when
`EN.ticket.first-real-orchestration-run` closes with a real measured run, or when the Mini
(rather than a laptop) becomes the standing runner and scheduling comes back on the table.
