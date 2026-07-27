---
type: Reference
title: Research Agent Workflow
description: How the RESEARCH_AGENT workflow works — dual-mode graph (company brief vs. prospecting) terminating in a MaterializeDocNode write, event schema, tunable ResearchAgentPolicy, triggering, and reading outputs
doc_id: research-agent-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [research-agent, workflow, graph, policy, websearch, prospecting, company brief, materialize-doc-node, opportunity]
related: [architecture, sdlc-flow-workflow, sdlc-flow-policy, data-contract, materialize-doc-node, opportunity-edit-workflows]
---

# Research Agent Workflow

`RESEARCH_AGENT` (block EN.4.A) is a policy-aware, `WebSearch`-backed workflow with two exit
modes: a single-company research brief, or a broader prospecting sweep across a vertical. It is
a port and broadening of the Python `orchestrator`'s RESEARCH_AGENT, rebuilt on the `engine-core`
shared policy framework introduced in EN.4.0 (see [sdlc-flow-policy.md](sdlc-flow-policy.md) for
that framework's mechanics — this doc only covers how `RESEARCH_AGENT` configures and uses it).

Source: `crates/engine-core/src/workflows/research_agent/` (`mod.rs`, `schema.rs`, `policy.rs`,
`profiles.rs`, `company_research.rs`, `prospecting.rs`, `graph.rs`), registered from
`crates/engine-serve/src/workflows.rs` (`register_research_agent` → `register_builtin_workflows`).

## Graph shape

```
ResearchModeRouterNode -> { CompanyResearchNode | ProspectingResearchNode } -> MaterializeDocNode
```

Four nodes. `ResearchModeRouterNode` is the start node and a deterministic `Router`
that reads `event.mode` and routes to whichever terminal node matches — a `Router::route` takes
`&TaskContext` and cannot mutate it, so policy resolution and telemetry live in the two research
nodes instead, not in the router. Both research branches converge on a single shared
`MaterializeDocNode` instance (`EN.7.B`) — the graph's **only** exit point; neither
`CompanyResearchNode` nor `ProspectingResearchNode` is terminal anymore.

| Node | Kind | What it does |
|---|---|---|
| `ResearchModeRouterNode` | Deterministic router | Deserializes the event into `ResearchAgentEventSchema`; routes to `CompanyResearchNode` for `mode: "company"`, `ProspectingResearchNode` for `mode: "prospecting"`, or `None` for an invalid/malformed event. |
| `CompanyResearchNode` | **Model** (Sonnet by default, tunable via policy) | Wraps `ClaudeCodeStep` with `WebSearch`/`WebFetch` tools granted and a `CompanyBrief` `json_schema`. Resolves the run's `ResearchAgentPolicy`, applies research-stage tier/prompt-cache/verbosity shaping, parses the reply into a `CompanyBrief`, stamps it + usage onto `ctx`, and persists `research-agent-state.json`. |
| `ProspectingResearchNode` | **Model** (Sonnet by default, tunable via policy) | Same shape as `CompanyResearchNode` for the `prospect` stage: resolves policy, applies shaping, runs a `WebSearch`-backed sweep, parses a `ProspectingResult`, stamps `ctx` + usage, and persists `research-agent-state.json`. |
| `MaterializeDocNode` (`EN.7.B`) | Deterministic writer | Configured with model `"opportunity"` and an ordered `with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"])` preference — exactly one of the two runs per event, so it reads whichever is present. Writes/updates the resulting `CompanyBrief`/`ProspectingResult` into the Brain corpus as an Opportunity `.md` document via `mev`'s `plan_ingest` (kind auto-detected from the payload shape — no adapter node needed). See [materialize-doc-node.md](materialize-doc-node.md). |

### Behaviour change: a served run now requires a resolvable brain root

Before `EN.7.B`, a `RESEARCH_AGENT` run succeeded whether or not a Brain corpus was reachable —
the research nodes were the graph's exit points. **As of `EN.7.B`, a run now *ends* by writing**:
`MaterializeDocNode`'s brain root is left unset on the registered node, so it resolves via
`crate::brain_root::resolve_brain_root()` (`ENGINE_BRAIN_ROOT` env var, else walking up from the
process cwd for `brain.toml`) at run time. In an environment with no resolvable brain root, the
run now **fails loudly with a `NodeError`** where it previously succeeded. This is the block's
intended semantics, not a bug — there is deliberately no event field to opt out of the write; set
`ENGINE_BRAIN_ROOT` (or run from within the brain checkout) for any deployment that triggers
`RESEARCH_AGENT`.

`registry_for_policy(&ResearchAgentPolicy)` in `graph.rs` never rewires either stage to the
`local` model tier — both `research` and `prospect` are cloud-only `WebSearch`-backed stages that
a local single-shot endpoint cannot serve, unlike `sdlc_flow`'s `triage`/`review` stages which
can be. `LocalConfig` is still carried on `ResearchAgentPolicy` for API-shape parity with
`crate::policy::tier`, but no built-in default or named profile ever resolves either stage to
`ModelTier::Local`.

## Event schema (`ResearchAgentEventSchema`)

```json
{
  "mode": "company",
  "company_name": "Acme Corp",
  "company_url": "https://acme.example",
  "profile": "cheap-fast"
}
```

or

```json
{
  "mode": "prospecting",
  "vertical": "legal-tech",
  "topic": "contract review pain points",
  "policy": { "output_verbosity": "terse" }
}
```

| Field | Mode | Notes |
|---|---|---|
| `mode` | both, required | `"company"` \| `"prospecting"` — selects which terminal node the router dispatches to. |
| `company_name` / `company_url` | `company` | Optional inputs for the single-company brief. |
| `vertical` / `topic` | `prospecting` | Optional seed inputs narrowing the sweep. |
| `policy` | both, optional | Per-run `PartialResearchAgentPolicy` override — highest-precedence layer. |
| `profile` | both, optional | Name of a built-in or `harness.json`-defined policy profile bundle. |

All per-mode input fields are optional at the schema level (`Option<String>`) — `ResearchMode`
alone determines which subset a given run is expected to populate; the model nodes' prompts, not
serde, enforce that the right fields are present for the chosen mode.

## Structured outputs

- **`CompanyBrief`** (`CompanyResearchNode`): `company_name`, `summary`, `recent_developments`,
  `pain_points`, `outreach_hooks`, `sources`. Only `company_name`/`summary` are JSON-schema
  `required`; the rest tolerate partial model output.
- **`ProspectingResult`** (`ProspectingResearchNode`): `vertical`, `prospects` (a list of
  `ProspectLead { name, pain_points, pillar, outreach_hook, source }`, mapped onto one of the
  practice's four service pillars), `common_pain_points`, `sources`. Only `vertical` is
  JSON-schema `required`.

Both nodes set `Config.json_schema` on the underlying `claude_code_rs::Config` (via
`company_brief_json_schema()` / `prospecting_result_json_schema()` in `schema.rs`) and prefer the
model's pre-parsed structured output over fence-stripped text parsing, the same idiom
`sdlc_flow`'s model nodes use (see [Structured-output adoption](sdlc-flow-policy.md#structured-output-adoption)).

## Policy: `ResearchAgentPolicy`

Same four-layer precedence as `SdlcPolicy` — **per-run event `policy` override > per-run event
`profile` > `harness.json` `research_agent.policy` defaults > built-in default** — resolved via
the shared `crate::policy::resolve` framework (EN.4.0). Unlike `SetupWorktreeNode` in
`sdlc_flow` (there is no setup node here), and (as of `EN.5.D`) unlike either terminal node
resolving it for itself: `engine-serve::workflows::register_research_agent`'s `WorkflowFactory`
resolves policy once, at dispatch, via `profiles::resolve_policy_for_run_from(&event.data,
&PolicyConfigSource::Builtin)` (no repo checkout in hand at dispatch time) and seeds the result
into the run's initial `ctx.nodes`. Each terminal node reads that stamp with
`crate::policy::resolved_policy_strict(&ctx)` rather than re-resolving it.

Knobs (a strict subset of `SdlcPolicy`'s — only what the two stages need):

| Field | Values | What it controls |
|---|---|---|
| `output_verbosity` | `terse` \| `normal` \| `verbose` | Verbosity directive added to both model nodes' prompts. |
| `prompt_cache` | `bool` | Whether a stable system-prompt anchor is added for provider-side prompt caching. |
| `model_tiers.{research,prospect}` | `sonnet` \| `haiku` \| `opus` \| `local` | Per-stage model tier. Never actually resolves to `local` in practice — see [Graph shape](#graph-shape). |
| `local.{endpoint,model,constrained_json}` | string / string / bool | Carried for API-shape parity; not exercised by either stage. |

Built-in default: `ResearchAgentPolicy::default()` — normal verbosity, both tiers `sonnet`,
prompt cache off.

### Named profiles

Three built-in bundles in `profiles.rs` (`profile_by_name`), looked up first in
`planning/harness.json` → `research_agent.profiles[name]`, then in this built-in set:

| Name | Tradeoff |
|---|---|
| `baseline` | Explicit no-op control: Sonnet on both stages, normal verbosity, prompt cache off — spelled out for clarity, matches the built-in default. |
| `cheap-fast` | `haiku` on both stages, terse output, prompt caching on. |
| `thorough` | `opus` on both stages, verbose output. |

`planning/harness.json` carries a matching `research_agent.{policy,profiles}` section (mirroring
`sdlc.{policy,profiles}` — see [sdlc-flow-policy.md](sdlc-flow-policy.md#2-planningharnessjson--sdlcpolicy-this-repos-defaults)
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
  "workflow_type": "RESEARCH_AGENT",
  "data": { "mode": "company", "company_name": "Acme Corp", "profile": "cheap-fast" }
}
```

`GET /workflows` lists `RESEARCH_AGENT` once `register_research_agent`/`register_builtin_workflows`
has run; `GET /workflows/RESEARCH_AGENT/graph` returns the declared schema above.

## Reading outputs

- **`<worktree>/planning/research-agent-state.json`** — the telemetry record each terminal node
  persists on completion: `{mode, policy, telemetry}`, where `telemetry` is a
  `RunTelemetryInputs`-shaped harvest from the shared `crate::policy::telemetry` module (cost,
  tokens, model tier used). Both `CompanyResearchNode` and `ProspectingResearchNode` write the
  same shape, so a batch of these files can be fed to the shared
  `policy::aggregate_state_files` aggregator (see
  [sdlc-flow-policy.md](sdlc-flow-policy.md#aggregating-across-runs) for the aggregator's
  mechanics) to rank named profiles by cost.
- **`ctx.nodes["CompanyResearchNode"]` / `ctx.nodes["ProspectingResearchNode"]`** — the parsed
  `CompanyBrief` / `ProspectingResult`, plus usage, on the final `TaskContext`.
- **`ctx.nodes["MaterializeDocNode"]`** — the terminal write's result stamp:
  `{materialized, dry_run, model: "opportunity", paths, warnings}`, naming the Opportunity `.md`
  path written or updated under `business/docs/opportunities/`. See
  [materialize-doc-node.md § Result stamp](materialize-doc-node.md#result-stamp).

## Scope notes

- **Node count is fixed at four** (as of `EN.7.B`) — `ResearchModeRouterNode`,
  `CompanyResearchNode`, `ProspectingResearchNode`, `MaterializeDocNode`. There is no
  setup/worktree node; each research node resolves its own worktree path from an upstream
  `SetupWorktreeNode` result if present in `ctx.nodes`, falling back to
  `std::env::current_dir()` otherwise.
- **Out of scope for this block**: intake extraction (EN.4.B), PDF render (EN.4.D — not yet
  built). Proposal generation (EN.4.C, built) reuses `CompanyResearchNode`, re-exported from
  `workflows::research_agent` — see [proposal-generator-workflow.md](proposal-generator-workflow.md).
- **No embedding/pgvector/corpus writes** — per THE BOUNDARY TEST (`CLAUDE.md`), this workflow
  only acquires and reasons and writes the repo-tracked source `.md` document via `mev`
  in-process (`MaterializeDocNode`, D53's fourth boundary-test channel); Synapse still owns the
  derived index (embeddings, `brain_edges`, retrieval) over whatever gets written here — see
  [materialize-doc-node.md § Why this node exists](materialize-doc-node.md#why-this-node-exists-d53s-fourth-boundary-test-channel).
  Editing an already-written opportunity's `stage`/`actions[]` after this run completes is the
  separate `OPPORTUNITY_SET_STAGE` / `OPPORTUNITY_ADD_ACTION` micro-workflows — see
  [opportunity-edit-workflows.md](opportunity-edit-workflows.md).
- **Hermetic test coverage**: `crates/engine-core/tests/research_agent_e2e.rs` drives both modes
  end-to-end against a stubbed transport and a stubbed `MaterializeDocNode` (no real corpus write),
  asserts `registry_for_policy` never rewires to `local`, and asserts dispatcher registration
  (`is_registered("RESEARCH_AGENT")`); a `#[ignore]`-gated experiment harness exercises the full
  profile-resolve → run → persist → aggregate pipeline.
  `crates/engine-core/tests/opportunity_loop_e2e.rs` (`EN.7.B`) drives the real `Workflow::run`
  walk with the real `MevDocMaterializer` against a `tempfile::tempdir()` corpus, proving both
  research branches close the loop and the write is idempotent on re-run.
