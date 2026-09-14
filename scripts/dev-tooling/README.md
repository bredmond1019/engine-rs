# engine-rs dev-tooling

Ad-hoc SDLC dev/testing tooling that engine-rs uses on itself: dispatch fixtures for exercising
the SDLC engines and the local-model bench, plus the scripts that drive them. This directory
groups what used to be a flat, mixed-in pile at the top of `planning/` and `scripts/` into one
findable home.

**This is not the fleet sandbox.** The two solve different problems:

| | This directory (`scripts/dev-tooling/`, `planning/dev-tooling/`) | The fleet sandbox |
|---|---|---|
| What it runs against | The **primary checkout** — real `bastion serve`, real git worktrees under `trees/sdlc/` | A **disposable full copy** of the fleet's repos, databases and ports |
| Scope | engine-rs's own SDLC-engine and local-model-bench fixtures | Any repo, any change, testable without touching the live checkout |
| When to reach for it | You want to smoke-test the SDLC engine itself, or run the local-model bench, in-repo | You want to test a change (yours or another session's) without risking the live checkouts other sessions are using |
| Docs | This file + the per-tool sections below | `` `agentic-portfolio/docs/sandbox/index.md` `` (overview) and `` `agentic-portfolio/docs/sandbox/quickstart.md` `` (start/check/refresh/teardown commands) |

If you're not sure which you need: if the question is "does the SDLC engine itself behave
correctly," use this directory's fixtures. If the question is "does my change break something
else in the fleet," use the sandbox.

## What's here

```
scripts/dev-tooling/
├── run_micro_spec.sh          drives the micro-spec-* fixtures via SDLC_FLOW
├── micro_spec_gate.sh         counter-file gate used by micro-spec fixture tasks
├── sdlc_smoke.sh              drives the smoke-sdlc-flow fixture via SDLC_FLOW
├── system_monitor.py          standalone CPU/memory/swap watcher (no fixture dependency)
├── bench_verify_medium.py     local-model-bench "medium" tier checker
└── bench_verify_hard.py       local-model-bench "hard" tier checker

planning/dev-tooling/
├── micro-spec-small/          reusable micro-spec fixture (harness.json + tasks.json)
├── micro-spec-large/          reusable micro-spec fixture (harness.json + tasks.json)
├── micro-spec-runs/           harvested run records (run_micro_spec.sh --out default)
├── micro-spec-runs-clean/     harvested run records (a --clean-adjacent test batch)
├── micro-spec-runs-verify/    harvested run records (a --defer-harvest verification batch)
├── smoke-sdlc-flow/           SDLC_FLOW smoke fixture (drives to the "done" state)
├── smoke-sdlc-review/         SDLC_FLOW smoke fixture (drives to the review branch)
├── smoke-sdlc-triage/         SDLC_FLOW smoke fixture (forces a triage path)
├── bench-easy/                trivial single-task dispatch fixture (registered as a Chores block)
└── bench-test-fixture/        default fixture spec for scripts/tests/test_bench_local_models.py
```

`planning/local-model-bench` (a compat symlink) and `planning/local-model-bench-run` (a live
SDLC worktree's spec-state directory) are **not** part of this grouping — see
[Why `local-model-bench-run` stays out](#why-local-model-bench-run-stays-out) below.

`bench_local_models.py` itself — the local-model bench driver — also stays in `scripts/`
(engine-rs's own tracked application code), not here; the two checker scripts it shells out to
per-tier (`bench_verify_medium.py`, `bench_verify_hard.py`) moved because they are dev/testing
fixtures, not the driver.

## The micro-spec harness

**What it's for:** comparing how the Rust SDLC engine and the legacy JS engine each execute the
exact same small spec — dispatch `k` times, harvest the result before each next dispatch, and
count distinct outcomes. It exists to catch a runner that harvests in the wrong order and silently
overwrites one run's evidence with the next one's before it's copied out.

**Run it:**

```bash
scripts/dev-tooling/run_micro_spec.sh --spec dev-tooling/micro-spec-small [options]
scripts/dev-tooling/run_micro_spec.sh --help          # full flag reference
```

Typed in a shell, from the engine-rs repo root, with `BASTION_ENGINE_API_KEY` set (via
`scripts/.env` or the environment) and `bastion serve` reachable at `BASTION_SERVE_ADDR` (default
`http://localhost:4317`).

| Flag | What it does |
|---|---|
| `--spec <slug>` | Required. Which fixture to dispatch — `dev-tooling/micro-spec-small` or `dev-tooling/micro-spec-large` |
| `-k <N>` | Number of consecutive dispatches (default 3) |
| `--out <dir>` | Where harvested records land (default `planning/dev-tooling/micro-spec-runs/`) |
| `--clean` | Reset-only: removes the worktree, deletes the branch, and clears leftover `sdlc/` state — **destructive**, use before a fresh manual run |
| `--defer-harvest` | Negative control only — harvests after all `k` runs instead of before each next one; documented to demonstrate the defect the normal order prevents, never a normal mode |

`micro_spec_gate.sh` is the counter-file gate one of each fixture's tasks calls to force a
retry/fix loop — it takes no fixture-specific arguments and needs no changes to reuse.

**Where output lands:** harvested `<spec>-<engine>-<profile>-<event_id>.json` +
`.meta.json` pairs under `planning/dev-tooling/micro-spec-runs/` (or wherever `--out` points).
`planning/dev-tooling/micro-spec-runs-clean/` and `-runs-verify/` are historical harvested batches
from past verification passes, kept as evidence.

**Fixture-evidence test:** `scripts/tests/test_run_micro_spec.sh` (gated in
`planning/harness.json` as the `micro-spec-runner` check) runs the runner against a shimmed
`curl` in an isolated `mktemp` playground — it never touches the real `planning/dev-tooling/`
fixtures, so it stays green regardless of their live state.

## The SDLC smoke tests

**What it's for:** proving a live `bastion serve` can run an `SDLC_FLOW` end to end — trigger,
dispatch, a real worktree write, and a terminal state on disk — using the smallest possible spec.

**Run it:**

```bash
scripts/dev-tooling/sdlc_smoke.sh              # trigger a fresh run and watch it to done
scripts/dev-tooling/sdlc_smoke.sh --watch ID   # attach to an existing run_id/event_id
scripts/dev-tooling/sdlc_smoke.sh --clean      # cleanup only — no trigger, no watch
scripts/dev-tooling/sdlc_smoke.sh --help       # full reference, including --repo
```

This drives `planning/dev-tooling/smoke-sdlc-flow/` specifically (spec_slug
`dev-tooling/smoke-sdlc-flow`). Full walkthrough, including what each check verifies and the
review/triage variants below: [`docs/workflows/sdlc-flow-smoke.md`](../../docs/workflows/sdlc-flow-smoke.md).

`planning/dev-tooling/smoke-sdlc-review/` and `planning/dev-tooling/smoke-sdlc-triage/` are
dispatched by hand (not by this script) to reach the SDLC review branch and a forced triage path,
respectively — see the same doc for the exact event bodies.

## The local-model bench

**What it's for:** running SDLC tasks against local models (via `aider`/`pi`) instead of Claude,
across `easy`/`medium`/`hard`/`edit` difficulty tiers, to measure whether a local model can close a
task at all.

**Run it:** the driver (`scripts/bench_local_models.py`, not moved) and its full operating guide
live outside this directory — see
[`docs/local-model-bench.md`](../../docs/local-model-bench.md) and
`` `agentic-portfolio/planning/open-work/local-models/local-model-bench/index.md` ``. This
directory holds only the two tier checker scripts the bench shells out to:

| Script | Tier | Checks |
|---|---|---|
| `bench_verify_medium.py` | medium | Run-length encode/decode logic in a generated `rle.py` |
| `bench_verify_hard.py` | hard | An arithmetic evaluator with precedence/parens, no `eval()` |

Both run from the worktree root (they resolve their target file relative to the current working
directory, not their own location) and exit non-zero with a clear `FAIL:` line when the target
file is missing or wrong. The tier `tasks.json` files that invoke them by path
(`` `agentic-portfolio/planning/open-work/local-models/local-model-bench/tiers/{medium,hard}/tasks.json` ``)
were updated to the new `scripts/dev-tooling/` path as part of this move.

`system_monitor.py` is unrelated to any fixture — it's a standalone CPU/memory/swap watcher for
keeping an eye on the machine during a long bench sweep:

```bash
python3 scripts/dev-tooling/system_monitor.py   # logs to /tmp/system_monitor.jsonl; --help for thresholds
```

## Why `local-model-bench-run` stays out

`planning/local-model-bench-run/` (the spec-state directory `bench_local_models.py` reuses by
default) was considered for this grouping and deliberately left where it is. The script computes
**both** its planning-side spec directory (`planning/<slug>/`) and its git-worktree directory
(`trees/sdlc/<slug>/`) from the same `--spec-slug` string. Moving only the planning-side directory
would either break the next `--spec-slug local-model-bench-run` resume (the old path is gone) or,
if the default were updated to a nested slug, break worktree/`clean_block` resolution (the actual
worktree stays at the un-nested `trees/sdlc/local-model-bench-run`, since only the planning-side
directory would move). Because this fixture already has a live worktree checked out, that
mismatch is real, not theoretical — see `core/engine-rs`'s own git history for the investigation.
The micro-spec/smoke-sdlc fixtures above don't have this problem because none of them currently
has a live worktree, so a fresh dispatch under the new nested slug creates matching paths on both
sides from scratch.

## See also

- `` `agentic-portfolio/docs/sandbox/index.md` `` — the fleet sandbox: a disposable full copy of the
  fleet, for testing a change without touching the live checkouts.
- `` `agentic-portfolio/docs/sandbox/quickstart.md` `` — start/check/refresh/teardown commands for
  the sandbox.
- [`docs/workflows/sdlc-flow-smoke.md`](../../docs/workflows/sdlc-flow-smoke.md) — full SDLC smoke
  test walkthrough.
- [`docs/workflows/orchestration-test-ladder.md`](../../docs/workflows/orchestration-test-ladder.md) —
  where these fixtures fit in the wider ORCHESTRATION test ladder.
- [`docs/local-model-bench.md`](../../docs/local-model-bench.md) — the local-model bench's own
  runbook.
- `` `agentic-portfolio/planning/open-work/local-models/local-model-bench/index.md` `` — the
  bench's HQ-vault home (results, tiers, checkers that outgrew this repo's own `planning/`).
