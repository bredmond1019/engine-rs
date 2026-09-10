---
type: Guide
title: Orchestration Test Ladder
description: Incremental tiers for proving ORCHESTRATION/CONDUCTOR mechanics work — gates, budget, abort, bails, coordination, autonomous selection — without paying to test how well Claude Code writes code.
doc_id: orchestration-test-ladder
layer: [engine]
project: engine-rs
status: active
keywords: [orchestration, conductor, testing, budget, bail, coordination]
related: [orchestration-workflow, trigger-and-monitor-runbook, brain:orchestration-cost-analysis-notes]
---

# Orchestration test ladder

Each tier proves one mechanic and nothing else. Run them **in order** — a later tier assumes the
one above it already passed. Trigger/monitor basics (env vars, health check, curl conventions)
aren't repeated here — see [trigger-and-monitor.md](trigger-and-monitor.md) first if you haven't.

**Already proven, not retested here:** `/orchestrate` sequencing a same-repo and a cross-repo
two-block chain (both via a live Claude Code session driving it turn by turn).

**What every tier below tests instead:** the HTTP-direct trigger path, with no chat session in the
loop. **Cost note:** a `block`-kind step always calls a real `SDLC_TASK`/`SDLC_FLOW` child —
`resolve_explicit_chain` (used by both an explicit `blocks` list and `CONDUCTOR` itself) always
sets `kind: Block`. There is no literal-$0 tier through the normal trigger path; "cheap" below
means small and bounded, via the smallest fixture spec you have — reuse `micro-spec-small` /
`micro-spec-large` (proven 2026-09-02, see
[orchestration.md § Status](orchestration.md#status-first-real-repo-chain-has-run-cross-repo-remains-unexercised))
or `smoke-sdlc-flow` ([sdlc-flow-smoke.md](sdlc-flow-smoke.md)) rather than authoring a new one.

## Tier 1 — the HTTP trigger path itself

**Trigger:**
```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"workflow_type":"ORCHESTRATION","data":{"blocks":[{"repo":"engine-rs","block_id":"<fixture-block>"}],"profile":"cheap-fast"}}'
```

| | |
|---|---|
| Watch for | `202 {run_id, event_id}` immediately — the walk runs async, this is not the result |
| Success | `GET /events/{event_id}` reaches `status: "succeeded"`; `lane-log.jsonl` gets exactly one `closed` line; that repo's `planning/state.json` flips the block to closed |
| If it doesn't work | `404` on trigger → engine routes not mounted, see [sdlc-flow-smoke.md § Prerequisites](sdlc-flow-smoke.md#prerequisites) items 1–3. Stuck at `running` past the fixture's known runtime → check `GET /api/coordination` for a held lease |

## Tier 2 — budget halt vs. clean abort

Two separate runs, same fixture, two-block explicit chain.

| Run | Trigger difference | Proves |
|---|---|---|
| A | Add `"policy":{"campaign_max_cost_usd_cents": 1}` (below the first block's real cost) | `lane-log.jsonl` gets a `budget_halted` line, not `bailed` — halting is a distinct, deliberate terminal state ([orchestration.md § Campaign identity](orchestration.md#campaign-identity)) |
| B | Trigger normally, then mid-flight: `curl -sf -X POST -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_SERVE_ADDR/campaigns/$CAMPAIGN_ID/abort"` | `lane-log.jsonl` gets a `cancelled` line; a block that had already closed before the abort keeps its `closed` line untouched |

**If it doesn't work:** no `budget_halted`/`cancelled` line at all → confirm you read `campaign_id`
from `GET /events/{event_id}` **before** the run finishes, not after (see
[trigger-and-monitor.md](trigger-and-monitor.md) step 3).

## Tier 3 — failure paths write escalations

| Case | How to produce it cheaply | Watch for |
|---|---|---|
| **`held`** (unmet gate) | Point a chain at a block with a real, currently-unmet `depends_on` edge — check `mev blocks --repo <repo>` for a `startable: false` candidate rather than authoring a fake one | The step waits at admission control; it does not fail |
| **`bailed`** | Cheapest real trigger seen so far: rerun a fixture with `auto_pr: true` against a repo state where PR creation will fail (stale base, no `gh` auth) — this is exactly how `micro-spec-small`'s first run bailed on 2026-09-02 (`node 'PullRequestNode' did not succeed`) | `escalations.jsonl` and `bails.jsonl` in that repo each get one new line; `check_id: "orchestration-step"` |

**Success:** run `check_escalations.py` (or read the file) and confirm the new line's
`verified_by` is evidence-shaped, not prose — [orchestration.md § A bail or a stuck operator
hold](orchestration.md#a-bail-or-a-stuck-operator-hold-now-writes-an-escalation-and-a-bails-entry-en15g)
names the exact shape.

**If it doesn't work:** `held` case never resolves → you picked a block whose dependency will
never clear; abort the run and pick another. `bailed` case produces no escalation line → check
`tracing::warn!` output in the serve log first; escalation writes are best-effort and swallow
their own failures by design.

## Tier 4 — coordination layer under concurrency

**Trigger:** fire two Tier-1-style single-block chains at the same time, against two **different**
repos (or two non-conflicting blocks in the same repo).

| | |
|---|---|
| Watch for | `curl -sf $BASTION_SERVE_ADDR/api/coordination \| jq` (no key needed) while both are live |
| Success | Both lanes appear as registered claims with fresh heartbeats; each holds its own per-block repo lease; neither reports the other's lease as held |
| If it doesn't work | One run sits at admission control the whole time → real lease contention on a **shared** repo — expected if you picked the same repo/block by mistake, not a bug. A lane reported `stale` immediately → clock skew between the two triggering processes, not a coordination defect |

## Tier 5 — `CONDUCTOR` refusal paths (genuinely $0)

No block ever executes here — this only proves the proposal logic.

**Trigger:**
```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
  -H 'Content-Type: application/json' -d '{"workflow_type":"ORCHESTRATION"}'
```
with `planning/objective.md` (HQ root) pointed at a week where every frontier candidate is
disqualified on purpose — no `tasks.json` yet, or already present in `git log -S<block_id>`.

| | |
|---|---|
| Watch for | The run returns almost immediately — no child `SDLC_FLOW`/`SDLC_TASK` ever starts |
| Success | The journal row (`kind: ConductorProposed`) names every candidate it dropped and why — `NotInSlate` / `MissingTasksJson` / the `git log -S` drop — see [orchestration.md § CONDUCTOR](orchestration.md#conductor-picking-tonights-chain-en12f) |
| If it doesn't work | Refuses with "needs either `blocks` or `roadmap`+`lane`" → `CONDUCTOR` isn't wired on the binary you're hitting; rebuild/reinstall before continuing. Proposes something anyway → your slate wasn't actually all-disqualified; re-check with `mev frontier --json` |

## Tier 6 — `CONDUCTOR` end-to-end (the one that counts)

Real `planning/objective.md`, real frontier, capped tight:

```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"workflow_type":"ORCHESTRATION","data":{"policy":{"conductor_max_chain_blocks":1,"campaign_max_cost_usd_cents":500}}}'
```

| | |
|---|---|
| Watch for | Same as Tier 1, plus the `ConductorProposed` journal row from Tier 5 preceding the real execution |
| Success | One real, non-fixture block closes, chosen and justified by the engine, not named by you — this is the actual test `EN.ticket.first-real-orchestration-run` and `planning/objective.md`'s own "Done when" checklist are waiting on |
| If it doesn't work | Same troubleshooting as Tiers 1–3, applied to whatever `CONDUCTOR` picked — read the journal row first to see *what* it picked before debugging *why* it failed |

## Optional — true $0 coordination testing

Tiers 2 and 4 only need coordination/budget mechanics, not a real code change. A `StepKind::Dispatch`
step runs a registered workflow with no model call at all — but it's reachable only through a
hand-authored `planning/lane-segments.json` (mev's format, not a plain `blocks` list), which is
more setup than a curl one-liner. Worth it only if you're running Tiers 2/4 often enough that even
the trivial fixture's cost adds up.

## Lookup table

| Question | Go here |
|---|---|
| Why this order, and what it's replacing | [trigger-and-monitor-rationale.md](trigger-and-monitor-rationale.md) |
| Full `CONDUCTOR` mechanics — caps, profiles, the pre-flight | [orchestration.md § CONDUCTOR](orchestration.md#conductor-picking-tonights-chain-en12f) |
| Env vars, health check, how to read a campaign back | [trigger-and-monitor.md](trigger-and-monitor.md) |
| The escalation/bail record shape | [orchestration.md § A bail or a stuck operator hold](orchestration.md#a-bail-or-a-stuck-operator-hold-now-writes-an-escalation-and-a-bails-entry-en15g) |
