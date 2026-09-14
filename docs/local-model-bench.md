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
sed -n '/## Leaderboard/,/## Jobs/p' ../../planning/open-work/local-models/local-model-bench/results/overnight-1/leaderboard.md
```

**Always pass `--run-name`** for a sweep that may cross midnight. The default name is today's date,
so a restart after midnight would start a new run instead of resuming.

**Optional: watch system load during a long sweep.** `scripts/dev-tooling/system_monitor.py` samples CPU/
memory/swap every 60s, keeps a rolling average, and fires a `bastion notify send` Telegram alert on
a sustained problem (not per-sample noise) — useful for an unattended overnight run on a machine
also running several loaded local models. Run it alongside the sweep, in a separate terminal:

```bash
python3 scripts/dev-tooling/system_monitor.py   # logs to /tmp/system_monitor.jsonl; --help for thresholds
```

### What must exist first

| Need | Why | If it's missing |
|---|---|---|
| `bastion serve` on `127.0.0.1:4317`, built from current engine-rs | It executes the runs; engine fixes only take effect once installed | Rebuild: `cargo install --path ../bastion --force`, then restart (below) |
| `BASTION_ENGINE_API_KEY` in `scripts/.env` | Authenticates dispatches | Preflight reports the key as rejected |
| Ollama running, models pulled | The models under test | `ollama pull <model>` |
| `aider` and `pi` on `PATH` | The two backends | Preflight names the missing binary; or pass `--agent-backends aider` |
| **`pi` is a LOCALLY PATCHED binary, not stock `pi_agent_rust`** | Fixes pitfalls 17/18 below — stock `pi` silently produces wrong data for reasoning models and any multi-line file write | `pi --version` must report a commit hash of `5fb9ab54` or later (not a bare version tag like `0.5.1` with no commit). If it doesn't: rebuild from `core/pi_agent_rust` (`cargo build --release`) and `cp target/release/pi ~/.local/bin/pi`. **Never run `pi`'s own installer/self-update against this bench** — it will silently overwrite the patched binary with stock upstream and pitfalls 17/18 will reappear with no warning. Pre-patch backups: `~/.local/bin/pi.upstream-0.5.0-backup`, `~/.local/bin/pi.pre-quote-fix-backup` |
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

Fixtures live in the HQ vault, moved 2026-09-14 from this repo's own `planning/` to
`planning/open-work/local-models/local-model-bench/tiers/<tier>/tasks.json` (bare path — the
`agentic-portfolio` company-brain repo, not this public one; two levels up from `core/engine-rs/`).
A tier's expected files and checks are written once, there. A compat symlink at
`core/_planning/engine-rs/local-model-bench` (this repo's own vault entry) still points at that same
directory, which is why every checker command below still reads `planning/local-model-bench/...` —
that literal string resolves unchanged through a worktree's own `planning/` symlink.

| Tier | Task | Checked by | Tests |
|---|---|---|---|
| `easy` | Write `BENCH_MARKER.md` with one exact line; write `bench_greeting.py` with `greet()` | `grep -qx`, a Python one-liner | Following exact instructions |
| `edit` | Change one line of the existing `scripts/dev-tooling/bench_verify_medium.py` | `planning/local-model-bench/checkers/verify_edit_one_line.py` — exactly 1 line added, 1 removed vs `origin/main` | Surgical edits (aider's whole-file format often drops or rewrites lines) |
| `medium` | Run-length encode/decode in `rle.py` | [`scripts/dev-tooling/bench_verify_medium.py`](../scripts/dev-tooling/bench_verify_medium.py) | Real logic, exact outputs |
| `hard` | Arithmetic evaluator with precedence and parentheses, no `eval()` | [`scripts/dev-tooling/bench_verify_hard.py`](../scripts/dev-tooling/bench_verify_hard.py) | Parsing |
| `rust` | `top_words()` in a standalone `word_freq.rs` | `planning/local-model-bench/checkers/verify_rust_word_freq.sh` — 8 tests via `rustc --test`, no Cargo | Rust that compiles |

**Every check runs under `perl -e 'alarm shift; exec @ARGV' 60`** (macOS has no `timeout`), and the
Rust test binary under a 30 s alarm. Model-written code can loop forever.

**Standing convention: every SDLC_FLOW stage this bench dispatches must resolve to the local model
under test** — verify via a run's `run-event.json` → `task_context.metadata.claude_sessions[].model`;
none should show a cloud model id (`claude-sonnet-*`/`claude-opus-*`/`claude-haiku-*`). `triage` and
`review` alone are not enough — see pitfall 20 below, where `PatchDocsNode`/`GenerateTasksNode` kept
silently calling the real `claude` CLI because `build_event_body`'s `model_tiers` never named them.
`build_event_body` (`scripts/bench_local_models.py`) now sets `triage`/`review`/`docs`/`generate` all
to `"local"`; `implement` is the one deliberate exception (it routes through `agent_backend`, not a
model-tier swap). Adding a new SDLC_FLOW model stage in the future means adding its tier here too —
check `crates/engine-core/src/workflows/sdlc_flow/graph.rs`'s `registry_for_policy_with_cancellation`
for the full list of stages a `Local` tier can route.

**Changing a tier:**
- Keep each task's `title` unique — see pitfall 7.
- A repo path a check reads must already be on `origin/main`. A new checker goes in the vault's
  `checkers/`, reached through the worktree's `planning/` symlink, which also keeps it out of aider's
  repo map.
- Prove every new checker both ways: a correct solution passes, and a wrong or missing one fails.

## Reading the results

Everything for a run lands in the HQ vault under
`planning/open-work/local-models/local-model-bench/results/<run-name>/` (bare `planning/local-model-bench/results/<run-name>/`
still works too, via the compat symlink noted above):

| File | What it is |
|---|---|
| `leaderboard.md` | Pass counts per model × backend × tier, median passing time, main failure categories, engine anomalies, and one row per job |
| `summary.json` | Every job record in one file |
| `<tier>/<backend>/<model>-r<rep>.json` | One job's record. Its presence is what makes a re-run skip that job |
| `artifacts/<job>/` | Evidence: the SDLC state file, the run event, aider's chat history (`.txt`), `git.txt` (log + diff vs `origin/main`), final check output |
| `../findings-log.md` (one directory up, shared across every run) | **Read this before trusting any leaderboard.** Append-only record of every bug found while running the bench — symptom, root cause, fix, and which `run-name`/jobs it affects. `leaderboard.md` is regenerated wholesale on every run and carries no history of its own |

**If a core engine/tool bug is found and fixed mid-sweep, retire that `run-name` as historical
evidence (see `../index.md`'s `results/overnight-1/` row for the pattern) and start a fresh
`--run-name` for the next real comparison — don't try to hand-filter a mixed leaderboard.**

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
| `--models` | required | Comma-separated, or `all` (every pulled model, smallest first, base variants only; `-ctxN` variants skipped -- see `--require-capability` for capability filtering) |
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
| `--require-capability` | `auto` | Ollama capability floor, queried live per model via `POST /api/show` (never a hardcoded name list). `auto` resolves to `tools` whenever `--agent-backends` includes `aider`/`pi` (both always send tool definitions; Ollama hard-rejects a tools-incapable model with HTTP 400 before generation starts), else `completion`. `tools`/`completion` force the floor explicitly |
| `--dry-run` | off | Preflight and plan only. Reports, but does not create, context variants |

Every preflight (dry-run or real) regenerates
`planning/open-work/local-models/local-model-bench/model-capabilities.md` and `.json` — every resolved model's Ollama capability set, which models were excluded and why,
distinct from other preflight warnings.

### The `pi` backend's own hardening (`PiConfig`)

Not a bench flag — a `SdlcTaskPolicy`/`SdlcPolicy` knob the engine resolves for every `pi`-backend
call, set in the bench's own event body via `build_event_body`'s default profile. `docs/workflows/README.md`
has the full table; in short: `no_context_files`/`no_session` default `true` (confirmed real ambient
`AGENTS.md`/`CLAUDE.md` leakage into every local-model prompt otherwise, and a zero-downside hygiene
default), `tools` scopes `pi`'s own 19-tool default down to 12 for a headless coding task.

## Running jobs concurrently, and exercising ORCHESTRATION (2026-09-14)

Two additive modes, both default-off so existing invocations are unchanged:

**`--parallel N`** (default `1`) — runs N worker slots concurrently against `--dispatch direct`
(the default dispatch mode, unchanged otherwise). Each slot gets its own `--spec-slug` suffix
(`-slot1`, `-slot2`, ...) and therefore its own worktree, so concurrent jobs never stomp each
other's `tasks.json`/`harness.json`. Verified 2026-09-14: two jobs dispatched at the same second
against the real dev `bastion serve` (port 4317), confirmed running concurrently via overlapping
`status: running` polls, completing independently (one failed fast, the other passed ~105s later).
Safe at any `--parallel` value — direct dispatch carries no `block_id`, so it has no merge/push/
close side effects to race on.

**`--dispatch orchestration`** — fires the real `ORCHESTRATION` workflow (not a direct
`SDLC_FLOW`/`SDLC_TASK` POST) against a disposable, repeatedly-reopened block
(`EN.ticket.local-model-bench-orchestration-slot`), so a bench run also exercises real chain
mechanics (`SetupWorktreeNode` → `SpecExistsRouterNode` → the full `SDLC_TASK` graph → close).
**Sandbox-only, hard-guarded** (`assert_sandbox_target`): refuses to run without an explicit
`--sandbox-root`, refuses a root that is or contains the real HQ vault
(`/Users/brandon/Dev/agentic-portfolio`), and refuses `BASTION_SERVE_ADDR` port `4317` (the real
dev instance). This is not optional hardening — `docs/workflows/orchestration.md`'s own "Pitfall"
section documents that a passing orchestration step merges its branch into `main`, pushes it, and
closes the block (a fleet-wide `mev emit-state --write`); pointed at the real HQ vault, a bench
sweep would rewrite real state and push real branches. **Sequential only** — `--parallel > 1` is
rejected for this mode, because that merge-and-push happens in the repo's *primary* checkout, which
races across concurrent dispatches even for different blocks.

**Known gap, not fixed by this mode: not zero-cloud-cost.** A PASSING orchestration job still
triggers `ORCHESTRATION`'s own ledger-composer step
(`crates/engine-serve/src/journal.rs::compose_ledger_entries_via_agent`), which has **no local
transport wired at all** — confirmed in source: `"Local` has no meaning for this composer (no
OpenAI-compatible transport is wired here) ... nothing sets this knob to `local` today."` Every
passing job makes one real sonnet-tier Claude call regardless of how local the child run's own
tiers are. A **failing** job never reaches that step (`CloseBlockNode` only runs on a real close),
so a task-failed/timeout job is genuinely free. Verified 2026-09-14: a real end-to-end dispatch
(real `qwen2.5-coder:7b` + `aider` against a sandbox instance) ran the full chain in 180s, made real
file changes, and failed at the engine's own final-check classification
(`engine_rejected_correct_work` — a real bench finding, not a plumbing bug) *before* reaching the
composer, so that specific verification run made zero Claude calls. A pass-through run would not be
free. Fixing the composer's local routing is out of scope here — same shape as the `llm_node.rs`
trait work elsewhere in this repo, but its own task.

**Setup, once per sandbox instance:** `ensure_sandbox_bench_block` idempotently creates the
disposable block and a `planning/roadmaps/local-model-bench/` directory (an explicit `blocks` list
still needs a `roadmap_slug` to resolve a lane-log directory) on first use — nothing to do by hand.

Auto-monitor (`system_monitor.py`, both modes): launched automatically alongside any run, logged to
`<run>/system_monitor.jsonl`, terminated on completion or interrupt. `--no-monitor` opts out.

```bash
# Real model parallelism against the real dev bastion serve (safe, default target)
python3 scripts/bench_local_models.py --parallel 3 --models all --tiers easy --agent-backends aider

# Exercise ORCHESTRATION for real, sandbox only
export BASTION_SERVE_ADDR=http://localhost:18090   # the sandbox's own engine port
python3 scripts/bench_local_models.py --dispatch orchestration \
  --sandbox-root /Users/brandon/Dev/engine-rs-sandbox-engrs1 \
  --models qwen2.5-coder:7b --tiers easy --agent-backends aider
```

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
| 9 | Every task failed with no feedback | `test_dispatch: queue_park` resumes checks with `passed: null` → failed, `check_results: []` | **Fixed** (engine-rs `ed5050f`, 2026-09-14): `HeavyWorkQueue` now persists the real outcome + check results. Bench still defaults to `--test-dispatch inline` regardless — no reason to switch |
| 10 | A check ran 7+ min, grew to 2.9 GB, filled swap; the bench was OOM-killed and abort could not land | A model's evaluator looped; engine checks have no timeout, and cancellation waits for the node boundary | Bench: 60 s alarm on every check. Engine defect still open |
| 11 | Aider discarded a correct edit, then edited `.gitignore` | A reply naming another tracked file makes aider add it and re-prompt **before** applying the edit | Bench: `edit` tier targets a file that names no other tracked file. Engine defect still open |
| 12 | `review_mode: end_only` failed with HTTP 404 | `EndReviewNode` ignored the local review tier | Engine: wired to the local transport |
| 13 | A manual Pi run hung for 7 min with no output | Pi was waiting on an open stdin | The engine already nulls stdin; only affects hand-run `pi` |
| 14 | A checker failed correct work | `py_compile` with `cfile=/dev/null` raises on every file | Fixed in the checker; caught by testing it against a correct edit first |
| 15 | Verifiably correct runs (checks 2/2, real commits) reported as a hard crash under `review_mode: end_only` | `EndReviewNode`'s strict JSON parse rejected `llama3.1:8b`/`llama3.2:3b`/`phi3.5:3.8b`'s prose-wrapped verdicts, and the parse failure returned a fatal `NodeError` instead of a labeled outcome | Engine: `parse_structured_or_fenced` (`workflows/mod.rs`) gained a balanced-JSON-extraction fallback for prose-wrapped replies; a still-unparseable reply now stamps a distinct `UNPARSEABLE` verdict (`EndReviewNode`, `end_review.rs`) that routes to `WrapUpNode` as a legible `blocked` run instead of crashing the whole run |
| 16 | `--models all` dispatched `phi3.5:3.8b`/`codestral:22b` (and their `-ctxN` variants) through aider/pi, which sent tool definitions on every call; Ollama hard-rejected with HTTP 400 before generation started -- instant, uninformative failures with nothing to do with model quality | Model selection checked only `completion` capability, never `tools` | Bench: `--require-capability` (default `auto` = `tools` for aider/pi), queried per model via `POST /api/show`. `/api/tags`'s own `capabilities` array is **not** reliable for this: it reports `deepseek-r1:14b`/`32b` as `[completion, thinking]` (no `tools`), while `/api/show` for the same model returns `[tools, thinking, completion]` -- confirmed 2026-09-14 |
| 17 | Every `pi`-backend job against a reasoning model (`deepseek-r1:*`) reported `no_change`/zero tokens near-instantly, though the model was actually generating real output | `pi_agent_rust`'s `OpenAIDelta` only recognized DeepSeek-official/OpenRouter's `reasoning_content` field name; Ollama's OpenAI-compat endpoint names the same field `reasoning`. Serde silently dropped every reasoning-phase delta | **Fixed locally**, not upstream: `core/pi_agent_rust` commit `87d475e5` adds a serde alias. Patched binary is now the installed `~/.local/bin/pi` (see the callout below — **this is not a stock `pi` install**) |
| 18 | Every Ollama-streamed `pi` tool call whose content contained a literal newline (almost any multi-line file write) wrote a literal two-character `\n` to disk instead of a real newline byte | Ollama's `/v1/chat/completions` endpoint double-escapes `\n`/`\t`/`\r` inside a **streamed** tool call's `arguments` JSON (confirmed by curling Ollama directly with `stream:false` vs `stream:true` against the same generation — an Ollama server bug, not a `pi_agent_rust` decoding bug) | **Fixed locally**: `core/pi_agent_rust` commit `5fb9ab54` repairs the doubled escape before the final JSON parse, scoped to `provider == "ollama"`. Same patched binary as #17 |
| 19 | (Not a bug, but looked like one at first) `llama3.1:8b` via `pi` wrote `return \"Hello, \\" + name + !\"` instead of `return 'Hello, ' + name + '!'` | 8 identical curl calls to Ollama with the same prompt/model produced 8 *different* escaping mistakes — non-deterministic model output, not a repeatable transport defect (contrast with #18, which is 100% deterministic) | Not fixable; documented as real bench data (`llama3.1:8b` is weak at nested-JSON string escaping in tool-call arguments), not chased further. See `findings-log.md` |
| 20 | A bench run's `run-event.json` showed `PatchDocsNode` calling the real `claude` CLI with sonnet, not the local model under test | `registry_for_policy` only rewired `triage`/`review`/`implement` for the `local` tier; `PatchDocsNode` (docs.rs) and `GenerateTasksNode` (setup.rs) had no `with_meta_transport` builder at all, so `model_tiers.docs`/`model_tiers.generate: local` was a silent no-op | Engine: both nodes gained a `TransportSlot` field + `with_meta_transport`, wired in `graph.rs`'s `registry_for_policy_with_cancellation` (and `sdlc_task/graph.rs` for `generate`) exactly like `TriageTaskNode`. Bench: `build_event_body` now sets `docs`/`generate` to `"local"` too — see the standing convention above |

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
`planning/harness.json`. [`scripts/dev-tooling/system_monitor.py`](../scripts/dev-tooling/system_monitor.py) (optional,
separate process, see Quickstart) is standalone — no bench-script dependency either direction.

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
| `crates/engine-core/src/workflows/llm_node.rs` | The shared `TransportSlotted`/`Cancellable` traits + `resolve_meta_transport`/`wire` every stage above's local-tier rewire now goes through (`EN.ticket.transport-slot-consolidation`, 2026-09-14) — same observable behavior as before, different internal mechanism. See its own doc comment for exactly which nodes are migrated |

## Engine defects still open

Filed as `carryover[]` in the private `planning/state.json`. `heavy-work-queue-park-resume-reports-every-task-failed`
(pitfall 9) is **fixed** (`ed5050f`) and cleared from this list. Still open:

- `sdlc-task-command-checks-have-no-timeout` — pitfall 10
- `aider-mention-reflection-discards-pending-edit` — pitfall 11
- `orchestration-merge-step-pushes-main-directly` — pitfall 6's push bypasses the fleet push script
- `orchestration-dev-node-invocations-table-missing`

Two more are fixed **outside this repo**, in the local `core/pi_agent_rust` clone (not yet filed
upstream) — see pitfalls 17/18 above and the callout on the patched `pi` binary requirement.

## Opportunity not yet explored: ORCHESTRATION's preflight/inbox-triage stages

As of `65f102e` (2026-09-14), `OrchestrationPolicy.preflight_model_tier`/`inbox_triage_model_tier:
local` actually dispatch to a local model — before, the tier resolved through policy but no
production call site forwarded it, so `local` silently did nothing (see [orchestration.md
§ preflight_model_tier](orchestration.md), [policy-and-profiles.md](policy-and-profiles.md)). **This
bench script does not exercise either stage** — every job here dispatches `SDLC_FLOW` only, and
`ORCHESTRATION`'s preflight/inbox-triage runs are a separate workflow this bench has no job type
for. No result in this runbook's leaderboards says anything about local-model quality on either
stage; that would need a new job type dispatching `ORCHESTRATION`, not a config change to an
existing one. Filed as a real gap, not yet a backlog ticket.

## Opportunity not yet explored: completion-only models elsewhere in the engine

A model excluded here for lacking `tools` (currently `phi3.5:3.8b`, `codestral:22b`) is not
necessarily bad — it's unusable by aider/pi specifically, which always send tool definitions.
`content_pipeline`'s `SummarizeNode`/`SelfCriticNode`/`ReviseNode`/`TranslateNode` call a local
model for plain text generation via `parse_structured_or_fenced`, not a tool-calling loop, so a
completion-only model is architecturally usable there. **This is an unproven hypothesis, not a
result** — passing this bench's tool-driven coding tasks says nothing about summarization/critique/
translation quality, and no such evaluation has been run. Filed as a backlog idea (not committed
work) in the brain's `planning/backlog.md`,
`local-model-bench-completion-only-models-for-content-pipeline`. The current capability set for
every local model is `planning/open-work/local-models/local-model-bench/model-capabilities.md`,
regenerated on every preflight.

## See also

- [workflows/README.md](workflows/README.md) — `agent_backend`, and the local-backend prompt rules
- [workflows/orchestration.md](workflows/orchestration.md) — why ORCHESTRATION merges, pushes and closes
- [heavy-work-queue.md](heavy-work-queue.md) — the queue behind `test_dispatch: queue_park`
- [workflows/policy-and-profiles.md](workflows/policy-and-profiles.md) — the policy fields the bench sets
- `planning/open-work/local-models/local-model-bench/index.md` (HQ vault, moved 2026-09-14) —
  fixtures, checkers and result runs
