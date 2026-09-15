---
type: Guide
title: Overnight Sandbox Model Sweep Quickstart
description: Open this to kick off tonight's full local-model sweep against the sandbox — one command, how to check on it, and where the leaderboard lands.
doc_id: overnight-sandbox-sweep-quickstart
layer: [engine]
project: engine-rs
status: active
keywords: [sandbox, overnight, local model, bench, leaderboard, quickstart]
related: [sandbox-orchestration-quickstart, local-model-bench]
---

# Start here

Run this from `core/engine-rs`. It runs in the foreground — start it in a terminal you can leave
open overnight (or under `nohup`/`tmux` if you need to close the terminal):

```bash
export BASTION_SERVE_ADDR=http://localhost:18090
export BASTION_ENGINE_API_KEY=$(grep BASTION_ENGINE_API_KEY /Users/brandon/Dev/engine-rs-sandbox-engrs1/scripts/.env | cut -d= -f2)

python3 scripts/bench_local_models.py \
  --dispatch orchestration \
  --sandbox-root /Users/brandon/Dev/engine-rs-sandbox-engrs1 \
  --models all \
  --agent-backends aider,pi \
  --repeat 1 \
  --run-name overnight-sandbox-$(date +%Y-%m-%d) \
  --deadline 07:00
```

**You can stop reading now if that command is running and logging `== [n/N] ... ==` lines.**
Everything below is what to do if something looks wrong, not required reading.

## Before you run it — read `docs/sandbox-orchestration-quickstart.md` first

That doc has the five hard rules (never target `:4317`, never `--parallel` with this dispatch
mode, `auto_push`/`auto_pr` stay off) and the "not yours to decide" list. This doc assumes you've
already read it — it does not repeat those rules.

## Why this exact command

| Choice | Why |
|---|---|
| `--dispatch orchestration` | The only mode that resolves worktree/artifact paths inside the sandbox's own checkout. `--dispatch direct` (the older, default mode) hardcodes paths to *this* repo, not the sandbox — using it here would silently read the wrong files. |
| No `--parallel` | `--dispatch orchestration` refuses it outright (concurrent jobs would race merging into the sandbox's shared `main`) — the script enforces this, you don't need to remember it. |
| `--models all` | Every pulled Ollama model, smallest first, filtered to tool-capable ones automatically (same selection logic every prior bench run has used). |
| No `--tiers` (defaults to all five) | Runs the full easy→rust difficulty ladder per model. |
| `--deadline 07:00` | Stops cleanly before the model finishes if it runs long — **it does not fail**, it just stops after the in-flight job and tells you to re-run the same command to resume where it left off. |
| `ledger_composer_model_tier: "local"` | Already set in the sandbox's own `planning/harness.json` (fixed 2026-09-14) — without it, every *passing* job would make one real cloud Claude call at chain-close regardless of every other setting. Don't remove it. |

## Checking on it without interrupting it

| Question | Command |
|---|---|
| Is it still running / how far along? | Watch the terminal — it logs one `== [n/N] ... ==` line per job |
| What's the leaderboard look like so far? | `cat planning/open-work/local-models/local-model-bench/results/overnight-sandbox-<date>/leaderboard.md` — regenerated after **every** job, safe to read anytime |
| Is memory/CPU okay? | `system_monitor.py` is auto-launched already — its log is `<run-dir>/system_monitor.jsonl` |
| Did it stop? Why? | The last log line always says why: `deadline reached`, `interrupted`, or a fatal job's error |

## If it stopped and you want to continue it

Re-run the **exact same command** (same `--run-name`). It skips every job that already has a
result and picks up where it left off — this is the script's normal resume behavior, not a
special recovery step.

## Not yours to decide

- **Don't add `--parallel`** even if the sweep feels slow — see the table above.
- **Don't switch to `--dispatch direct`** to try to speed it up — it will silently read/write the
  wrong repo's files when pointed at the sandbox (see the table above).
- **A `task_failed` outcome for a genuinely hard tier/small model is expected**, not a bug — that's
  the whole point of the sweep. Only stop and report if the *same* failure shape repeats across
  many models/tiers (that pattern usually means an engine or harness bug, not a model limit) — read
  `docs/local-model-bench.md`'s findings-log discipline before assuming which one it is.

## Lookup table

| Question | Go here |
|---|---|
| The 5 hard rules for anything sandbox-related | `docs/sandbox-orchestration-quickstart.md` |
| What every bench flag does, in full | `docs/local-model-bench.md` |
| Known engine/harness pitfalls already found | `docs/local-model-bench.md` § Pitfalls |
| Real bugs vs. genuine model failures, historically | `planning/open-work/local-models/local-model-bench/findings-log.md` |
| What changed this session that made tonight's run possible | `planning/handoff.md` |
