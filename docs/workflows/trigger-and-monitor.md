---
type: Guide
title: Trigger & Monitor Runbook
description: Start here to fire an unattended ORCHESTRATION run and check on it yourself, without opening a live Claude Code coordinating session. One command, a situation table, then stop reading.
doc_id: trigger-and-monitor-runbook
layer: [engine]
project: engine-rs
status: active
keywords: [orchestration, conductor, trigger, monitor, campaign, unattended, runbook]
related: [orchestration-workflow, debrief-workflow, workflows-index]
---

# Trigger & monitor — start here

```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" \
  -H "X-API-Key: $BASTION_ENGINE_API_KEY" -H 'Content-Type: application/json' \
  -d '{"workflow_type":"ORCHESTRATION","data":{"profile":"cheap-fast"}}'
```

That's the whole trigger — no `blocks`, no `roadmap`, so `CONDUCTOR` picks the chain itself from
`planning/objective.md` + the frontier slate. Returns `{"run_id": "...", "event_id": "..."}`
(same value). Everything below is a lookup table, not reading. **Why this replaces a long Claude
Code session:** [trigger-and-monitor-rationale.md](trigger-and-monitor-rationale.md).

## Which situation are you in

| Situation | Do this |
|---|---|
| Never run a real unattended chain before | Run the two-block fixture smoke first — [orchestration.md § Status](orchestration.md#status-first-real-repo-chain-has-run-cross-repo-remains-unexercised) has the exact commands used 2026-09-02 |
| `planning/objective.md` (HQ root) is more than a week old | Rewrite it first — `CONDUCTOR` refuses to propose anything with no current objective |
| A run is in flight | Skip to **Watch it**, below |
| Something needs a decision | Read `escalations.jsonl` for the repo the chain ran in — see lookup table |

## Do this, in order

1. **Confirm the engine is up** — `curl -sf $BASTION_SERVE_ADDR/health` — 1 min.
2. **Trigger** — the command at the top. Save the `event_id` it returns — 1 min.
3. **Learn the campaign id** — `curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY"
   "$BASTION_SERVE_ADDR/events/$EVENT_ID" | jq '.nodes.OrchestrationRunNode.campaign_id'` — 2 min.
4. **Watch it** — no fixed cadence; check back whenever. Two commands, both read-only:
   `curl -sf $BASTION_SERVE_ADDR/api/coordination | jq` (no key needed — leases, live lanes) and
   `curl -sf -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_SERVE_ADDR/campaigns/$CAMPAIGN_ID" | jq`
   (cost/tokens so far, per-run rollup) — 2 min per check.
5. **Once it stops, ask for the brief** —
   `curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" -H 'Content-Type: application/json' -d '{"workflow_type":"DEBRIEF","data":"'"$CAMPAIGN_ID"'"}'`
   — DEBRIEF is **not** automatic; you always ask for it — 2 min.

## Not yours to do

- **Cross-repo chains** — unexercised past a single repo. Don't hand `CONDUCTOR` an objective that
  implies more than one repo yet; that's still a "treat as a test" event, not routine.
- **Widening the permission profile above `standard`** — `ClearOperatorGate` is denied at every
  level regardless, so there's no upside, only surprise.
- **Scheduling this on a cron** — deliberately not done (Fork 4). Trigger it yourself, on purpose,
  each time, until the Mini is confirmed as the stable unattended runner.

## To stop a run

```bash
curl -sf -X POST -H "X-API-Key: $BASTION_ENGINE_API_KEY" "$BASTION_SERVE_ADDR/campaigns/$CAMPAIGN_ID/abort"
```
`202 {"campaign_id":"...","status":"aborting"}`. Stops the whole chain, not just the current block.

## Lookup table

| Question | Go here |
|---|---|
| What is `CONDUCTOR` actually choosing, and why these cost caps? | [orchestration.md § CONDUCTOR](orchestration.md#conductor-picking-tonights-chain-en12f) |
| Why this shape instead of a Claude Code coordinating session? | [trigger-and-monitor-rationale.md](trigger-and-monitor-rationale.md) |
| A block bailed / an operator gate came up | That repo's `escalations.jsonl` and `bails.jsonl` (chain-originated entries carry `check_id: "orchestration-step"` or `"operator-hold"`) |
| What did the whole fleet's coordination layer see? | `curl -sf $BASTION_SERVE_ADDR/api/coordination` — no key needed |
| I need the env vars (`BASTION_SERVE_ADDR`, `BASTION_ENGINE_API_KEY`) | `core/bastion/.env` |
| How do I get the morning brief again later? | Re-POST the `DEBRIEF` command in step 5 with the same campaign id — it's idempotent, reads the journal fresh each time |
