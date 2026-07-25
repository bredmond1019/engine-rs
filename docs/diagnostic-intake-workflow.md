---
type: Reference
title: Diagnostic Intake Workflow
description: How the DIAGNOSTIC_INTAKE workflow works — single-node structured extraction from raw diagnostic-call notes/transcript, event schema, tunable DiagnosticIntakePolicy (with a Local-tier rewire), triggering, and reading outputs
doc_id: diagnostic-intake-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [diagnostic-intake, workflow, graph, policy, structured extraction, local model, evidence contract]
related: [architecture, research-agent-workflow, sdlc-flow-workflow, sdlc-flow-policy, data-contract]
---

# Diagnostic Intake Workflow

`DIAGNOSTIC_INTAKE` (block EN.4.B) is a net-new, policy-aware, single-node workflow that turns
raw diagnostic-call notes/transcript into a validated `DiagnosticIntake` evidence contract
(`agentic-portfolio/business/docs/diagnostic/intake.md §3`). It is built on the EN.4.0 shared
policy framework introduced for `SDLC_FLOW`/`RESEARCH_AGENT` (see
[sdlc-flow-policy.md](sdlc-flow-policy.md) for that framework's mechanics — this doc only covers
how `DIAGNOSTIC_INTAKE` configures and uses it).

Source: `crates/engine-core/src/workflows/diagnostic_intake/` (`mod.rs`, `schema.rs`,
`policy.rs`, `profiles.rs`, `extract.rs`, `graph.rs`), registered from
`crates/engine-serve/src/workflows.rs` (`register_diagnostic_intake` →
`register_builtin_workflows`).

## Graph shape

```
IntakeExtractNode
```

Exactly one node. Unlike `RESEARCH_AGENT`'s router + two terminal nodes, `IntakeExtractNode` is
both the start node and the sole (terminal) node — there is no router.

| Node | Kind | What it does |
|---|---|---|
| `IntakeExtractNode` | **Model** (Sonnet by default, tunable via policy; the only stage in this workflow, and the only one across `sdlc_flow`/`research_agent`/`diagnostic_intake` that is Local-eligible) | Wraps `ClaudeCodeStep` with **no** `WebSearch`/`WebFetch` tools (pure extraction) and a `DiagnosticIntake` `json_schema`. Resolves the run's `DiagnosticIntakePolicy`, applies `extract`-stage tier/prompt-cache/verbosity shaping, parses the reply into a `DiagnosticIntake`, stamps it + usage onto `ctx`, and persists `diagnostic-intake-state.json`. |

## Event schema (`DiagnosticIntakeEventSchema`)

```json
{
  "notes": "Client call transcript: ...",
  "profile": "baseline"
}
```

| Field | Notes |
|---|---|
| `notes` | Required. Raw diagnostic-call notes or transcript text. `IntakeExtractNode`'s prompt ports `intake.md`'s four interview groups + evidence discipline against this text. |
| `policy` | Optional per-run `PartialDiagnosticIntakePolicy` override — highest-precedence layer. |
| `profile` | Optional. Name of a built-in or `harness.json`-defined policy profile bundle (e.g. `"baseline"`, `"local-extract"`). |

## Structured output

**`DiagnosticIntake`** (`IntakeExtractNode`): `company_name`, `company_type`, `team_size`,
`primary_channels[]`, `existing_tools[]`, `existing_automations[]`, `top_workflows[]` — a list of
`WorkflowCandidate { name, description, frequency_evidence, time_cost_evidence,
buildability_notes, knowledge_holder, failure_mode }`. Only `company_name`, `company_type`,
`team_size`, and `top_workflows` are JSON-schema `required`; each `WorkflowCandidate` only
requires `name`+`description`, since real transcripts may genuinely leave a rubric axis
unaddressed — evidence discipline means flagging a gap (empty string), never inventing content.

This type is **load-bearing**: EN.4.C imports `DiagnosticIntake` by name (re-exported from
`workflows::diagnostic_intake` in `mod.rs`).

`IntakeExtractNode` sets `Config.json_schema` on the underlying `claude_code_rs::Config` (via
`diagnostic_intake_json_schema()` in `schema.rs`) and prefers the model's pre-parsed structured
output over fence-stripped text parsing, the same idiom `sdlc_flow`/`research_agent`'s model
nodes use (see [Structured-output adoption](sdlc-flow-policy.md#structured-output-adoption)).

### Evidence discipline

The extraction prompt (`extract.rs::build_prompt`) ports `intake.md`'s four interview groups
(company context, process & pain, tool landscape, existing automations) and enforces one rule
above the rest: every `*_evidence` field (`frequency_evidence`, `time_cost_evidence`) must hold
the client's own words or a direct observation from the notes — never inference. An unsupported
field is left an empty string rather than invented, since a downstream scoring stage reads these
fields directly. The prompt also encodes São Paulo SMB priors (`intake.md §5`: WhatsApp as
system of record, Pix as payment backbone, Mercado Livre/Instagram as storefront, spreadsheets as
the glue) to recognize, not assume, in the notes.

## Policy: `DiagnosticIntakePolicy`

Same four-layer precedence as `SdlcPolicy`/`ResearchAgentPolicy` — **per-run event `policy`
override > per-run event `profile` > `harness.json` `diagnostic_intake.policy` defaults >
built-in default** — resolved via the shared `crate::policy::resolve` framework (EN.4.0). There
is no dedicated setup node in this workflow; `IntakeExtractNode::process` calls
`profiles::resolve_policy_for_run(ctx, worktree)` itself before applying shaping to its `Config`.

Knobs (a single-stage subset of `SdlcPolicy`'s):

| Field | Values | What it controls |
|---|---|---|
| `output_verbosity` | `terse` \| `normal` \| `verbose` | Verbosity directive added to the extraction prompt. |
| `prompt_cache` | `bool` | Whether a stable system-prompt anchor is added for provider-side prompt caching. |
| `model_tiers.extract` | `sonnet` \| `haiku` \| `opus` \| `local` | The one stage's model tier. |
| `local.{endpoint,model,constrained_json}` | string / string / bool | Configuration for the `local` tier — consumed by `graph::registry_for_policy`'s rewire when `model_tiers.extract == ModelTier::Local`. |

Built-in default: `DiagnosticIntakePolicy::default()` — normal verbosity, Sonnet extract tier,
prompt cache off.

**Unlike `RESEARCH_AGENT`'s two cloud-only stages, this workflow's single `extract` stage is
Local-eligible** — pure structured extraction suits a local coder model, so `ModelTier::Local` is
a valid resolved value here. `graph::registry_for_policy(&DiagnosticIntakePolicy)` rewires
`IntakeExtractNode` to route through `openai_compat_transport_live` whenever the resolved
`extract` tier is `Local`, falling back to the real `claude` CLI transport at call time if the
local endpoint is unavailable — the direct analog of `sdlc_flow::graph::registry_for_policy`'s
triage/review rewire, and the inverse of `research_agent::graph::registry_for_policy`'s
permanent no-rewire guard.

### Named profiles

Four built-in bundles in `profiles.rs` (`profile_by_name`), looked up first in
`planning/harness.json` → `diagnostic_intake.profiles[name]`, then in this built-in set:

| Name | Tradeoff |
|---|---|
| `baseline` | Explicit no-op control: Sonnet on `extract`, normal verbosity, prompt cache off — spelled out for clarity, matches the built-in default. |
| `cheap-fast` | `haiku` extract, terse output, prompt caching on. |
| `thorough` | `opus` extract, verbose output. |
| `local-extract` | Rewires `extract` to `ModelTier::Local` with `constrained_json: true`; `local.endpoint`/`local.model` left unset, falling through to `LocalConfig::default()`. Exercises the Local-tier rewire that is this block's key differentiator from `research_agent`. |

`planning/harness.json` carries a matching `diagnostic_intake.{policy,profiles}` section
(mirroring `sdlc.{policy,profiles}` / `research_agent.{policy,profiles}` — see
[sdlc-flow-policy.md](sdlc-flow-policy.md#2-planningharnessjson--sdlcpolicy-this-repos-defaults)
for the reader/precedence mechanics, identical here).

## How to trigger a run

Same HTTP surface as every other `engine-serve` workflow (`docs/cli.md`; see
[sdlc-flow-workflow.md](sdlc-flow-workflow.md#how-to-trigger-a-run) for the full auth/mounting
story):

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "DIAGNOSTIC_INTAKE",
  "data": { "notes": "Client call transcript: ...", "profile": "cheap-fast" }
}
```

`GET /workflows` lists `DIAGNOSTIC_INTAKE` once
`register_diagnostic_intake`/`register_builtin_workflows` has run; `GET
/workflows/DIAGNOSTIC_INTAKE/graph` returns the declared schema above.

## Reading outputs

- **`<worktree>/planning/diagnostic-intake-state.json`** — the telemetry record
  `IntakeExtractNode` persists on completion: `{policy, telemetry}`, where `telemetry` is a
  `RunTelemetryInputs`-shaped harvest from the shared `crate::policy::telemetry` module (cost,
  tokens, model tier used). Same shape as `sdlc_flow`/`research_agent`'s state files, so a batch
  of these can be fed to the shared `policy::aggregate_state_files` aggregator (see
  [sdlc-flow-policy.md](sdlc-flow-policy.md#aggregating-across-runs)) to rank named profiles by
  cost.
- **`ctx.nodes["IntakeExtractNode"]`** — the parsed `DiagnosticIntake`, plus usage, on the final
  `TaskContext`.

## Scope notes

- **Node count is fixed at one** — `IntakeExtractNode`, both start and terminal, no router, no
  dedicated setup node. It resolves its own worktree path from an upstream `SetupWorktreeNode`
  result if present in `ctx.nodes`, falling back to `std::env::current_dir()` otherwise.
- **Out of scope for this block** (owned by later Phase 4 blocks): company/prospecting research
  (EN.4.A, `research_agent`), proposal generation and scoring (EN.4.C, which imports
  `DiagnosticIntake` by name), PDF render (EN.4.D).
- **No embedding/pgvector/corpus writes** — per THE BOUNDARY TEST (`CLAUDE.md`), this workflow
  only acquires and reasons; a downstream `PersistToBrainNode` (not part of this block) would own
  handing extracted intake off to Synapse's ingest endpoint.
- **Hermetic test coverage**: `crates/engine-core/tests/diagnostic_intake_e2e.rs` drives
  `IntakeExtractNode` end-to-end against a stubbed transport, asserts `*_evidence` field integrity
  through an `EventsRow` round-trip, asserts `registry_for_policy`'s Local-tier rewire keeps the
  same node identity/count, and asserts dispatcher registration
  (`is_registered("DIAGNOSTIC_INTAKE")`); a `#[ignore]`-gated experiment harness exercises the
  full profile-resolve → run → persist → aggregate pipeline across all four named profiles.
