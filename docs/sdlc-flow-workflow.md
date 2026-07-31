---
type: Reference
title: SDLC Flow Workflow
description: How the SDLC_FLOW workflow graph works — node roles, triggering from engine-rs and bastion, stopping a run, reading outputs, and inspecting/resuming state
doc_id: sdlc-flow-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc-flow, workflow, graph, nodes, resume, abort, cancellation, state, bastion, http]
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
                            | FinalValidationNode -> PatchDocsNode -> WrapUpNode -> PullRequestNode
                                -> EmitStateNode }

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
| `GenerateTasksNode` | **Model** (Opus) | Planning-fallback path only — gathers `planning/<slug>/*.md` context and prompts for a task list, writing `tasks.json` + `tasks.md`. Sets `config.json_schema` and prefers the model's structured output (`ctx.nodes["GenerateTasksNode"]["structured"]`) over fence-stripped text parsing, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Hardcoded to Opus; does not read policy. |
| `LoadTaskStateNode` | Deterministic | Loads `sdlc-flow-state.json` if present (always preferred over `tasks.json`), else bootstraps a fresh `SDLCState` from `tasks.json`. Applies the event's `task_range` filter. |
| `TaskQueueRouterNode` | Deterministic router | Finds the first `PENDING` task and routes to `ImplementTaskNode`; if none remain, routes to `FinalValidationNode` (task loop exit — the drain branch). |
| `ImplementTaskNode` | **Model** (Sonnet, tunable via policy) | Drives the model to implement the current task; parses `{summary, modified_files, tests_added}`, preferring the model's structured output (`ctx.nodes["ImplementTaskNode"]["structured"]`) over fence-stripped text parsing and falling back to `strip_json_fence` + `serde_json::from_str` (or a synthesized `ImplementOutput` on parse error) when structured output is absent. Reads policy for model tier, prompt-cache anchor, and verbosity directive — never rewired to the `local` tier regardless of policy. |
| `TestTaskNode` | Deterministic | Runs the worktree's `planning/harness.json` validation-suite `checks`. Only the `command` check kind is fully supported today — other declared kinds fail closed with a "not yet supported" message rather than silently passing. |
| `TriageTaskNode` | Deterministic, **conditionally model** | Classifies the test result as `PASS` / `RETRYABLE` / `MAJOR_BAIL`. Deterministic by default; only calls the model (Sonnet, tunable) when `event.llm_triage == true` **and** the task is failing-but-under-budget — that LLM branch sets `config.json_schema` and prefers the model's structured output over fence-stripped text parsing, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Also reads policy on the `PASS` branch for deterministic trivial-diff classification (`review_skip_max_files`/`review_skip_max_diff_lines`), independent of `llm_triage`. |
| `TriageRouterNode` | Deterministic router | On `TriageTaskNode`'s verdict: `PASS` → `ConsolidatedReviewNode` or `UpdateTaskStatusNode` (per policy's `review_mode`); `RETRYABLE` → `IncrementAttemptNode`; `MAJOR_BAIL` → `WrapUpNode`; any other/unrecognized verdict string → `WrapUpNode` (fallback, not `None`) with the offending string stamped as `unrecognized_verdict` on `TriageTaskNode`'s result. |
| `ConsolidatedReviewNode` | **Model** (Sonnet, tunable via policy) | Reviews the task's `git diff main..HEAD` against acceptance criteria; parses `{verdict, summary, issues}`, preferring the model's structured output (`ctx.nodes["ConsolidatedReviewNode"]["structured"]`) over fence-stripped text parsing and falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Reads policy for model tier/prompt-cache/verbosity; can be rewired to the `local` tier. |
| `ReviewRouterNode` | Deterministic router | On review verdict: `PASS` → `UpdateTaskStatusNode`; minor `FAIL`/`PARTIAL` (1–5 issues) → `IncrementAttemptNode`; structural `FAIL`/`PARTIAL` (0 or >5 issues) → `WrapUpNode` (bail); any other/unrecognized verdict string → `WrapUpNode` (fallback, not `None`) with the offending string stamped as `unrecognized_verdict` on `ConsolidatedReviewNode`'s result. |
| `IncrementAttemptNode` | Deterministic | Shared retry back-edge target for `TriageRouterNode`'s `RETRYABLE` and `ReviewRouterNode`'s minor fail. Bumps `task.attempt_count` + telemetry, forwards to `ImplementTaskNode`. |
| `UpdateTaskStatusNode` | Deterministic | Mutates the durable state: sets the task's status to `Done`/`Failed`, bumps telemetry counters. |
| `SaveStateNode` | Deterministic | Serializes the latest `SDLCState` to `planning/<slug>/sdlc-flow-state.json` inside the worktree, `git add` + `git commit`. Runs once per completed task-loop iteration, then loops back to `TaskQueueRouterNode`. |
| `FinalValidationNode` | Deterministic | The run-level authoritative gate (`EN.3.E`). Runs the worktree's `planning/harness.json` `validation.checks[]` at `TestDepth::Full` with NO `perTask` filter (`apply_per_task_filter = false`) and an empty `task_validation_commands` slice — so `cargo build --release` (marked `"perTask": false`) runs here even though `TestTaskNode` skips it. Sits on the task-loop drain branch only (`TaskQueueRouterNode`'s "no pending" edge), so it runs exactly once per run, never per task; the `ImplementTaskNode` branch and both routers' bail edges into `WrapUpNode` do not pass through it. It is unconditional and **not** policy-gated — no `SdlcPolicy` field, profile entry, or `harness.json` key changes whether it runs or at what depth (see [D12](../planning/decisions/D12-per-task-vs-final-check-depth.md) and the [two-check-site model](#the-two-check-site-model-per-task-tripwire-vs-final-gate) below). A failing gate does **not** halt the walk — it stamps `{all_passed, check_results, failure_summary}` (the same shape `TestTaskNode` emits) and returns `Ok`, so the run still reaches `EmitStateNode` and `WrapUpNode` reports a degraded terminal status rather than bailing. |
| `PatchDocsNode` | **Model** (Sonnet) | Reads the most recent `ImplementTaskNode`'s `modified_files`, asks the model to find and patch stale `docs/` references (the model edits files itself via its own tool use — this node doesn't). Sets `config.json_schema` and prefers the model's structured output (`ctx.nodes["PatchDocsNode"]["structured"]`) over fence-stripped text parsing for its own `{summary, files_patched}` output, falling back to `strip_json_fence` + `serde_json::from_str` when structured output is absent. Hardcoded to Sonnet; does not read policy. |
| `WrapUpNode` | Deterministic | Computes the PASS/PARTIAL-FAIL outcome, renders `log_entry`/`report`/`status_suggestion` text, stamps `policy` + `RunOutcomes` telemetry into `SDLCState`, and persists that stamped state to `sdlc-flow-state.json` — the only point the `policy`/`outcomes` blocks reach disk, since `SaveStateNode` never runs again after the loop exits. Also `git add`/`git commit`s that write itself (via the same shared `commit_state_file` helper `SaveStateNode` uses), so a `--worktree` run's PR now contains its own terminal state (EN.3.G task 3). Terminal target for both routers' bail branches, and the natural end of a fully-passing run. |
| `PullRequestNode` | Deterministic | Pushes the branch and opens a PR via `gh pr create` — never auto-merges (human review gate, decision D25). No-op (`{pr_url: null, skipped: true}`) when `event.auto_pr == false` (default `true`). |
| `EmitStateNode` | Deterministic | Runs `mev emit-state --write` in the worktree to refresh the brain freshness spine. Also patches the committed `sdlc-flow-state.json`'s `pr` block (`url`/`number`, parsed from `PullRequestNode`'s PR URL) in place and re-commits it via `commit_state_file`, since `PullRequestNode` runs after `WrapUpNode` and so cannot have populated that block itself (EN.3.G task 6). Infallible best-effort: any failure mode (skipped PR, no worktree, missing/unparsable state file) is a silent no-op. Terminal node. |

**Nodes that read the tunable policy:** `SetupWorktreeNode` (resolves it, but idempotently as of
`EN.5.D` — a served run's dispatch factory, `engine-serve::register_sdlc_flow`, already resolves
policy via `resolve_policy_for_run_from(&event.data, &PolicyConfigSource::Worktree(cwd))` and
seeds `RESOLVED_POLICY_IDENTITY` before this node runs, so it only resolves + stamps when no
policy was seeded yet — in-tree/CLI-driven runs and unit tests driving this node directly),
`ImplementTaskNode`,
`TriageTaskNode`, `ConsolidatedReviewNode`, `TriageRouterNode`, `WrapUpNode` — plus
`registry_for_policy` in `graph.rs`, which isn't a node but decides whether `TriageTaskNode`/
`ConsolidatedReviewNode` get rewired to the local-model transport. Every other node either has a
hardcoded model choice (`GenerateTasksNode`: Opus, `PatchDocsNode`: Sonnet) or makes no model call
at all. Full knob-by-knob reference: [sdlc-flow-policy.md](sdlc-flow-policy.md).

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
[D12](../planning/decisions/D12-per-task-vs-final-check-depth.md) for the full rationale.

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
    [D12](../planning/decisions/D12-per-task-vs-final-check-depth.md)).
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
- `LoadTaskStateNode` always prefers `sdlc-flow-state.json` over `tasks.json` when both exist, so
  it naturally loads wherever the crashed run last saved — task statuses, attempt counts, and
  telemetry all carry forward. There's no separate "resume" flag this node checks; presence of the
  state file is sufficient.
- `TaskQueueRouterNode` then finds the first still-`PENDING` task and continues the loop from
  there — no re-running of already-`Done` tasks.

## Other operationally-relevant details

- **Event fields** (`SDLCFlowEventSchema`): `spec_slug` (required), `task_range` (e.g. `"1-3,5"`,
  1-indexed inclusive, rejects `end < start`), `resume` (default `false`), `auto_pr` (default
  **`true`**), `branch_name` (defaults to `sdlc/<spec_slug>`), `llm_triage` (default `false`),
  `policy` (optional per-run override), `profile` (optional named policy-profile bundle — see
  [sdlc-flow-policy.md](sdlc-flow-policy.md)).
- **Worktree/branch naming**: branch defaults to `sdlc/<spec_slug>`; worktree path is
  `trees/<branch>`. A fresh (non-resume) run does `git worktree add trees/<branch> -b <branch>
  origin/main`.
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
