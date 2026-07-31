---
type: Reference
title: SDLC Flow — Tunable Run Policy & Telemetry
description: How to configure the SDLC Flow's SdlcPolicy (model tiers, review mode, verbosity, local-model tier) and read/aggregate per-run RunOutcomes telemetry
doc_id: sdlc-flow-policy
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc-flow, policy, telemetry, model tiers, review mode, local model, aggregate, cost]
related: [sdlc-flow-workflow, architecture, cli]
---

# SDLC Flow — Tunable Run Policy & Telemetry

Added in EN.3.C. Before this, the SDLC Flow's cost/quality levers (which model tier ran which
stage, how strict review was, how verbose prompts were) were hardcoded. Now every run resolves a
concrete `SdlcPolicy`, applies it, and records `RunOutcomes` telemetry at the tail — so different
policies can be compared on real cost/time/quality data instead of guessed at.

**EN.4.0:** the underlying mechanism — tier types, the four-layer `resolve<P>` precedence, the
model-node shaping helpers, telemetry harvest, and aggregation — was generalized out of
`sdlc_flow` into a workflow-agnostic `engine-core::policy` framework (`crates/engine-core/src/policy/`),
so any workflow can reuse it, not just `sdlc_flow`. `SdlcPolicy` now implements `crate::policy::Policy`
and every SDLC-flow-facing type below (`PolicyAggregate`, `RunOutcomes`/`RunTelemetry`,
`EmitStateNode`) delegates to the generic framework internally; the serialized shapes, CLI-facing
behavior, and everything documented below are unchanged. `sdlc_flow` keeps its own
`CommandRunner`/`default_command_runner` (subprocess execution) and its four concrete named
profiles — those did not move.

Source: `crates/engine-core/src/policy/` (the generic framework: `resolve.rs`, `tier.rs`,
`shaping.rs`, `telemetry.rs`, `aggregate.rs`, `emit_state.rs`, `profiles.rs`) plus
`crates/engine-core/src/workflows/sdlc_flow/policy.rs` (`SdlcPolicy`, its `Policy` impl, and
resolution wired to the generic framework), `profiles.rs` (the four named `PartialPolicy`
bundles), `setup.rs` (`resolve_policy_for_run`, `resolve_profile`, wired into
`SetupWorktreeNode` — both now thin wrappers over `crate::policy::resolve_profile` /
`crate::policy::read_harness_policy_defaults`), `graph.rs` (`registry_for_policy`, the
local-tier rewiring), `schema.rs` (`RunOutcomes`, with `From`/`Into` conversions to/from the
generic `crate::policy::RunTelemetry`; the event's `policy` and `profile` fields), `aggregate.rs`
(cross-run aggregation — `PolicyAggregate` is now a type alias for the generic
`crate::policy::PolicyAggregate<SdlcPolicy>`).

## Resolved once per run, at dispatch (EN.5.D)

Through `EN.4.0`/`EN.4.x`, every model node called `resolve_policy_for_run(&ctx, &worktree)` itself,
inside `process()` — every node re-read `harness.json` and re-resolved the same policy
independently, and no served run ever reached the policy-aware `registry_for_policy` registry at
all (`engine-serve`'s `Dispatcher` only ever built the default-policy graph). `EN.5.D` closes that
gap: `WorkflowFactory` (`engine-serve::dispatch`) now takes the triggering event payload, resolves
policy exactly once inside the factory — before the `Workflow` is even constructed — builds
`registry_for_policy(&policy)`, and seeds the result into the run's initial `ctx.nodes` under
`policy::RESOLVED_POLICY_IDENTITY` (`Workflow::with_seeded_nodes`). Nodes now read the stamp via
`policy::resolved_policy_strict(&ctx)` rather than re-resolving it; reading an absent/unparsable
stamp is an `Err`, not a silent `SdlcPolicy::default()` — a policy that fails to resolve (an unknown
`profile`, a malformed inline `policy` override) fails the *dispatch*, surfaced as
`DispatchError::PolicyResolutionFailed` (HTTP 422, naming the offending profile) rather than quietly
falling back once the run is already underway.

**Config source, decoupled from a worktree.** Resolving `sdlc.policy`/`sdlc.profiles` out of
`harness.json` no longer requires a worktree path in hand: `policy::PolicyConfigSource` has three
variants — `Worktree(path)` (read `<path>/planning/harness.json`, what `SDLC_FLOW` uses, since its
served process is itself a real repo checkout), `HarnessFile(path)` (an explicit file path), and
`Builtin` (skip the file entirely — builtin + profile + event layers only). The three workflows with
no repo checkout at dispatch time (`RESEARCH_AGENT`, `DIAGNOSTIC_INTAKE`, `PROPOSAL_GENERATOR`) all
resolve via `PolicyConfigSource::Builtin` in their `engine-serve::workflows` registrations, so a
worktree-free trigger resolves policy successfully instead of falling back to
`std::env::current_dir()` (which was never actually the run's own worktree anyway — `SDLC_FLOW`'s
own dispatch-time read is off the *serving process's* cwd, not the run's eventual
`SetupWorktreeNode` output, since that node hasn't run yet at dispatch time).

See `planning/decisions/D11-policy-dispatch-seam.md` and `docs/architecture.md`'s `Dispatcher`
entry for the full mechanism; `crates/engine-core/tests/policy_dispatch_e2e.rs` is the hermetic
end-to-end proof (stubbed transports, no live model calls) that a `profile` sent over `POST
/events/` actually reaches a served run and changes which transport a judgment stage calls.

## Configuring a run

Four layers, resolved high-to-low precedence — **per-run event `policy` override > per-run event
`profile` > `harness.json` `sdlc.policy` > built-in default**. Every field is independent: an
unset field at a higher layer just falls through to the next layer down, so you only need to
specify the knobs you want to change.

### 1. Built-in default (baseline)

`SdlcPolicy::default()` in `policy.rs`. Guaranteed to reproduce exactly the pre-EN.3.C behavior:
`normal` verbosity, `per_task` review, all stages on `sonnet`, prompt-cache off, `llm_triage`
false, `max_attempts` 3, no close-out reuse. You never edit this directly — override it via the
layers below.

### 2. `planning/harness.json` → `sdlc.policy` (this repo's defaults)

This repo's own `planning/harness.json` already carries a documented no-op example (every value
matches the built-in default):

```json
"sdlc": {
  "policy": {
    "output_verbosity": "normal",
    "review_mode": "per_task",
    "model_tiers": {
      "implement": "sonnet",
      "implement_simple": "sonnet",
      "review": "sonnet",
      "triage": "sonnet",
      "generate": "sonnet"
    },
    "llm_triage": false,
    "max_attempts": 3,
    "test_depth": "full"
  }
}
```

Flip a value here to change every future `sdlc-flow`/`sdlc-task` run in this repo. Only include
the fields you want to change from the built-in default — omitted fields fall through. This
section is read by `SetupWorktreeNode` on every run (via `resolve_policy_for_run`); if
`planning/harness.json` has no `sdlc.policy` key at all, this layer is skipped entirely.

### 3. Named policy profile (`profile:` — reusable bundle)

Pass a `profile` name (a string) in the `SDLC_FLOW` event JSON instead of (or alongside) an
inline `policy` object:

```json
{
  "spec_slug": "EN.3.C-tunable-run-policy-telemetry",
  "profile": "cheap-fast"
}
```

`resolve_profile` (in `setup.rs`) looks the name up in two places, in order:

1. `planning/harness.json` → `sdlc.profiles[name]` — a repo-local `PartialPolicy` bundle that
   overrides (or adds to) the built-in set.
2. The built-in bundles in `profiles.rs` (`profiles::profile_by_name`) — four canonical
   cost/time/quality tradeoffs:

   | Name | Tradeoff |
   |---|---|
   | `baseline` | Explicit no-op control: Sonnet on every tier, `per_task` review, `llm_triage` off — matches the built-in default, spelled out for clarity. |
   | `cheap-fast` | `haiku` implement, `local` triage + review, `terse` output, `trivial_skip` review. |
   | `pragmatist` | `sonnet` implement, `local` review, prompt caching on, `trivial_skip` review, `llm_triage` on. |
   | `batch-reviewer` | `sonnet` implement, per-task review collapsed into a single end-of-run review (`end_only`). |

   An `event.profile` name found in neither place is an error (`resolve_profile` returns `Err`,
   failing the run) rather than a silent no-op.

A resolved profile bundle sits between `harness.json`'s `sdlc.policy` defaults and the event's
inline `policy` override in the precedence chain — same as any other layer, unset fields fall
through.

> `planning/harness.json`'s `sdlc.profiles` map documents itself with a sibling `_comment` string
> key (see the example below). `read_harness_profiles` strips any `_comment*`-prefixed key before
> deserializing the map, so the comment doesn't get parsed as a (broken) named `PartialPolicy`
> entry — add new profiles as sibling keys of `_comment`, not inside it.

### 4. Per-run event `policy` override (one-off experiment)

Pass a `policy` object (a `PartialPolicy`, same shape as above) directly in the `SDLC_FLOW` event
JSON:

```json
{
  "spec_slug": "EN.3.C-tunable-run-policy-telemetry",
  "policy": { "output_verbosity": "terse", "max_attempts": 5 }
}
```

This wins over `profile`, `harness.json`, and the built-in default for the fields it sets, and
doesn't touch any file — use it to try a variant for a single run without changing this repo's
standing defaults. `policy` and `profile` can be combined: the profile's bundle resolves first,
then the inline `policy` fields override individual knobs on top of it.

## Available knobs

| Field | Values | What it controls |
|---|---|---|
| `output_verbosity` | `terse` \| `normal` \| `verbose` | Verbosity directive added to model-node prompts (`ImplementTaskNode`, triage, review). |
| `prompt_cache` | `bool` | Whether a stable system-prompt anchor is added for provider-side prompt caching. |
| `review_mode` | `per_task` \| `trivial_skip` \| `end_only` | `per_task`: every task routes to `ConsolidatedReviewNode` (today's default). `trivial_skip`: a first-pass-green task under the diff-size thresholds below skips per-task review; a non-trivial task still routes to review. `end_only`: per-task review is collapsed into a single end-of-run review. |
| `review_skip_max_files` / `review_skip_max_diff_lines` | `u32` | Thresholds (`git diff --numstat`) a task's diff must stay under to count as "trivial" for `trivial_skip`. Defaults: 2 files / 40 lines. |
| `model_tiers.{implement,implement_simple,review,triage,generate}` | `sonnet` \| `haiku` \| `opus` \| `local` | Per-stage model tier. `implement`/`implement_simple` only ever resolve to a concrete cloud model string — `local` is not wired for the agentic implement stage (see below). |
| `local.{endpoint,model,constrained_json}` | string / string / bool | Config for the `local` tier's OpenAI-compatible transport (endpoint URL, model name, whether to pass a constrained-decoding `response_format`). Only meaningful when some stage's tier is `local`. |
| `simple_task_max_files` | `u32` | Threshold for classifying a task as "simple" for the `implement_simple` tier. **Not yet consumed** by `ImplementTaskNode`'s tier selection — flagged out-of-scope in EN.3.C task 4, left for a later block. |
| `llm_triage` | `bool` | Whether `TriageTaskNode` invokes the LLM classifier for a failing-but-under-budget task (`true`) vs. deterministically calling it `RETRYABLE` (`false`, default). |
| `max_attempts` | `u32` | Retry budget per task before it's marked `FAILED`. |
| `close_out.reuse.{validation,review,docs}` | `bool` each | Which `close-out` (EN.2.x) stages are allowed to reuse a prior flow record's result rather than re-running. |
| `test_depth` | `full` \| `fast` (default `full`) | Which per-task validation checks `TestTaskNode` runs — see [Per-task check selection (`test_depth`)](#per-task-check-selection-test_depth) below. |

### Per-task check selection (`test_depth`)

Added in EN.3.D, bringing the Rust `SDLC_FLOW` to parity with the JS engine's three per-task
test-selection behaviors (`.claude/workflows/sdlc-flow.js`): `fastCommand` substitution,
`perTask: false` exclusion, and a task's own `validation_commands` replacing the project-wide
harness suite. `TestTaskNode` resolves the checks to run via a pure `select_task_checks` function,
in this precedence order:

| Precedence | Condition | Result |
|---|---|---|
| 1 | The current task's own `validation_commands` (in `tasks.json`) is non-empty | Those commands run verbatim, as-is — `test_depth` is ignored entirely. |
| 2 | Otherwise | `harness.json`'s `validation.checks[]`, minus any `enabled: false` check, minus any `perTask: false` check (excluded at BOTH depths, for `TestTaskNode` specifically — see the caveat below) — and, for the remaining checks, `fastCommand` is substituted for `command` when `test_depth` is `fast` and the check declares a `fastCommand`, falling back to `command` when it doesn't or when `test_depth` is `full`. |
| — | No `planning/harness.json` present AND the task has no `validation_commands` | A gating `harness-missing` failure — never a silent `all_passed: true`. |

`TestTaskNode`'s result additively stamps `test_depth` (the resolved `full`/`fast` value),
`check_source` (`"harness"` or `"task_validation_commands"`), and `excluded_checks` (the names skipped via
`enabled: false` / `perTask: false`) onto its existing result payload, alongside the unchanged
`all_passed` / `check_results` / `failure_summary` shape — so `RunTelemetry`/`PolicyAggregate` can
attribute observed cost to the depth that caused it.

This knob exists because per-task check selection is the single largest iteration-speed lever in
this repo: CLAUDE.md measures the per-task tripwire at **2m44s** (running the full
`cargo nextest run --workspace` + `cargo build --release` on every task attempt) versus **6.4s**
(the `fastCommand`-substituted, `perTask: false`-excluded selection) — a ~25x difference for a
check whose only job is to catch a regression before the next task attempt, not to be the
authoritative gate. That gate is `FinalValidationNode` (`EN.3.E`), a second, unconditional
check-running site on the declared graph's task-loop drain branch that runs the full,
unfiltered suite (including the `perTask: false` `build` check) exactly once per run — see
[FinalValidationNode](sdlc-flow-workflow.md#the-two-check-site-model-per-task-tripwire-vs-final-gate)
and [D12](../planning/decisions/D12-per-task-vs-final-check-depth.md). `test_depth`/`flow.testDepth`
governs `TestTaskNode` only; it is deliberately never read by `FinalValidationNode`, which is
pinned to `TestDepth::Full` regardless of policy (a policy knob may tune a per-task tripwire's
depth, but whether the authoritative suite runs at all is not a cost lever — CLAUDE.md standing
rule 6, "one graph, validated once, is what makes runs comparable"). The end-of-run "Validate"
*task* in an SDLC spec (this repo's own `tasks.json` convention) and CI still separately run the
full suite too, but those are outside the graph — `FinalValidationNode` is what guarantees it
inside every `SDLC_FLOW` run itself.

## Structured-output adoption

Every cloud-side model node that expects a specific JSON reply shape sets `Config.json_schema`
on its `claude_code_rs::Config` before calling `ClaudeCodeStep`/`execute()` — `GenerateTasksNode`
(`setup.rs`, `generated_tasks_schema()`), `ImplementTaskNode`, `TriageTaskNode`, and
`ConsolidatedReviewNode`'s review call (`task_loop.rs`, `implement_output_schema()` /
`triage_output_schema()` / `review_output_schema()`), and `PatchDocsNode` (`docs.rs`,
`patch_docs_output_schema()`). This asks the Claude CLI to constrain its reply to the given
schema and hands back a pre-parsed `Outcome.structured_output` alongside the raw text; each node
prefers the pre-parsed value over its own fence/regex parse of the prompt text, falling back to
that parse only when `structured_output` is `None` (e.g. an older CLI, or a schema-less reply).
This is unconditional — it isn't gated by any `SdlcPolicy` field and applies regardless of which
`model_tiers` value a stage resolves to, as long as the stage runs through the Claude CLI
transport.

This is a distinct mechanism from `local.constrained_json` (see [Available knobs](#available-knobs)
above): `local.constrained_json` is the equivalent guarantee for the **`local` model tier's**
OpenAI-compatible transport (`openai_compat_transport.rs`) — when set, it adds a
`response_format: {"type": "json_object"}` hint to the `/v1/chat/completions` request body sent
to the local endpoint, and the caller is expected to skip its own JSON-repair retry for that
stage. Both mechanisms serve the same goal (a schema-honest reply the node can trust without a
repair pass) over the two different transports the policy can route a stage through — the
Claude CLI (schema requested via `Config.json_schema`, always on) and the local OpenAI-compatible
endpoint (schema-shaped hint gated by `local.constrained_json`, off by default).

## The `local` model tier

Setting `model_tiers.triage` and/or `model_tiers.review` to `local` routes that stage's calls
through `openai_compat_transport` to an OpenAI-compatible endpoint (e.g. a local Ollama server)
instead of the Claude CLI — for cheap, zero-cost judgment calls on cheap hardware.

- **Scoped to single-shot judgment stages only** — `TriageTaskNode`'s LLM-triage branch and
  `ConsolidatedReviewNode`. `ImplementTaskNode` (the agentic implement stage) is **never** rewired
  to `local`, regardless of what `model_tiers.implement` is set to — the local tier isn't suited to
  multi-turn agentic work (see `planning/local-llm-tier-investigation/notes.md`).
- **Automatic fallback**: any failure calling the local endpoint (unreachable, error response)
  falls back to the real Claude CLI transport for that call — a run degrades gracefully rather than
  hard-failing because a local server was down.
- Wired via `registry_for_policy(&SdlcPolicy)` in `graph.rs`, which builds on the default node
  registry and swaps in the local-routed node only for stages resolved to `local`.

## Telemetry: `RunOutcomes`

`WrapUpNode` stamps a `policy` snapshot (the resolved `SdlcPolicy` for that run) and a
`RunOutcomes` block into `SDLCState` at the run tail:

- `wall_clock_secs` — from `SetupWorktreeNode`'s start to the snapshot.
- `total_attempts` / `total_retries` — implement→test attempts across all tasks, and the subset
  beyond each task's first attempt.
- `tasks_passed` / `tasks_failed`.
- `review_verdicts` — e.g. `["TriageTaskNode:RETRYABLE", "ConsolidatedReviewNode:PASS"]`.
- `total_input_tokens` / `total_output_tokens` / `total_cost_usd`.
- `model_tier_used` — per-stage tier actually **called**, not the resolved policy's intent (see
  below).

**Observed vs. intended tier (`EN.5.D`).** `openai_compat_transport`'s `local` tier fails fast and
silently falls back to the real cloud transport when its endpoint is unreachable — deliberately, so
a down local server never hard-fails a run. That fallback is invisible to intent-derived telemetry:
if `model_tier_used` just echoed the resolved policy, a run whose `local`-tier review stage silently
fell back to cloud would still report `"review": "local"`. `RunTelemetry` instead harvests each
stage's tier from what the transport actually stamped —
`ctx.nodes[stage]["transport"]["tier"]`/`["model"]`/`["endpoint"]` (a `MetaTransport`, EN.5.D task 9;
`openai_compat_meta_transport` stamps `{"tier": "local", ...}` on a successful local call and
`{"tier": "cloud", ...}` on the fallback) — and this **observed** value overrides any
caller-supplied `model_tier_used` entry for that same stage. A stage that ran no model this run (or
whose node only exposes the plain, non-meta `with_transport` seam — still generic `"cloud"`-tier,
per `ClaudeCodeStep`'s doc comment) simply has no observed entry, leaving the caller-supplied value,
if any, as the only source for that key.

`Workflow::run_with` (`crates/engine-core/src/workflow.rs`) now stamps this generic
`policy::telemetry::RunTelemetry` snapshot into every completed run's
`TaskContext::metadata["run_telemetry"]` automatically (`EN.5.D` task 10) — not only under the
`#[ignore]`d profile-ranking experiments. `SDLC_FLOW`'s own `WrapUpNode` still separately computes
its precise `RunOutcomes` (with `total_attempts`/`tasks_passed`/etc. it alone can derive from
`SDLCState`) into `ctx.nodes["WrapUpNode"]`/the on-disk state file below — the two writes sit at
different `ctx` locations and don't disturb each other.

This is persisted to `planning/<spec-slug>/sdlc/sdlc-flow-state.json` (the same file
`SaveStateNode` writes throughout the run — `WrapUpNode`'s policy/outcomes blocks land in that
same on-disk snapshot). A state file from a run that never reached `WrapUpNode` (bailed early, or
predates EN.3.C) simply has `policy`/`outcomes` both `null`.

## Aggregating across runs

`aggregate.rs` groups a set of completed state files by **identical resolved policy** (exact
field-for-field match) and tabulates cost/time/quality per policy group — the same shape as the
"Consolidated ranking" table in `planning/sdlc-token-time-economics/notes.md`, but computed from
real run data instead of hand-estimated:

```rust
use engine_core::workflows::sdlc_flow::aggregate::aggregate_state_files;

let rows = aggregate_state_files(&[
    "planning/spec-a/sdlc/sdlc-flow-state.json",
    "planning/spec-b/sdlc/sdlc-flow-state.json",
])?;
// rows: Vec<PolicyAggregate> — one row per distinct policy, with run_count,
// total/avg cost, total/avg wall-clock, token sums, attempts/retries,
// pass_rate, and a review_verdict_counts tally.
```

Runs missing either the `policy` or `outcomes` block are silently skipped (there's no policy to
key them by). **There is no CLI command wrapping this yet** — it's a library-only API today; call
it from a Rust test/scratch binary, or wire a `bastion`/CLI subcommand in a later block if this
needs to be run routinely.

A worked example lives at `crates/engine-core/tests/sdlc_flow_experiment.rs`: a `#[ignore]`-gated
real-CLI test (`experiment_four_profiles_real_cli_ranked_by_cost`) that drives the four named
profiles (`baseline`, `cheap-fast`, `pragmatist`, `batch-reviewer`) through the full `SDLC_FLOW`
graph against a synthetic multi-task fixture, then calls `aggregate_state_files` on the resulting
state files and prints a table ranked by ascending `avg_cost_usd`. It also asserts that more than
one distinct resolved-policy row shows up, as an end-to-end proof that policy stamping actually
changes run behavior. Run it explicitly with
`cargo test -p engine-core --test sdlc_flow_experiment -- --ignored`.

## Gaps / follow-ups (as of EN.3.C)

- `simple_task_max_files`-based tier classification in `ImplementTaskNode` is defined in the
  policy but not yet consumed.
- No CLI surface for `aggregate.rs` — Rust-only today.
- No `bastion`/dashboard visualization of `PolicyAggregate` rows yet.
