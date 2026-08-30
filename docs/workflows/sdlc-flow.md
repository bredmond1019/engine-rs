---
type: Reference
title: SDLC Flow Workflow
description: How the SDLC_FLOW workflow graph works — node roles, the run's commit topology and working-tree review diffs, triggering from engine-rs and bastion, stopping a run, reading outputs, and inspecting/resuming state
doc_id: sdlc-flow-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc-flow, workflow, graph, nodes, commit-topology, review-diff, resume, state, bastion]
related: [sdlc-flow-policy, architecture, cli, data-contract]
---

# SDLC Flow Workflow

`SDLC_FLOW` is the workflow graph that drives a spec through
implement → test → triage → review → docs → wrap-up → PR, one task at a time, with durable
state written after every step. This doc covers the graph shape, how to trigger/stop/resume a
run, and where to look for output. For the tunable cost/quality knobs a run can carry (model
tiers, review strictness, verbosity, local-model tier) and the telemetry it records, see
[sdlc-flow-policy.md](sdlc-flow-policy.md) — this doc only calls out *which* nodes read those
knobs, not what the knobs do.

Source: `crates/engine-core/src/workflows/sdlc_flow/` (`graph.rs`, `setup.rs`, `task_loop.rs`,
`docs.rs`, `wrap_up.rs`, `pr.rs`, `emit_state.rs`, `schema.rs`), `crates/engine-serve/src/`
(`workflows.rs`, `http.rs`, `abort.rs`, `dispatch.rs`).

## Graph shape

```
SetupWorktreeNode -> SpecExistsRouterNode -> { GenerateTasksNode -> LoadTaskStateNode | LoadTaskStateNode }
  -> TaskQueueRouterNode -> { ImplementTaskNode -> TestTaskNode -> TriageTaskNode
                                -> TriageRouterNode -> ConsolidatedReviewNode
                                -> ReviewRouterNode -> UpdateTaskStatusNode
                                -> SaveStateNode -> (loop) TaskQueueRouterNode
                            | FinalValidationNode -> PatchDocsNode -> WrapUpNode -> CloseBlockNode
                                -> PullRequestNode -> EmitStateNode }

TriageRouterNode     -> { ConsolidatedReviewNode | IncrementAttemptNode | WrapUpNode }
ReviewRouterNode      -> { UpdateTaskStatusNode | IncrementAttemptNode | WrapUpNode }
IncrementAttemptNode -> ImplementTaskNode
```

The retry/bail back-edges (`IncrementAttemptNode -> ImplementTaskNode`, both routers' bail
branches into `WrapUpNode`, and `SaveStateNode`'s loop-closing hop back to `TaskQueueRouterNode`)
are runtime-only — declared but not walked by the graph's acyclic-shape validator (decision D42,
"declared-acyclic / runtime-cyclic").

## Nodes: model vs. deterministic, and what each does

| Node | Kind | What it does |
|---|---|---|
| `SetupWorktreeNode` | Deterministic | Creates/reattaches the spec's git worktree (`git worktree add`, or reattach if `resume` and it already exists on disk). Resolves the 4-layer `SdlcPolicy` (event `policy` > event `profile` > `harness.json` > built-in default) and stamps it into ctx as `ResolvedPolicy` — see [sdlc-flow-policy.md](sdlc-flow-policy.md). |
| `SpecExistsRouterNode` | Deterministic router | Routes to `LoadTaskStateNode` if `sdlc-flow-state.json` or `tasks.json` already exists under `planning/<slug>/`, else to `GenerateTasksNode`. |
| `GenerateTasksNode` | **Model** (Opus, tunable via policy) | Planning-fallback path only — gathers `planning/<slug>/*.md` context and prompts for a task list, writing `tasks.json` + `tasks.md`. Sets `config.json_schema` and prefers the model's structured output (`ctx.nodes["GenerateTasksNode"]["structured"]`) over fence-stripped text parsing, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Reads `model_tiers.generate` and `timeouts.generate`; Opus is the built-in default for `model_tiers.generate`, chosen to be byte-identical to the removed hardcode. |
| `LoadTaskStateNode` | Deterministic | Honours `resume`: with `resume: true` loads `sdlc-flow-state.json` if present, otherwise archives it to `.superseded-<run_id>.bak` and bootstraps a fresh `SDLCState` from `tasks.json`. Applies the event's `task_range` filter. See [Restart vs. resume](#restart-vs-resume--what-resume-actually-does). |
| `TaskQueueRouterNode` | Deterministic router | Finds the first `PENDING` task and routes to `ImplementTaskNode`; if none remain, routes to `FinalValidationNode` (task loop exit — the drain branch). |
| `ImplementTaskNode` | **Model** (Sonnet, tunable via policy) | Drives the model to implement the current task; parses `{summary, modified_files, tests_added}`, preferring the model's structured output (`ctx.nodes["ImplementTaskNode"]["structured"]`) over fence-stripped text parsing and falling back to `strip_json_fence` + `serde_json::from_str` (or a synthesized `ImplementOutput` on parse error) when structured output is absent. Reads policy for model tier, prompt-cache anchor, and verbosity directive — never rewired to the `local` tier regardless of policy. |
| `TestTaskNode` | Deterministic | Runs the worktree's `planning/harness.json` validation-suite `checks`. Only the `command` check kind is fully supported today — other declared kinds fail closed with a "not yet supported" message rather than silently passing. Before the suite it runs the **write-verification guard**: `git status --porcelain` in the worktree, unconditionally, failing the task with a `write-verification` `CheckResult` (routed through the normal triage/retry path) when NOTHING changed. It never compares the changed paths against `ImplementTaskNode`'s `modified_files` — that self-report is unreliable in both directions, so only the worktree is evidence. Harness checks pass happily on an untouched checkout, so this guard is the only thing that distinguishes "task done" from "task never ran". A task opts out by declaring `"expects_writes": false` in `tasks.json` (investigation-only work); the field is absent-defaults-to-`true`, so forgetting it is safe. |
| `TriageTaskNode` | Deterministic, **conditionally model** | Classifies the test result as `PASS` / `RETRYABLE` / `MAJOR_BAIL`. Deterministic by default; only calls the model (Sonnet, tunable) when `event.llm_triage == true` **and** the task is failing-but-under-budget — that LLM branch sets `config.json_schema` and prefers the model's structured output over fence-stripped text parsing, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. That LLM prompt carries the **failed checks' captured output** (name + message + `check_results[].output`) under a classifier-facing header, appended to the per-run prompt body and bounded by the shared `retry_feedback.max_chars` knob (`retry_feedback.enabled: false` suppresses it) — without it the classifier saw only `failure_summary`'s check *names* and judged nearly blind. No review-verdict fallback: unlike `ImplementTaskNode`'s retry feedback, triage renders test failures only. Also reads policy on the `PASS` branch for deterministic trivial-diff classification (`review_skip_max_files`/`review_skip_max_diff_lines`), independent of `llm_triage`. |
| `TriageRouterNode` | Deterministic router | On `TriageTaskNode`'s verdict: `PASS` → `ConsolidatedReviewNode` or `UpdateTaskStatusNode` (per policy's `review_mode`); `RETRYABLE` → `IncrementAttemptNode`; `MAJOR_BAIL` → `WrapUpNode`; any other/unrecognized verdict string → `WrapUpNode` (fallback, not `None`) with the offending string stamped as `unrecognized_verdict` on `TriageTaskNode`'s result. |
| `ConsolidatedReviewNode` | **Model** (Sonnet, tunable via policy) | Reviews the task's **working-tree diff against `HEAD`** (`git add -N -A`, then `git diff HEAD` — see [Commit topology and what the reviewer sees](#commit-topology-and-what-the-reviewer-sees)) against acceptance criteria; parses `{verdict, summary, issues}`, preferring the model's structured output (`ctx.nodes["ConsolidatedReviewNode"]["structured"]`) over fence-stripped text parsing and falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Reads policy for model tier/prompt-cache/verbosity; can be rewired to the `local` tier. |
| `ReviewRouterNode` | Deterministic router | On review verdict: `PASS` → `UpdateTaskStatusNode`; minor `FAIL`/`PARTIAL` (1–5 issues) → `IncrementAttemptNode`; structural `FAIL`/`PARTIAL` (0 or >5 issues) → `WrapUpNode` (bail); any other/unrecognized verdict string → `WrapUpNode` (fallback, not `None`) with the offending string stamped as `unrecognized_verdict` on `ConsolidatedReviewNode`'s result. |
| `IncrementAttemptNode` | Deterministic | Shared retry back-edge target for `TriageRouterNode`'s `RETRYABLE` and `ReviewRouterNode`'s minor fail. Bumps `task.attempt_count` + telemetry, forwards to `ImplementTaskNode`. |
| `UpdateTaskStatusNode` | Deterministic | Mutates the durable state: sets the task's status to `Done`/`Failed`, bumps telemetry counters. |
| `SaveStateNode` | Deterministic | Serializes the latest `SDLCState` to `planning/<slug>/sdlc-flow-state.json` inside the worktree, then `git add -A` + `git commit -m "feat(sdlc): <task_id> — <title>"` — staging the task's **code**, not just the state file (which is gitignored in this repo — see the caveat under [Commit topology](#commit-topology-and-what-the-reviewer-sees)). Runs once per completed task-loop iteration (pass path only), then loops back to `TaskQueueRouterNode`. |
| `FinalValidationNode` | Deterministic | The run-level authoritative gate (`EN.3.E`). Runs the worktree's `planning/harness.json` `validation.checks[]` at `TestDepth::Full` with NO `perTask` filter (`apply_per_task_filter = false`) and an empty `task_validation_commands` slice — so `cargo build --release` (marked `"perTask": false`) runs here even though `TestTaskNode` skips it. Sits on the task-loop drain branch only (`TaskQueueRouterNode`'s "no pending" edge), so it runs exactly once per run, never per task; the `ImplementTaskNode` branch and both routers' bail edges into `WrapUpNode` do not pass through it. It is unconditional and **not** policy-gated — no `SdlcPolicy` field, profile entry, or `harness.json` key changes whether it runs or at what depth (see [D12](../../planning/decisions/D12-per-task-vs-final-check-depth.md) and the [two-check-site model](#the-two-check-site-model-per-task-tripwire-vs-final-gate) below). A failing gate does **not** halt the walk — it stamps `{all_passed, check_results, failure_summary}` (the same shape `TestTaskNode` emits) and returns `Ok`, so the run still reaches `EmitStateNode` and `WrapUpNode` reports a degraded terminal status rather than bailing. |
| `PatchDocsNode` | **Model** (Sonnet, tunable via policy) | Reads the most recent `ImplementTaskNode`'s `modified_files`, asks the model to find and patch stale `docs/` references (the model edits files itself via its own tool use — this node doesn't). Sets `config.json_schema` and prefers the model's structured output (`ctx.nodes["PatchDocsNode"]["structured"]`) over fence-stripped text parsing for its own `{summary, files_patched}` output, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Reads `model_tiers.docs` and `timeouts.docs`; Sonnet is the built-in default for `model_tiers.docs`, chosen to be byte-identical to the removed hardcode. |
| `WrapUpNode` | Deterministic | Computes the PASS/PARTIAL-FAIL outcome, renders `log_entry`/`report`/`status_suggestion` text, stamps `policy` + `RunOutcomes` telemetry into `SDLCState`, and persists that stamped state to `sdlc-flow-state.json` — the only point the `policy`/`outcomes` blocks reach disk, since `SaveStateNode` never runs again after the loop exits. Also `git add -A` + `git commit -m "chore(sdlc): wrap-up — docs patch + terminal state"` (via the same shared `commit_all` helper `SaveStateNode` uses), so a `--worktree` run's PR contains its own terminal state (EN.3.G task 3) **and** `PatchDocsNode`'s doc edits, which nothing else commits. Terminal target for both routers' bail branches, and the natural end of a fully-passing run. |
| `PullRequestNode` | Deterministic | Pushes the branch and opens a PR via `gh pr create` — never auto-merges (human review gate, decision D25). No-op (`{pr_url: null, skipped: true, branch_name}`) when `event.auto_pr == false` (default `true`); `branch_name` is stamped on both the success and no-op shapes (falling back to `null` on the no-op path if `SetupWorktreeNode` hasn't run) so a chain's integrate stage (`EN.11.C`, `docs/orchestration-workflow.md`) can always resolve which branch this run actually pushed, even when `auto_pr` was `false`. |
| `EmitStateNode` | Deterministic | Runs `mev emit-state --write` in the worktree to refresh the brain freshness spine. Also patches the committed `sdlc-flow-state.json`'s `pr` block (`url`/`number`, parsed from `PullRequestNode`'s PR URL) in place and re-commits it via `commit_all`, since `PullRequestNode` runs after `WrapUpNode` and so cannot have populated that block itself (EN.3.G task 6). Infallible best-effort: any failure mode (skipped PR, no worktree, missing/unparsable state file) is a silent no-op. Terminal node. |

**Nodes that read the tunable policy:** `SetupWorktreeNode` (resolves it, but idempotently as of
`EN.5.D` — a served run's dispatch factory, `engine-serve::register_sdlc_flow`, already resolves
policy via `resolve_policy_for_run_from(&event.data, &PolicyConfigSource::Worktree(cwd))` and
seeds `RESOLVED_POLICY_IDENTITY` before this node runs, so it only resolves + stamps when no
policy was seeded yet — in-tree/CLI-driven runs and unit tests driving this node directly),
`ImplementTaskNode`,
`TriageTaskNode`, `ConsolidatedReviewNode`, `TriageRouterNode`, `WrapUpNode`, `GenerateTasksNode`
(`model_tiers.generate`, `timeouts.generate`), `PatchDocsNode` (`model_tiers.docs`,
`timeouts.docs`) — plus `registry_for_policy` in `graph.rs`, which isn't a node but decides
whether `TriageTaskNode`/`ConsolidatedReviewNode` get rewired to the local-model transport. Every
other node makes no model call at all. Full knob-by-knob reference:
[sdlc-flow-policy.md](sdlc-flow-policy.md).

### Commit topology and what the reviewer sees

**The invariant:** `HEAD` carries every previously completed task's code; the working tree's delta
against `HEAD` is exactly the current task's in-progress work.

That falls out of where the commits are:

| Commit | Made by | Message | Contents |
|---|---|---|---|
| One per **passed task** | `SaveStateNode` | `feat(sdlc): <task_id> — <title>` | that task's code (plus `sdlc-flow-state.json` where `planning/` is tracked — see the caveat below) |
| One per **run**, at the end | `WrapUpNode` | `chore(sdlc): wrap-up — docs patch + terminal state` | `PatchDocsNode`'s doc edits (plus the terminal state, same caveat) |

> **Caveat — the state file is NOT committed in engine-rs.** `planning/` here is a gitignored
> symlink into the brain vault (`.gitignore` `/planning`), so `git add -A` cannot stage
> `planning/<slug>/sdlc/sdlc-flow-state.json` and these commits carry code only. That was equally
> true before the staging widened (`git add <ignored-path>` also no-opped), and it is why
> `log_noop_commit`'s quiet path exists at all. In a repo that tracks `planning/`, the state file
> does ride along. Do not read EN.3.G's "a `--worktree` run's PR contains its own terminal state"
> as satisfied in this repo.

`SaveStateNode` sits only on the pass path (`UpdateTaskStatusNode → SaveStateNode`). The retry path
(`TriageRouterNode`/`ReviewRouterNode → IncrementAttemptNode → ImplementTaskNode`) never reaches it,
so **a failed attempt commits nothing** — its edits stay in the working tree and the next attempt's
review sees the cumulative current attempt. Nothing leaks into the *next* task, because the previous
task's commit moved `HEAD` past it.

The wrap-up commit runs before `PullRequestNode`'s `git push` (drain order is
`FinalValidationNode → PatchDocsNode → WrapUpNode → PullRequestNode`), which is what puts the docs
patch on the pushed branch — `docs.rs` makes no git calls of its own.

**On a `MAJOR_BAIL`**, the bailed task never reaches `SaveStateNode`, so its partial, unvalidated
work is swept into the wrap-up commit. This is deliberate: visible attempted work on a draft-PR
handoff beats silently discarding it.

> **Blast radius — `use_worktree` defaults to `false`, and the dirty-tree guard is what contains
> it.** `add -A` and `add -N -A` are TREE-WIDE, and on the default event path
> (`use_worktree: false`) `SetupWorktreeNode` checks the run's branch out in a **live checkout**
> (`worktree_path == "."`, or the registry-resolved repo root), not under `trees/<branch>`. Left
> unguarded, a run started there would sweep any unrelated dirty file in that checkout into its
> commits, and the intent-to-add pass would write to that repository's real index.
>
> `SetupWorktreeNode` therefore runs `git status --porcelain` **before** creating the branch
> whenever `use_worktree` is false, and aborts the run when the tree is not clean, naming the
> dirty paths:
>
> ```
> Working tree is not clean — commit or stash your changes, then re-run (or set
> use_worktree: true for an isolated checkout). Dirty paths:
>  M src/lib.rs
> ?? scratch/notes.txt
> ```
>
> This mirrors `.claude/workflows/sdlc-flow.js`'s branch-mode guard (setup STEP 3a), so both
> engines behave the same way. A **clean** live-repo run is unchanged. A `use_worktree: true` run
> is already isolated and is deliberately not guarded — the guard command is not even issued. The
> abort is a safety invariant, not a policy knob: there is no cost/latency/quality tradeoff a run
> could reasonably want the other way, and `use_worktree: true` is the supported escape hatch.
> **Prefer `use_worktree: true`** for any run you do not want touching the ambient tree at all.

**What the reviewer and the trivial-skip classifier read.** Both take the working tree against
`HEAD` — `git add -N -A` (intent-to-add) followed by `git diff HEAD` for `ConsolidatedReviewNode`
and `git diff --numstat HEAD` for `classify_trivial`. The intent-to-add pass is what makes
brand-new untracked files appear in the diff with their full content (as `+` lines) and in the
numstat with their real line counts; without it git would ignore them entirely, and implementer
agents routinely create new files. A binary untracked file numstats as `-\t-\t<path>` and hits the
classifier's existing conservative non-trivial arm.

**The reviewer's diff is bounded.** Because that diff is real, reviewer prompt size — and therefore
context consumption and cost — scales with it. `policy.review_diff_max_chars` (default 120 000
characters) is the ceiling; an over-budget diff is **clipped, not dropped**, and the clip is
announced to the model in the prompt body so it cannot confidently approve code it never saw. See
[`review_diff_max_chars`](sdlc-flow-policy.md#available-knobs).

> **Why this is not a commit range.** These diffs used to be taken over `<base_sha>..HEAD`, where
> `base_sha` is the SHA `SetupWorktreeNode` stamps at setup time. Since nothing in a run ever
> committed the implementer's code, that range was empty on **every** run: the reviewer was shown
> an empty diff (so every `PASS` verdict was a rubber stamp), `classify_trivial` always counted 0
> files / 0 lines (so `ReviewMode::TrivialSkip` skipped the review unconditionally), and an
> `auto_pr` run pushed a branch whose only commits were state files. `SetupWorktreeNode` still
> stamps `base_sha` as run metadata, but nothing derives a diff from it.

### The two-check-site model: per-task tripwire vs. final gate

`SDLC_FLOW` runs the `planning/harness.json` validation suite at two different sites, mirroring
the shape `.claude/workflows/sdlc-flow.js` has always had (its fast per-task tripwire vs. its full
end-review suite):

- **`TestTaskNode`** — the per-task tripwire, run on every task attempt. Honors `test_depth`
  (`fastCommand` substitution, `perTask: false` exclusion at both depths) so the implement-fix loop
  stays cheap — see [Per-task check selection (`test_depth`)](sdlc-flow-policy.md#per-task-check-selection-test_depth).
- **`FinalValidationNode`** (`EN.3.E`) — the run-level authoritative gate, run exactly once on the
  task-loop drain branch. Always `TestDepth::Full`, no `perTask` filter — `cargo build --release`
  runs here even though the tripwire skips it. Not a knob: `flow.testDepth` from `harness.json`
  governs `TestTaskNode` only and is deliberately never read here.

Without `FinalValidationNode`, EN.3.D's cheap tripwire would mean the authoritative
`cargo nextest run --workspace` and `cargo build --release` never ran at all in a Rust
`SDLC_FLOW` run — trading a cost bug for a correctness bug. See
[D12](../../planning/decisions/D12-per-task-vs-final-check-depth.md) for the full rationale.

## How to trigger a run

`engine-rs` has **no standalone CLI** — it's a library/runtime that embeds in the `bastion serve`
daemon (`docs/cli.md`). The only way to start a run is the HTTP surface registered by
`crates/engine-serve`:

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "SDLC_FLOW",
  "data": {
    "spec_slug": "EN.3.C-tunable-run-policy-telemetry",
    "resume": false,
    "auto_pr": true,
    "task_range": "1-3",
    "llm_triage": false,
    "profile": "cheap-fast"
  }
}
```

`profile` names a built-in or `harness.json`-defined policy-profile bundle (see
[sdlc-flow-policy.md](sdlc-flow-policy.md)); it's mutually layered with the inline `policy`
override shown above — an event can carry either, both, or neither. See the 4-layer precedence
in the event-fields list below.

- 401 without a valid `X-API-Key`; 422 if `workflow_type` isn't registered.
- On success, mints a `run_id` (UUID) and starts the workflow; that `run_id` is what you abort
  with (below) and what live-state reads are keyed by.
- `GET /workflows` lists registered types; `GET /workflows/SDLC_FLOW/graph` returns the declared
  schema/graph shape; `GET /health` is a plain liveness check.

**From bastion:** `engine-serve`'s route table is embedded at `bastion serve`'s server root
(decision D48), mounted only when both `DATABASE_URL` and `BASTION_ENGINE_API_KEY` are set at
boot — otherwise the engine routes are simply left unmounted (logged, not a boot failure). The
engine routes use their own `X-API-Key` auth, entirely separate from bastion's own
`Authorization: Bearer <BASTION_SERVE_TOKEN>` scheme for `/api/*`/`/ws`. As of this writing,
`bastion`'s own CLI (`bastion run <workflow>`) dispatches through the Python orchestrator's
generic endpoint instead, not through this `/events/` route — triggering `SDLC_FLOW` specifically
means POSTing to the mounted engine route directly (e.g. from BastionUI or `curl`), not via a
`bastion` subcommand. `bastion abort` is the one first-class CLI wrapper around this workflow's
engine-serve surface (see below).

## How to stop a running workflow

Cancellation is cooperative, not a forced kill:

```
POST /events/{run_id}/abort
X-API-Key: <BASTION_ENGINE_API_KEY>
```

- 401 without a valid `X-API-Key`; 404 if `run_id` is unknown or the run already finished.
- 202 on success — this flips that run's `CancellationToken`.

What happens after that: `Workflow::run_with` checks the token **at every node boundary**, before
dispatching the next node. On a positive check it stamps `metadata.cancellation`, emits a final
progress snapshot, and returns — any node not yet reached stays `Pending`. A model call in
`ClaudeCodeStep` (used by `ImplementTaskNode`/`TriageTaskNode`/`ConsolidatedReviewNode`/
`PatchDocsNode`/`GenerateTasksNode`) races the cancellation token against its own transport call,
so an in-flight model call is dropped promptly rather than waiting for it to finish. A node
without cancellation wiring (e.g. `TestTaskNode`'s subprocess loop, `SetupWorktreeNode`'s `git
worktree add`) runs to completion before the next boundary check gets a chance to stop the walk —
so an abort against a run mid-`TestTaskNode` still lets that test suite finish before halting.

From bastion, `bastion abort <run> [--yes]` is the human-facing wrapper around this exact
endpoint — it prompts for confirmation and reports 202/404/401/connection-failure distinctly.

## Reading outputs

- **`planning/<spec_slug>/sdlc-flow-state.json`** — the ground-truth progress/outcome file.
  Contains every task's `status`/`attempt_count`, the run's cumulative `telemetry`, and — once
  the run reaches `WrapUpNode` — the `policy` + `outcomes` (`RunOutcomes`) snapshot. This is the
  first place to check "what happened" for any run, live or finished.
  - **`run_id`** (top-level key, EN.6.J) — the engine's `events.id` run UUID that produced this
    file, stamped into `ctx.metadata` via `RunOptions::run_id` at dispatch and read back by both
    `SaveStateNode` (per-task saves) and `WrapUpNode` (the run tail) so the file can be joined to
    its engine run. `null` for any state written by base-template's JS `sdlc-flow.js` engine,
    which never sets it, and for any older file predating this field — both parse cleanly via
    `SDLCState::from_committed_state_json`, which tolerates an absent key as well as an explicit
    `null`.
  - **`final_validation`** (top-level key, `EN.3.E`) — the `FinalValidationNode` result, folded in
    by `WrapUpNode` at the same point it stamps `policy`/`outcomes`: `{all_passed, check_results,
    failure_summary}`, the same shape `TestTaskNode` stamps per task. `null` for any run that
    didn't reach `WrapUpNode` (a bailed run) and for any state written by base-template's JS
    `sdlc-flow.js` engine, which never sets it — additive, same precedent as `run_id`/`telemetry`/
    `policy`/`outcomes`, so an older or JS-engine-written file still parses via
    `SDLCState::from_committed_state_json` and reports `None`. A `false` `all_passed` here means
    the run-level authoritative gate failed even though every task passed its own tripwire and
    review; it degrades the run's terminal status but does not flip it to `"blocked"` (see
    [D12](../../planning/decisions/D12-per-task-vs-final-check-depth.md)).
  - **`build`/`writer`/`host`** (top-level keys, `EN.11.A`) — stamped by
    `SDLCState::to_committed_state_json` on every write, same additive precedent as `run_id`:
    `build.git_sha`/`build.built_at` are the compile-time identity of the engine binary that wrote
    this file (`engine_core::build_info::{GIT_SHA, BUILT_AT}`, sourced from `build.rs` via `env!`,
    falling back to `"unknown"` outside a git checkout); `writer` is the fixed string
    `"engine-rs"`; `host.hostname`/`host.pid` (`build_info::host_stamp()`) identify the process
    that produced the write. Together they let a reader tell which build/host/process is behind
    any given state file — useful once more than one binary or machine can write the same spec's
    state. `SDLCState::from_committed_state_json` still parses a file missing all three keys.
  - **Artifact staleness** — `committed_artifact_is_stale(value, current_run_id)` compares a
    committed artifact's `run_id` against the current run's: they differ (including the artifact
    having no `run_id` at all while the current run does) -> stale; they match -> not stale; both
    absent -> not stale (nothing to compare against, so it declines to guess rather than
    flagging a false positive).
  - **Terminal status on a failed walk** (EN.6.J) — a node returning `Err` halts the walk before
    `WrapUpNode` ever runs, which used to leave this file saying `"running"` forever. `engine-serve`
    now detects that outcome after the walk exits (a failed node run, an `Err` from `run_with`, or
    a panic) and calls `wrap_up::write_terminal_blocked_state`, which writes `status: "blocked"`
    plus a `bail_reason` naming the failure directly into the file. This write is file-only — it
    does **not** `git commit` (unlike `SaveStateNode`'s per-task saves), since it runs from
    `engine-serve`'s post-walk cleanup outside any node and outside the `CommandRunner` seam; the
    next `SaveStateNode`/`WrapUpNode` write on a resumed run commits it along with its own changes.
    It is a no-op (no file, no panic) for a context that isn't an SDLC flow run, or that has no
    worktree / no loaded state.
- **PR** — `PullRequestNode` opens one via `gh pr create --base main --head <branch>`, never
  auto-merges (D25). `pr_url` lands in that node's output; skipped with `{pr_url: null, skipped:
  true}` when `auto_pr` was `false`.
- **Docs patch** — `PatchDocsNode`'s output (`{summary, files_patched}`) records which docs it
  claims to have patched; the actual edits are the model's own tool-use, not a separate diff to
  review elsewhere.
- **`WrapUpNode`'s rendered text** (`log_entry`, `report`, `status_suggestion`) — a human-readable
  summary of the PASS/PARTIAL-FAIL outcome and task/attempt counts, surfaced through the live-state
  and durable-write paths below. This Rust port does not itself append anything to a
  `worklog.md`/`log.md` file — a downstream step would need to consume this node's output to file
  it somewhere.
- **Live/durable state** — every node-boundary snapshot flows through `on_progress` to an
  in-memory `LiveStateStore` (what a local Console reads for live progress without polling) and a
  durable Postgres `events`-row writer (`engine-store`) — the two paths that `bastion-ui`/
  `bastion monitor`-style tooling would read, distinct from the on-disk state file.

## Restart vs. resume — what `resume` actually does

`resume` is the single switch deciding whether a re-triggered spec **continues** its previous run or
**restarts** it. It is a typed event field (`SDLCFlowEventSchema::resume`, `#[serde(default)]` =
`false`), read by exactly two nodes:

| `resume` | `SetupWorktreeNode` | `LoadTaskStateNode` |
|---|---|---|
| `true` | Reattaches to the existing worktree if it's on disk; `git checkout <branch>` | Loads `sdlc/sdlc-flow-state.json` when present — statuses, attempt counts, telemetry all carry forward |
| `false` | `git worktree add … -b <branch>` / `git checkout -B <branch>` — recreates the branch | **Archives** any existing state file, then bootstraps fresh from `tasks.json` (every task back to `pending`, `attempt_count` 0) |

**The archive.** A restart never deletes or overwrites the previous run's state — the corpse is
forensics. The file is renamed beside itself to:

```
planning/<spec_slug>/sdlc/sdlc-flow-state.json.superseded-<discriminator>.bak
```

- `<discriminator>` is the **old file's own stamped `run_id`** (the `events.id` UUID written by
  `EN.6.J`), so the archive names the run whose record it is.
- States with no `run_id` — written before `EN.6.J`, or by base-template's JS `sdlc-flow.js`
  engine, which never emits the key — fall back to `<status>-attempts<N>`, where `N` is the summed
  `attempt_count` across all tasks. The fallback is derived from the file's contents, never from a
  clock, so archiving is reproducible.
- A name collision (restarting the same superseded run twice) appends `-2`, `-3`, … rather than
  clobbering the earlier archive.
- A state file that fails to parse is **not** archived: the run errors with the parse failure and
  leaves the file in place, so corruption is reported rather than silently renamed.

**Two edge cases, both pinned deliberately:**

- `resume: true` with **no** state file is a graceful fresh start, not an error. The caller asked to
  continue and there is nothing to continue from — that's a normal first run.
- `resume: false` with a state file but **no** `tasks.json` loads the state anyway and archives
  nothing. There is nothing to bootstrap back from, so continuing beats stranding the spec.

**This is a behavior change (2026-08-02).** Before it, `LoadTaskStateNode` loaded the state file
whenever it existed and ignored `resume` entirely. A spec that had bailed therefore reloaded its own
corpse — every task `failed` with attempts exhausted — and re-bailed within seconds without doing
any work, with no API-level way to re-run it short of moving the state file by hand inside the brain
vault. If you were relying on a bare re-POST silently continuing a run, add `"resume": true`.

> Not to be confused with **`POST /events/{id}/resume`**, the suspend/resume index — a different
> mechanism entirely (resuming a *suspended* engine run by its event id), unrelated to this field.

## Inspecting a stalled or crashed run, and resuming

Since `SaveStateNode` writes `sdlc-flow-state.json` once per **completed** task-loop iteration (not
mid-attempt), a crash mid-run loses at most the in-flight task's current attempt — every task that
already finished a full implement→test→triage→review cycle is durably recorded.

**To inspect:** read `planning/<spec_slug>/sdlc-flow-state.json` directly. Each task's `status`
tells you where it stopped (`Pending` = never started this run, `InProgress`/still-`Pending` after
a crash = wherever `SaveStateNode` last wrote, `Done`/`Failed` = finished). `telemetry` gives
cumulative attempt/pass/fail counts; `policy`/`outcomes` are only present if the run reached
`WrapUpNode`.

**To resume:** re-trigger `POST /events/` with the same `spec_slug` and `resume: true`:

```json
{ "workflow_type": "SDLC_FLOW", "data": { "spec_slug": "EN.3.C-...", "resume": true } }
```

- `SetupWorktreeNode` sees `resume: true` and an existing worktree path on disk, and reattaches
  instead of re-running `git worktree add`.
- `LoadTaskStateNode` sees `resume: true` and prefers `sdlc-flow-state.json` over `tasks.json`, so
  it loads wherever the crashed run last saved — task statuses, attempt counts, and telemetry all
  carry forward. **`resume: true` is required here**: without it the node archives the state file
  and restarts the spec from `tasks.json` (see [Restart vs. resume](#restart-vs-resume--what-resume-actually-does)).
- `TaskQueueRouterNode` then finds the first still-`PENDING` task and continues the loop from
  there — no re-running of already-`Done` tasks.

## Other operationally-relevant details

- **Event fields** (`SDLCFlowEventSchema`): `spec_slug` (required), `task_range` (e.g. `"1-3,5"`,
  1-indexed inclusive, rejects `end < start`), `resume` (default `false` — continue the previous run
  vs. archive its state and restart; see [Restart vs. resume](#restart-vs-resume--what-resume-actually-does)), `auto_pr` (default
  **`true`**), `branch_name` (defaults to `sdlc/<spec_slug>`), `llm_triage` (default `false`),
  `policy` (optional per-run override), `profile` (optional named policy-profile bundle — see
  [sdlc-flow-policy.md](sdlc-flow-policy.md)), `repo` (optional, `EN.3.K` — see below).
- **`repo` — the dispatch target, as a registry slug, never a path (`EN.3.K`).** `repo` is an
  `Option<String>` naming an entry in `brain.toml`'s `[[repos]]` list (e.g. `"bastion"`, `"mev"`,
  `"engine-rs"`) — **a slug, never a filesystem path.** This is a deliberate security boundary, not
  a stylistic choice: the `SDLC_FLOW` graph's agentic nodes (`ImplementTaskNode`, `PatchDocsNode`)
  run with `dangerously_skip_permissions: true` by design
  (`planning/decisions/D8-autonomous-node-write-permission.md`), so a caller-supplied **path** in
  the event payload would let anything holding the `X-API-Key` point an autonomous,
  skip-permissions agent at any directory on the machine. A **slug** bounds the reachable set to a
  deliberate, reviewable list — the blast radius of a leaked key, a typo, or a compromised caller
  becomes "one of the repos in `brain.toml`" instead of "the filesystem". No code path anywhere in
  this workflow accepts, joins, or canonicalizes a caller-supplied path for `repo`.
  - **Resolution**: the process-global repo registry (`crates/engine-core/src/repo_registry.rs`,
    installed from `ENGINE_BRAIN_ROOT` at server startup — see
    [deployment-launchd.md](../deployment-launchd.md)) maps `repo` to `brain_root.join(repo_path)`.
    Every resolvable path is inside the brain root by construction — a `repo_path` that escapes it
    (e.g. via `..`), does not exist, or is not a directory is a typed resolution error, not a
    silently accepted path.
  - **`ENGINE_REPO_ALLOWLIST`** — an optional, comma-separated env var that narrows the registry to
    a subset of `brain.toml` slugs. Unset (the default, and what the Mac Mini runs) means every
    `brain.toml` slug is reachable; set, it intersects. It exists so a future
    internet-exposed deployment can shrink the reachable set (e.g. to `engine-rs,bastion`) without
    editing the brain.
  - **Absent `repo` — byte-identical to pre-`EN.3.K` behavior.** An event with no `repo` field
    resolves its target root to `std::env::current_dir()`, exactly as before this block: the same
    relative `worktree_path` (`"."` / `trees/{branch}`), the same `Path::new(".")` git cwds, and the
    same `PolicyConfigSource::Worktree(current_dir())`. No new rejection path is introduced for
    absent-`repo` events.
  - **Two new `POST /events/` 422 conditions**, checked before a `run_id` is minted or `spawn_run`
    is called (so a rejection registers no live-state entry and needs no cleanup): an unknown
    `repo` slug (`{"error": "unknown repo", "repo": "<slug>", "message": "..."}`), and a
    `spec_slug` whose directory does not exist under the resolved target root
    (`{"error": "unknown spec_slug", "spec_slug": "<slug>"}`). **Carve-out:** a spec directory that
    *exists* but has no `tasks.json` yet is **not** rejected — that is the legitimate
    `GenerateTasksNode` "author a missing task list" path, and still dispatches `202` and routes
    accordingly. A valid request otherwise still returns the unchanged `202 {run_id, event_id}`
    contract (`EN.5.F`), `repo`-bearing or not.
  - **Not a policy knob.** `repo` selects the run's *target*, not a cost/latency/quality trade —
    per standing rule 6's own test, it therefore lives as a plain event field, is not part of
    `SdlcPolicy`, and is not set in any of the three named policy profiles.
- **Worktree/branch naming**: branch defaults to `sdlc/<spec_slug>`; worktree path is
  `trees/<branch>`. A fresh (non-resume) run does `git worktree add trees/<branch> -b <branch>
  origin/main`. Both branch-creation sites (that one, and the `git checkout -B <branch>
  origin/main` used when `use_worktree` is false) are preceded by a **best-effort, non-fatal
  `git fetch origin main`**, so the base ref is not an arbitrarily stale local `origin/main` — run
  `2d46b140` based itself on a pre-merge SHA for exactly that reason. A failing or unreachable
  fetch is ignored and the run proceeds off whatever `origin/main` the local repo already had.
  Neither resume path fetches: they create no branch from `origin/main`.
- **Bail conditions**: a task bails the whole run (routes to `WrapUpNode`, skipping any remaining
  pending tasks) via either `TriageRouterNode`'s `MAJOR_BAIL` (attempts exhausted — default
  `max_attempts: 3`) or `ReviewRouterNode`'s "structural" review failure (0 issues reported, or
  more than 5 — `STRUCTURAL_ISSUE_THRESHOLD`). A minor review fail (1–5 issues) or a `RETRYABLE`
  triage verdict instead loops back through `IncrementAttemptNode` to retry the task. An
  unrecognized verdict string from either stage (neither `PASS`/`RETRYABLE`/`MAJOR_BAIL` nor a
  parseable review verdict) also bails to `WrapUpNode` rather than leaving the router with no
  match — `WrapUpNode`'s terminal `bail_reason` names the offending string
  (`"unrecognized triage verdict: <string>"` / `"unrecognized review verdict: <string>"`),
  distinguishing it from a genuine `MAJOR_BAIL`/structural-review bail (EN.3.G).
- **`TestTaskNode` scope gap**: only the `command` validation-check kind from
  `planning/harness.json` is fully implemented; other declared kinds
  (`forbidden-pattern-scan`/`baseline-diff`/`count-delta`/`warning-scan`) fail closed with an
  explicit "not yet supported" error rather than silently passing.
- **Error handling shape**: a node's `Err` halts the walk, but `Workflow::run`/`run_with` still
  return `Ok(TaskContext)` with whatever accumulated — not a hard workflow-level error. A separate
  `WorkflowError` type is reserved for graph-shape problems (e.g. an unresolvable node identity),
  which would surface at registration/dispatch time, not mid-run.
- **Transport-level retry (`ticket-implement-node-transport-retry`)**: before a `claude_code_rs`
  transport failure ever reaches the halt-on-`Err` behavior above, `ClaudeCodeStep::process`
  retries it in place, bounded by a `TransportRetry` budget (attempt cap + exponential backoff,
  capped at `MAX_TRANSPORT_BACKOFF_MS`). Only the transient/cheap-to-retry
  `claude_code_rs::Error` variants (`Spawn`, `Timeout`, `Cli`, `Api`) are retried; the
  deterministic ones (`BinaryNotFound`, `Parse`, `Isolation`) fail on the first attempt, since a
  retry would reproduce the same error. The budget is not unbounded: a persistent failure still
  exhausts it and becomes a `NodeError`, which still halts the walk exactly as before — this is a
  deferral of the halt, not a removal of it. A cancellation already in effect is checked before
  the first attempt and between retries, so a cancelled run is never resurrected by a retry. This
  is a property of `ClaudeCodeStep` itself, not of any one caller — it applies uniformly to all
  five nodes built on it (`ImplementTaskNode`, `TriageTaskNode`, `ConsolidatedReviewNode`,
  `GenerateTasksNode`, `PatchDocsNode`), since none of them currently pass a per-stage override to
  `ClaudeCodeStep::with_retry_policy` and every constructor defaults to the same
  `TransportRetry::default()` budget. `SdlcPolicy::transport_retry`
  (`crates/engine-core/src/workflows/sdlc_flow/policy.rs`) exists as the future
  four-layer-resolved override surface for this budget, but as of this writing no call site wires
  it in yet — every stage runs the same built-in default. See
  `crates/engine-core/src/nodes/claude_code_step.rs` and this
  ticket's Amendment Log (`planning/ticket-implement-node-transport-retry/tasks.md`) for the full
  retryable/non-retryable classification and the reasoning for applying the retry to all five
  consumers rather than implement-only.
