---
type: Guide
title: Sandbox Orchestration Quickstart
description: Open this first, before touching the sandbox or the bench script — one command per situation, the five hard rules that must never break, and where to look when something's wrong.
doc_id: sandbox-orchestration-quickstart
layer: [engine]
project: engine-rs
status: active
keywords: [sandbox, orchestration, local model, quickstart, runbook, bench]
related: [local-model-bench, orchestration-workflow]
---

# Start here

```bash
curl -s localhost:18090/health
```

If that returns `{"status":"ok",...}`, the sandbox is up — go to the table below. If it errors or
times out, go to row 1 of the table first.

**You can stop reading after the table below if your situation matches a row.** Everything past
that is a lookup, not required reading.

## Which situation are you in?

| Situation | Do this |
|---|---|
| Sandbox not responding, or you're not sure it has today's code | **Action 1** below |
| Sandbox is healthy, you want to run the real verification test | **Action 2** below |
| A run just finished, you need to check it was actually local-only | **Action 3** below |
| A run failed and you don't know if it's a real bug or a model limit | **Action 4** below |

## The five hard rules — never break these

1. **Only ever target `127.0.0.1:18090`** (the sandbox's own `bastion serve`). **Never `:4317`**
   — that's the real, shared dev instance. The bench script's own `--dispatch orchestration` mode
   already refuses to run against anything else, but never override that guard by hand.
2. **Never pass `--parallel` together with `--dispatch orchestration`.** The script refuses this
   combination on its own — don't work around it. (Why: `docs/workflows/orchestration.md`'s
   "Concurrent same-repo dispatch" section.)
3. **Never set `auto_push` or `auto_pr` to `true`** unless the operator explicitly asked for that
   run. Both default to `false` in this repo's and the sandbox's `planning/harness.json` — leave
   them alone.
4. **Never `git push` from inside `core/engine-rs`**, in the main repo or the sandbox. Not your
   call — see rule 10 in `AGENTS.md`.
5. **Never touch `/Users/brandon/Dev/agentic-portfolio/planning/state.json` or `status.md`**
   directly. If something needs recording, say so in your final report instead.

## Actions

**Action 1 — Refresh the sandbox (2 min).** Run this whenever health check fails or you're unsure
the sandbox has the latest code:

```bash
cd /Users/brandon/Dev/engine-rs-sandbox-engrs1 && ./scripts/refresh.sh
```

Confirm it ends with `9 passed, 0 failed`. If anything fails, stop and report the failing step —
don't retry blindly.

**Action 2 — Run the real verification test (10-15 min).** From `core/engine-rs`:

```bash
python3 scripts/bench_local_models.py --dispatch orchestration \
  --sandbox-root /Users/brandon/Dev/engine-rs-sandbox-engrs1 \
  --models qwen2.5-coder:7b --tiers easy --agent-backends aider --repeat 1
```

This is sandbox-gated by the script itself (rule 1 above) and auto-launches
`system_monitor.py` alongside it — you don't need to start that separately.

**Action 3 — Confirm the run was actually local-only (5 min).** Find the run's record under
`planning/open-work/local-models/local-model-bench/results/<run-name>/artifacts/<job>/run-event.json`
and check every stage's telemetry:

```bash
grep -o '"tier":"[a-z]*"' <path-to-run-event.json> | sort -u
```

Every result must read `"tier":"local"`. If you see `"tier":"cloud"` anywhere, **stop and report
it** — do not assume it's fine. Name the exact stage.

**Action 4 — Tell a real bug from a model limit (5 min).** Read the last few entries of
`planning/open-work/local-models/local-model-bench/findings-log.md`. If the failure matches a
known pattern there, it's already understood — say so in your report. If it doesn't match
anything, describe exactly what happened (the error text, the stage, the model) and stop — do not
attempt an engine fix yourself.

## Not yours to decide or fix

- **`gh auth login` in the sandbox is not set up on purpose.** A `PullRequestNode` failure citing
  missing GitHub auth is expected, not a bug — report it as expected, don't try to configure auth.
- **The merge-train / same-repo parallel dispatch is not built.** If you're tempted to enable
  `--parallel` for orchestration mode because it looks like it should work now, don't — it's an
  open, unverified item (see the lookup table below).
- **Reverting old bench junk on `origin/main`, or filing upstream issues** — both are pending
  operator decisions noted in `planning/handoff.md`. Not blocking your work; not yours to act on.

## Lookup table

| Question | Go here |
|---|---|
| Why does the script refuse `--parallel` + `--dispatch orchestration`? | `docs/workflows/orchestration.md` § "Concurrent same-repo dispatch" |
| What does every bench flag do, and what does a leaderboard mean? | `docs/local-model-bench.md` |
| Why does the ledger composer need its own harness.json setting? | `planning/pre-plan/orchestration-improvements/orchestration-harness-only-policy-gap/notes.md` |
| What's still open/unverified about running this in parallel? | `planning/pre-plan/orchestration-improvements/end-of-chain-concurrency-cleanup/notes.md` |
| What changed this session, and what's still open? | `planning/handoff.md` (delete once you've read it) |
| How does the `llm_node` trait work, if you're touching engine code | `crates/engine-core/src/workflows/llm_node.rs`'s doc comment |
