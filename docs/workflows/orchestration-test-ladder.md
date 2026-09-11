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

## Tier 7 — the coordination-native workflows, each dispatched alone

Proves each of the three coordination workflows runs standalone, outside any `ORCHESTRATION`
chain. Full mechanics for each: [commander.md](commander.md), [sweep.md](sweep.md),
[consolidate.md](consolidate.md).

| Workflow | Trigger | Success |
|---|---|---|
| `COMMANDER` | `curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" -H 'Content-Type: application/json' -d '{"workflow_type":"COMMANDER","data":{"root":"<brain-root>","repo":"<repo>","agent":"<agent-id>"}}'` | The run's journal shows the drain step, then the triage step; `<brain-root>/planning/roadmaps/*/lane-log.jsonl`... no new content is required — a drain over an empty queue is a normal, successful no-op |
| `SWEEP` | `curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" -H 'Content-Type: application/json' -d '{"workflow_type":"SWEEP","data":{"root":"<brain-root>","roadmap":"<roadmap-slug>"}}'` | A new file appears at `<roadmap-dir>/sweeps/<ts>.json`; run it twice and confirm the second run's `diff` against the first is empty when nothing changed in between |
| `CONSOLIDATE` | `curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" -H 'Content-Type: application/json' -d '{"workflow_type":"CONSOLIDATE","data":{"brain_root":"<brain-root>","roadmap_slug":"<roadmap-slug>"}}'` | `disposal.json` is written (or updated) for that roadmap; the watermark advances — confirm via a second run producing no new rows |

**If it doesn't work:** any of the three returns `404` on trigger → same check as Tier 1, items
1–3. `SWEEP` writes an empty diff on a roadmap you know changed → confirm you pointed `root` at the
brain root (`agentic-portfolio/`), not this repo — `roadmap` alone is not enough to locate the
snapshot history.

## Tier 8 — `HELD_SESSION` survives a gap between node calls

Full mechanics: [coordination.md § HELD_SESSION](coordination.md#held_session--a-tmux-session-that-survives-a-workflows-own-gaps).

**Trigger:**
```bash
curl -sf -X POST "$BASTION_SERVE_ADDR/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
  -H 'Content-Type: application/json' -d '{"workflow_type":"HELD_SESSION","data":{}}'
```

| | |
|---|---|
| Watch for | The run acquires a real tmux session (check `tmux ls` on the host for a new session name) |
| Success | A second dispatch against the same session identity reuses it rather than creating a new tmux session; killing the tmux server mid-run (`tmux kill-server`) surfaces a bounded-time `session_lost` error in the run's result, not a hang |
| If it doesn't work | No tmux session appears at all → `tmux` isn't on PATH where `bastion serve` runs, or the held-session lease directory (`coord::resolve_lock_dir`) isn't writable. Run hangs past a few seconds after `tmux kill-server` → the abandoned-lease reclaim path regressed; this is exactly what `real_tmux_external_kill_surfaces_a_node_error_within_a_bounded_time_not_a_hang` gates in CI, so check that test first before debugging live |

## Tier 9 — `SDLC_FLOW`'s pre-run baseline snapshot and `failureClass: escalate` (`EN.17.G`)

Proves the test-stage gate mechanics, not `ORCHESTRATION` — run this against a plain `SDLC_FLOW`
dispatch. Full mechanics: [sdlc-flow.md § Baseline-diff pre-run snapshot](sdlc-flow.md#baseline-diff-pre-run-snapshot-and-failureclass-escalate-en17g).

| Case | How to produce it | Watch for |
|---|---|---|
| **Baseline snapshot catches a task's own regression** | Give a fixture spec's `harness.json` a `baseline-diff` check with a `baselineCommand`, and a task that adds one net-new entry to that command's output | The check FAILS on that task — confirming the pre-run snapshot (not a post-implementation one) is what it's judged against; re-run the identical fixture with no net-new entry and confirm it passes |
| **`failureClass: escalate` skips retries** | Mark one check `"failureClass": "escalate"` in the fixture `harness.json`, and make it fail on task 1's first attempt | The task bails on the FIRST attempt (`max_attempts` never consumed past 1); its bail `check_id` equals the check's declared `name`, not a generic label |

**If it doesn't work:** the net-new entry passes anyway → confirm a baseline file actually exists at
`<spec_dir>/sdlc/baseline-<check-name-slug>.txt` before the first task ran; if absent, the run
fell back to the pre-`EN.17.G` post-implementation behavior (not a bug, just the documented
fallback — the `CheckResult.message` states this explicitly). The escalate case still retries →
confirm the check's `failureClass` key is spelled exactly `escalate` (any other value, including a
typo, is `Fixable`, behavior-stable per CLAUDE.md standing rule 6).

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
| The `/api/coordination/*` HTTP read/write surface | [coordination.md](coordination.md) |
| `COMMANDER`/`SWEEP`/`CONSOLIDATE` full mechanics | [commander.md](commander.md) · [sweep.md](sweep.md) · [consolidate.md](consolidate.md) |
| Baseline-diff snapshot and `failureClass: escalate` full mechanics | [sdlc-flow.md § Baseline-diff pre-run snapshot](sdlc-flow.md#baseline-diff-pre-run-snapshot-and-failureclass-escalate-en17g) |
