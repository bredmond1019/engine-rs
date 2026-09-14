---
type: Guide
title: Local-model bench
description: Runbook for scripts/bench_local_models.py -- comparing local Ollama models and the aider and pi agent backends on real SDLC_FLOW runs, unattended, with every pitfall found building it.
doc_id: local-model-bench
layer: [engine]
project: engine-rs
status: active
keywords: [local models, ollama, benchmark, aider, pi, runbook]
related: [docs-index, workflows-index]
created: 2026-09-14
updated: 2026-09-14
---

# Local-model bench

A script that runs the same small coding tasks through the real engine once per local model and
agent backend, and tells you which combinations actually get work done. New to the engine? Start
with [workflows/README.md](workflows/README.md).

## What this page is for

Use it to **run the bench**, **read its results**, and **understand why it is built the way it is**.
Almost every design choice below exists because the obvious version silently produced wrong data —
the [Pitfalls](#pitfalls-every-one-of-these-happened) section lists each one.

- **Model** — a model pulled into your local [Ollama](https://ollama.com) (`ollama list`).
- **Agent backend** — the coding agent the engine drives for the implement stage: `aider` or `pi`
  (see [workflows/README.md § `agent_backend`](workflows/README.md)).
- **Tier** — one fixed task set (`easy`, `edit`, `medium`, `hard`, `rust`).
- **Job** — one tier × model × backend × repeat. One job is one real `SDLC_FLOW` run.

Everything runs locally and costs $0.

## Quickstart

All commands are typed in a **terminal**, from `core/engine-rs`.

```bash
# 1. Make sure the engine server is up (it is usually already running).
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:4317/health     # expect 200

# 2. Preflight + job plan, dispatching nothing. Fix anything it reports.
python3 scripts/bench_local_models.py --models all --dry-run

# 3. The unattended sweep. Re-running the SAME command resumes where it stopped.
python3 scripts/bench_local_models.py --models all --run-name overnight-1 --deadline 07:30

# 4. Read the result.
sed -n '/## Leaderboard/,/## Jobs/p' planning/local-model-bench/results/overnight-1/leaderboard.md
```

**Always pass `--run-name`** for a sweep that may cross midnight. The default name is today's date,
so a restart after midnight would start a new run instead of resuming.

### What must exist first

| Need | Why | If it's missing |
|---|---|---|
| `bastion serve` on `127.0.0.1:4317`, built from current engine-rs | It executes the runs; engine fixes only take effect once installed | Rebuild: `cargo install --path ../bastion --force`, then restart (below) |
| `BASTION_ENGINE_API_KEY` in `scripts/.env` | Authenticates dispatches | Preflight reports the key as rejected |
| Ollama running, models pulled | The models under test | `ollama pull <model>` |
| `aider` and `pi` on `PATH` | The two backends | Preflight names the missing binary; or pass `--agent-backends aider` |
| Tier files on `origin/main` | Every run's worktree is cut from `origin/main`, not local `HEAD` | Preflight lists the missing path |
| Free memory | 32B models plus a 16k context need ~24 GB | Reboot before a long sweep; see [Troubleshooting](#troubleshooting) |

**Restarting `bastion serve`** (terminal, from the brain root `agentic-portfolio/`). The two `.env`
files hold **different** values for `BASTION_ENGINE_API_KEY`; source `engine-rs/scripts/.env` last so
its value wins, or every dispatch returns `unauthorized`.

```bash
set -a; source core/.env; source core/engine-rs/scripts/.env; set +a
export DATABASE_URL="postgres://orchestration:orchestration@localhost:5432/orchestration_dev"
pkill -TERM -f "bastion serve --addr 127.0.0.1:4317"; sleep 2
nohup bastion serve --addr 127.0.0.1:4317 > /tmp/bastion_serve_restart.log 2>&1 & disown
```

**Restarting the server kills any live run.** Check nothing is dispatched first.

## How one job runs

The script is a plain Python loop: it prepares a spec directory, asks the engine to run it, waits,
then inspects the result itself instead of trusting the engine's verdict.

```mermaid
flowchart TD
    A["Preflight: server, key, models,<br/>tiers not already done on origin/main"] --> B["Write tasks.json + harness.json<br/>into planning/local-model-bench-run/"]
    B --> C["POST /events/ SDLC_FLOW<br/>(no block_id)"]
    C --> D["Engine: worktree from origin/main,<br/>implement → test → triage → review"]
    D --> E["Poll; abort if over --timeout-minutes"]
    E --> F["Re-run the tier's checks<br/>on the final worktree"]
    F --> G["Classify, save evidence,<br/>write record, regenerate leaderboard"]
    G --> H["Remove worktree + branch"]
```

1. **Preflight** refuses to start on anything that would make results meaningless (details in
   [Pitfalls](#pitfalls-every-one-of-these-happened)).
2. Every job **reuses one spec directory**, `planning/local-model-bench-run/`, overwriting its
   `tasks.json` and a `harness.json` generated from those same tasks.
3. It dispatches **`SDLC_FLOW` directly, with no `block_id`**, so no block is ever closed and no
   branch is ever merged or pushed.
4. The engine runs the tasks in a worktree at `trees/sdlc/local-model-bench-run`.
5. The script waits. Past `--timeout-minutes` it calls `POST /events/{id}/abort`; if the run still
   does not stop, **the sweep halts** rather than clean up under a live run.
6. It re-runs every task's checks on the final tree and compares that with the engine's own verdict.
7. It saves evidence, writes the job's JSON record, and regenerates the leaderboard.
8. It removes the worktree and branch before the next job.

**What you personally do:** steps 1–4 of the Quickstart. Everything else is automatic.

**Job order is rep → tier → model → backend.** If a deadline or crash stops the sweep, every model
has the early tiers rather than half the models having everything.

## The tiers

Fixtures live in the private vault at `planning/local-model-bench/tiers/<tier>/tasks.json` (bare path
— `planning/` is not in the public repo). A tier's expected files and checks are written once, there.

| Tier | Task | Checked by | Tests |
|---|---|---|---|
| `easy` | Write `BENCH_MARKER.md` with one exact line; write `bench_greeting.py` with `greet()` | `grep -qx`, a Python one-liner | Following exact instructions |
| `edit` | Change one line of the existing `scripts/bench_verify_medium.py` | `planning/local-model-bench/checkers/verify_edit_one_line.py` — exactly 1 line added, 1 removed vs `origin/main` | Surgical edits (aider's whole-file format often drops or rewrites lines) |
| `medium` | Run-length encode/decode in `rle.py` | [`scripts/bench_verify_medium.py`](../scripts/bench_verify_medium.py) | Real logic, exact outputs |
| `hard` | Arithmetic evaluator with precedence and parentheses, no `eval()` | [`scripts/bench_verify_hard.py`](../scripts/bench_verify_hard.py) | Parsing |
| `rust` | `top_words()` in a standalone `word_freq.rs` | `planning/local-model-bench/checkers/verify_rust_word_freq.sh` — 8 tests via `rustc --test`, no Cargo | Rust that compiles |

**Every check runs under `perl -e 'alarm shift; exec @ARGV' 60`** (macOS has no `timeout`), and the
Rust test binary under a 30 s alarm. Model-written code can loop forever.

**Changing a tier:**
- Keep each task's `title` unique — see pitfall 7.
- A repo path a check reads must already be on `origin/main`. A new checker goes in the vault's
  `checkers/`, reached through the worktree's `planning/` symlink, which also keeps it out of aider's
  repo map.
- Prove every new checker both ways: a correct solution passes, and a wrong or missing one fails.

## Reading the results

Everything for a run lands in the private vault under `planning/local-model-bench/results/<run-name>/`:

| File | What it is |
|---|---|
| `leaderboard.md` | Pass counts per model × backend × tier, median passing time, main failure categories, engine anomalies, and one row per job |
| `summary.json` | Every job record in one file |
| `<tier>/<backend>/<model>-r<rep>.json` | One job's record. Its presence is what makes a re-run skip that job |
| `artifacts/<job>/` | Evidence: the SDLC state file, the run event, aider's chat history (`.txt`), `git.txt` (log + diff vs `origin/main`), final check output |

### `outcome` — what happened to the job

| Value | Meaning | Re-run on resume? |
|---|---|---|
| `passed` | The engine passed every task **and** the final tree passes every check | no |
| `task_failed` | Anything short of that; see `failure_category` | no |
| `timeout` | Hit `--timeout-minutes` and was aborted | no |
| `exception` · `dispatch_error` · `infra_error` | The harness or server failed, not the model | **yes** |

### `failure_category` — why

| Value | Whose failure | Meaning |
|---|---|---|
| `no_change` | model | Nothing in the worktree changed |
| `wrong_path` | model | Files changed, but none of the task's declared files |
| `check_failed` | model | The right files changed, but the checks fail |
| `engine_rejected_correct_work` | **engine** | Final checks pass, yet the engine failed a task — or burned all attempts on a correct task so a later one never ran |
| `engine_accepted_failing_work` | **engine** | The engine marked tasks done, but the checks fail |
| `abort_unconfirmed` | **engine** | A timed-out run did not stop; the sweep halted |

**Any `engine_*` row is a bug report, not a model result.** Open that job's `artifacts/` first.

## Options

Run `python3 scripts/bench_local_models.py --help` for the full list. Every flag also has a
`BENCH_LOCAL_MODELS_*` environment variable.

| Flag | Default | Notes |
|---|---|---|
| `--models` | required | Comma-separated, or `all` (every pulled model with completion capability, smallest first; embedding models and `-ctxN` variants skipped) |
| `--tiers` | `easy,edit,medium,hard,rust` | |
| `--agent-backends` | `aider,pi` | |
| `--repeat` | `1` | All of rep 1 finishes before rep 2 starts |
| `--run-name` | today's date | The resume key |
| `--deadline` | none | Local `HH:MM`; no new job starts after it. A time already past means tomorrow |
| `--timeout-minutes` | `40` | Per job, then abort |
| `--ollama-num-ctx` | `16384` | Context baked into a `<model>-ctx<N>` Ollama variant used by **both** backends. `0` disables — Pi will then fail |
| `--review-mode` | `end_only` | One local-model review of the whole run. `per_task`/`trivial_skip` depend on the aider review-diff fix |
| `--test-dispatch` | `inline` | Never `queue_park` — pitfall 9 |
| `--spec-slug` | `local-model-bench-run` | Refused if it names a registered block |
| `--dry-run` | off | Preflight and plan only. Reports, but does not create, context variants |

## Pitfalls (every one of these happened)

Each was measured on 2026-09-14 while getting the first clean run. The fix column says where the
protection now lives, so nobody removes it as clutter.

| # | What went wrong | Cause | Protection now |
|---|---|---|---|
| 1 | Aider overwrote `CLAUDE.md` with a JSON blob | The implement prompt asked for a JSON reply; aider applies replies as file edits | Engine: aider gets an act-now prompt |
| 2 | Aider wrote `path/to/LOCAL_ORCH_1.md` | Small models copy aider's example path | Engine: declared `files[]` passed to aider as file args and named in the prompt |
| 3 | Correct aider work failed write-verification, then review | Aider commits during the call, so `git diff HEAD` is empty | Engine: `task_base_sha` — diffs use the task's first-attempt HEAD |
| 4 | Pi claimed writes it never made | Prompt asked for JSON; Pi replied with JSON instead of calling its write tool | Engine: Pi gets a tool-call contract |
| 5 | Pi flailed or wrote nothing | **Ollama's default context truncated Pi's ~9k-token prompt to ~2k tokens**; Pi sends no `num_ctx` | Bench: `--ollama-num-ctx` model variants |
| 6 | Through ORCHESTRATION, a passing job **closed its block, merged the branch into `main` and pushed `origin/main`**; every later job was skipped as "block closed" | ORCHESTRATION's integrate step merges, pushes and closes by design | Bench: dispatches `SDLC_FLOW` with no `block_id`; refuses a spec slug that is a block |
| 7 | Tasks marked `done` with zero attempts | `LoadTaskStateNode` resumes a task when `feat(sdlc): <id> — <title>` is in git history; the pushed merge put bench titles there | Bench preflight: refuses a task title already committed on `origin/main` |
| 8 | Tasks "passed" without work | The same merge put the easy tier's output files on `origin/main`, which every worktree is cut from | Bench preflight: runs every check on a pristine `origin/main` worktree and refuses any that already pass |
| 9 | Every task failed with no feedback | `test_dispatch: queue_park` resumes checks with `passed: null` → failed, `check_results: []` | Bench: `--test-dispatch inline`. Engine defect still open |
| 10 | A check ran 7+ min, grew to 2.9 GB, filled swap; the bench was OOM-killed and abort could not land | A model's evaluator looped; engine checks have no timeout, and cancellation waits for the node boundary | Bench: 60 s alarm on every check. Engine defect still open |
| 11 | Aider discarded a correct edit, then edited `.gitignore` | A reply naming another tracked file makes aider add it and re-prompt **before** applying the edit | Bench: `edit` tier targets a file that names no other tracked file. Engine defect still open |
| 12 | `review_mode: end_only` failed with HTTP 404 | `EndReviewNode` ignored the local review tier | Engine: wired to the local transport |
| 13 | A manual Pi run hung for 7 min with no output | Pi was waiting on an open stdin | The engine already nulls stdin; only affects hand-run `pi` |
| 14 | A checker failed correct work | `py_compile` with `cfile=/dev/null` raises on every file | Fixed in the checker; caught by testing it against a correct edit first |

## Troubleshooting

| Symptom | Likely cause | What to check |
|---|---|---|
| Every dispatch returns `unauthorized` | Server started with the other `.env`'s API key | Restart with `engine-rs/scripts/.env` sourced last |
| Preflight: "checks already pass on a pristine origin/main" | A tier's output is already on `origin/main` | Rename the tier's output files |
| Preflight: "origin/main already has a commit 'feat(sdlc): …'" | Title collision (pitfall 7) | Retitle the task |
| Sweep stopped with `abort_unconfirmed` | A run ignored abort, usually a hung check | `pgrep -fl bench_verify`; kill it; re-run the same command |
| Sweep stopped: "infrastructure unavailable" | Server or Ollama down past `--infra-wait-minutes` | Restart what's down; re-run the same command |
| Many `engine_*` rows after an engine change | Running `bastion` predates the change | Rebuild, restart, re-run with a new `--run-name` |
| Machine crawling, jobs timing out | Memory pressure | `sysctl vm.swapusage`; `ollama ps`; reboot before long sweeps |
| Log shows `relation "node_invocations" does not exist` | Local database missing that migration | Harmless to the bench |

## Code map

Everything is in [`scripts/bench_local_models.py`](../scripts/bench_local_models.py); tests in
[`scripts/tests/test_bench_local_models.py`](../scripts/tests/test_bench_local_models.py), run with
`python3 scripts/tests/test_bench_local_models.py` and gated as `bench-local-models-tests` in
`planning/harness.json`.

| Function | Job |
|---|---|
| `preflight` | Server, key, binaries, models and capabilities, context variants, both `origin/main` guards |
| `commit_subject_collisions` · `tasks_already_satisfied` · `missing_fixture_paths` | The three "results would be meaningless" guards |
| `ensure_ctx_variant` · `ctx_variant_name` | Create `<model>-ctx<N>` via `ollama create` |
| `build_harness_from_tasks` | Derive `harness.json` from `tasks.json` |
| `build_event_body` | The `SDLC_FLOW` event; the policy carries backend, model and review mode |
| `plan_jobs` · `completed_record` | Job order and resume |
| `run_one_job` | Dispatch, poll, abort, harvest, classify, capture, clean — never raises |
| `run_final_checks` · `changed_paths` · `classify` | Independent verdict on the final tree |
| `capture_evidence` · `render_reports` | `artifacts/` and `leaderboard.md` |

Engine code the bench depends on:

| Where | What |
|---|---|
| `crates/engine-core/src/workflows/sdlc_flow/task_loop.rs` | `ImplementTaskNode` backend prompts and `task_base_sha`; `TestTaskNode::verify_claimed_writes`; `task_diff_base` |
| `crates/engine-core/src/nodes/aider_transport.rs` | `AIDER_FILES_ENV`, the aider command line |
| `crates/engine-core/src/nodes/pi_transport.rs` | The pi command line |
| `crates/engine-core/src/workflows/sdlc_flow/graph.rs` | `registry_for_policy_with_cancellation` — local transports per stage |
| `crates/engine-core/src/workflows/sdlc_flow/setup.rs` | Worktrees from `origin/main`; resume by commit title |

## Engine defects still open

Filed as `carryover[]` in the private `planning/state.json`:

- `heavy-work-queue-park-resume-reports-every-task-failed` — pitfall 9
- `sdlc-task-command-checks-have-no-timeout` — pitfall 10
- `aider-mention-reflection-discards-pending-edit` — pitfall 11
- `orchestration-merge-step-pushes-main-directly` — pitfall 6's push bypasses the fleet push script
- `orchestration-dev-node-invocations-table-missing`

## See also

- [workflows/README.md](workflows/README.md) — `agent_backend`, and the local-backend prompt rules
- [workflows/orchestration.md](workflows/orchestration.md) — why ORCHESTRATION merges, pushes and closes
- [heavy-work-queue.md](heavy-work-queue.md) — the queue behind `test_dispatch: queue_park`
- [workflows/policy-and-profiles.md](workflows/policy-and-profiles.md) — the policy fields the bench sets
- `planning/local-model-bench/index.md` (private vault) — fixtures, checkers and result runs
