---
type: Reference
title: Research Agent Workflow
description: How the RESEARCH_AGENT workflow works — dual-mode graph (company brief vs. prospecting) terminating in a MaterializeDocNode write, event schema, tunable ResearchAgentPolicy, triggering, and reading outputs
doc_id: research-agent-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [research-agent, workflow, graph, policy, websearch, prospecting, company brief, materialize-doc-node, opportunity, contacts, merge-contacts, contact-enrichment, locale, language directive]
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
ResearchModeRouterNode -> { CompanyResearchNode | ProspectingResearchNode } -> MaterializeDocNode -> MergeContactsNode
```

Five nodes. `ResearchModeRouterNode` is the start node and a deterministic `Router`
that reads `event.mode` and routes to whichever terminal node matches — a `Router::route` takes
`&TaskContext` and cannot mutate it, so policy resolution and telemetry live in the two research
nodes instead, not in the router. Both research branches converge on a single shared
`MaterializeDocNode` instance (`EN.7.B`), which in turn feeds a single shared `MergeContactsNode`
instance (`EN.4.E`) — the graph's **only** exit point; neither `CompanyResearchNode` nor
`ProspectingResearchNode` is terminal, and `MaterializeDocNode` is no longer terminal either. The
graph shape is **invariant across `contact_enrichment` policy depth** (see
[Policy: `ResearchAgentPolicy`](#policy-researchagentpolicy) below) — no run ever rewires around
`MergeContactsNode`; its own empty-contacts-list no-op is the only cost control.

| Node | Kind | What it does |
|---|---|---|
| `ResearchModeRouterNode` | Deterministic router | Deserializes the event into `ResearchAgentEventSchema`; routes to `CompanyResearchNode` for `mode: "company"`, `ProspectingResearchNode` for `mode: "prospecting"`, or `None` for an invalid/malformed event. |
| `CompanyResearchNode` | **Model** (Sonnet by default, tunable via policy) | Wraps `ClaudeCodeStep` with `WebSearch`/`WebFetch` tools granted and a `CompanyBrief` `json_schema`. Resolves the run's `ResearchAgentPolicy`, applies research-stage tier/prompt-cache/verbosity/contact-acquisition shaping, parses the reply into a `CompanyBrief`, deterministically stamps `company_url` from the trigger event when the model omitted it, stamps it + usage onto `ctx`, and persists `research-agent-state.json`. |
| `ProspectingResearchNode` | **Model** (Sonnet by default, tunable via policy) | Same shape as `CompanyResearchNode` for the `prospect` stage: resolves policy, applies shaping, runs a `WebSearch`-backed sweep, parses a `ProspectingResult`, stamps `ctx` + usage, and persists `research-agent-state.json`. |
| `MaterializeDocNode` (`EN.7.B`) | Deterministic writer | Configured with model `"opportunity"` and an ordered `with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"])` preference — exactly one of the two runs per event, so it reads whichever is present. Writes/updates the resulting `CompanyBrief`/`ProspectingResult` into the Brain corpus as an Opportunity `.md` document via `mev`'s `plan_ingest` (kind auto-detected from the payload shape — no adapter node needed), including the `company_url` -> `url` and `sources[]` -> `links[]` lifting (see [Source-link and contact lifting](#source-link-and-contact-lifting) below). See [materialize-doc-node.md](materialize-doc-node.md). |
| `MergeContactsNode` (`EN.4.E`) | Deterministic writer | The graph's terminal node. Configured with the same ordered `with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"])` preference as `MaterializeDocNode` and an unset brain root (resolves the same way). Collects the `contacts[]` the research node surfaced (per-brief for company mode, unioned across every `ProspectLead` for prospecting mode) and, when non-empty, calls the injectable `DocMaterializer::edit_opportunity` seam with `OpportunityEdit::MergeContacts` — routed by `MevDocMaterializer` to `mev::doc::opportunity::plan_merge_contacts`. An empty collected list short-circuits to a stamped no-op result with **no** seam call — see [Contacts: extraction contract and the two-step write](#contacts-extraction-contract-and-the-two-step-write). See [materialize-doc-node.md § `OpportunityEdit`](materialize-doc-node.md#the-docmaterializer-seam). |

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
| `locale` | both, optional | `Locale` (`"pt-BR"` \| `"en-US"`), defaults to `"pt-BR"` when omitted (`EN.4.F`). Drives the language `CompanyResearchNode`/`ProspectingResearchNode` write their prose in via `crate::locale::language_directive(locale)`, spliced into the per-run prompt body (never the `STABLE_SYSTEM_PROMPT`, so prompt caching is unaffected across locales). Both nodes stamp the resolved `locale` alongside their result onto `ctx` for telemetry. A per-client attribute, not a policy knob — it is not on `ResearchAgentPolicy`. |
| `policy` | both, optional | Per-run `PartialResearchAgentPolicy` override — highest-precedence layer. |
| `profile` | both, optional | Name of a built-in or `harness.json`-defined policy profile bundle. |

**Contact strings are never translated.** The language directive governs prose fields only —
`ResearchContact` values (email, phone, WhatsApp number, handle, URL) are scraped literals under
`EN.4.E`'s anti-fabrication contract, and the directive itself carries an explicit exception
telling the model to reproduce any literal contact string exactly as found, regardless of locale.

All per-mode input fields are optional at the schema level (`Option<String>`) — `ResearchMode`
alone determines which subset a given run is expected to populate; the model nodes' prompts, not
serde, enforce that the right fields are present for the chosen mode.

## Structured outputs

- **`CompanyBrief`** (`CompanyResearchNode`): `company_name`, `summary`, `recent_developments`,
  `pain_points`, `outreach_hooks`, `sources`, `contacts` (`Vec<ResearchContact>`), `company_url`.
  Only `company_name`/`summary` are JSON-schema `required`; the rest tolerate partial model
  output, including an empty `contacts[]`.
- **`ProspectingResult`** (`ProspectingResearchNode`): `vertical`, `prospects` (a list of
  `ProspectLead { name, pain_points, pillar, outreach_hook, source, contacts }`, mapped onto one
  of the practice's four service pillars), `common_pain_points`, `sources`. Only `vertical` is
  JSON-schema `required` at the top level, and only `name`/`pillar` are `required` per prospect.

Both nodes set `Config.json_schema` on the underlying `claude_code_rs::Config` (via
`company_brief_json_schema()` / `prospecting_result_json_schema()` in `schema.rs`) and prefer the
model's pre-parsed structured output over fence-stripped text parsing, the same idiom
`sdlc_flow`'s model nodes use (see [Structured-output adoption](sdlc-flow-policy.md#structured-output-adoption)).

### `ResearchContact` — the contact shape

`ResearchContact` (`schema.rs`) is shaped field-for-field like okf-core's `Contact`
(`../okf-core/src/doc/opportunity.rs`) so `MergeContactsNode` can hand it straight to mev's
`plan_merge_contacts` without a lossy remap:

```
{ "name": "", "role": "", "emails": [], "whatsapp": [], "phones": [], "links": [], "note": "" }
```

Both `company_brief_json_schema()` and `prospecting_result_json_schema()` embed the identical
`research_contact_json_schema()` sub-schema wherever `contacts` appears, and neither adds
`contacts` — nor any field of it, including `name` — to a `required` list. This sub-schema is
**invariant across `contact_enrichment` policy depth**: only the prompt text varies with depth,
never the emitted schema, so mev's `detect_kind` and okf-core's mapping stay stable regardless of
policy.

## Contacts: extraction contract and the two-step write

`EN.4.E` extends the workflow so a run produces **reachable contacts** and lifts its
`sources[]`/`company_url` into the materialized Opportunity, turning the previously hand-run `mev
doc opportunity merge-contacts` step into part of the automated graph.

### Anti-fabrication contract (load-bearing)

A hallucinated email sends real mail to a real stranger under Brandon's name. Both prompts and
every code path in this chain hold to one rule: **only ever report a contact channel that appeared
verbatim in a fetched source. Never construct or guess one.**

- "No contact found" is a success path, not an error — an empty `contacts[]` is valid at every
  JSON-schema level, and `MergeContactsNode` treats it as a clean no-op (see below), never a
  `NodeError`.
- No prompt text or code path synthesizes an address from a domain or a person's name — no
  `info@{domain}` guessing, no name+domain composition.
- A generic channel with no named human (`contato@`, a storefront WhatsApp number) is a **valid**
  contact, recorded with an empty `name` — not discarded.

**Acquisition and anti-fabrication compose, they do not compete.** At any depth above `off`, both
prompts carry an explicit *acquisition* directive — company mode names the contact-bearing
surfaces to visit (contact/about/team page, footer, `mailto:`/`wa.me` links, and at `deep` the
public LinkedIn/Instagram/Facebook profiles plus a named-decision-maker hunt); prospecting mode
makes one cheap enrichment attempt per identifiable business and skips pseudonymous posters. Effort
spent *searching* carries no fabrication risk — only the *reporting* step does, so both prompts
also state explicitly that omitting a contact is preferred over guessing one. A prompt that only
asks "were there any contacts in what you already fetched?" does not satisfy this contract; look
hard, report only what you saw.

### The two-step write

Contacts reach the written document through **two** graph steps, not one:

1. **`MaterializeDocNode`** writes/updates the Opportunity via mev's `plan_ingest` — the ingest
   mapping (okf-core's `Opportunity::from_company_brief`/`from_prospecting_result`) does **not**
   set `contacts` at all; only `url`/`links` (see below).
2. **`MergeContactsNode`** then merges the collected `contacts[]` into that same, already-written
   opportunity via `OpportunityEdit::MergeContacts`, which mev's `MevDocMaterializer` routes to
   `mev::doc::opportunity::plan_merge_contacts(slug, contacts, root)`.

Contacts go through mev's merge planner rather than the ingest mapping because merging requires
**conflict policy** the ingest step has no context for: `plan_merge_contacts` matches an incoming
contact against an existing one by `name`, unions `emails`/`whatsapp`/`phones`/`links`, and fills
`role`/`note` only when the existing value is empty — a re-run must not duplicate a contact or
clobber a human-edited note. That policy stays owned by mev, not duplicated here. Because
`plan_merge_contacts` loads an *existing* opportunity by slug, the merge must run strictly after
`MaterializeDocNode` has written (or confirmed) that document — which is exactly why
`MergeContactsNode` is wired downstream of it rather than folded into the same step.

**Slug derivation** for the merge call uses `okf_core::derive_slug` over the same title the ingest
mapping uses (`company_name` for company mode, `"{vertical} — Prospecting Sweep"` for prospecting)
— not `MaterializeDocNode`'s stamped `paths`, because mev's idempotency guard zero-stamps `paths`
when content is unchanged (see [materialize-doc-node.md](materialize-doc-node.md)), which would
lose the slug on a re-run.

**A zero-contact run is a success, not a partial failure.** When `MergeContactsNode` collects an
empty contact list — the normal outcome for most prospecting leads, and the *only* outcome at a
`contact_enrichment` depth of `off` — it stamps a no-op result and makes **no** call into the
`DocMaterializer` seam at all. The opportunity `MaterializeDocNode` already wrote stands untouched.

### Source-link and contact lifting

Two lifts happen at different points in the chain, on different sides of the repo boundary:

| Lift | Where | Owned by |
|---|---|---|
| `company_url` -> `url` | `Opportunity::from_company_brief` | okf-core (`../okf-core/src/doc/opportunity.rs`) |
| `sources[]` -> `links[]` | `Opportunity::from_company_brief` / `from_prospecting_result` | okf-core |
| `contacts[]` -> merged `contacts:` | `mev::doc::opportunity::plan_merge_contacts` | mev, via `MergeContactsNode`/`OpportunityEdit::MergeContacts` |

The `url`/`links` lifting lives in okf-core's mapping — not forked into a repo-local copy — because
mev's `mev doc opportunity ingest` CLI path calls the same `Opportunity::from_company_brief`/
`from_prospecting_result` functions via `plan_ingest`; putting the lift there means the CLI path
and the automated `RESEARCH_AGENT` write get it for free, identically. Both mappings are
order-stable and deduped, and both leave every previously-mapped field (`title`, `description`,
`kind`, `stage`, `layer`, `research_brief`) unchanged. `CompanyResearchNode` deterministically
stamps `company_url` onto the parsed `CompanyBrief` from the trigger event when the model omitted
it, so `url` lifting does not depend on the model choosing to echo the field back.

`crates/engine-core/src/nodes/doc_materializer.rs`'s `OpportunityEdit` enum carries the third
variant this block adds:

```rust
pub enum OpportunityEdit {
    SetStage { slug: String, stage: String },
    AddAction { slug: String, /* .. */ },
    MergeContacts { slug: String, contacts: Vec<Contact> },
}
```

`MevDocMaterializer` routes `MergeContacts` to `mev::doc::opportunity::plan_merge_contacts`;
`StubDocMaterializer` records it the same way it records `SetStage`/`AddAction`, for tests. See
[materialize-doc-node.md](materialize-doc-node.md#the-docmaterializer-seam) for the seam's full
shape.

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
| `contact_enrichment.{research,prospect}` | `off` \| `standard` \| `deep` | Per-stage contact-acquisition depth (`EN.4.E`) — see below. |
| `contact_enrichment.max_fetches` | `u8` | Cap on the EXTRA page loads spent on contact acquisition per run. |

Built-in default: `ResearchAgentPolicy::default()` — normal verbosity, both tiers `sonnet`,
prompt cache off, `contact_enrichment` `standard`/`standard`/4 fetches.

### The `contact_enrichment` knob

Contact acquisition spends real fetches and real latency beyond what a run would otherwise spend,
so — per `CLAUDE.md` standing rule 6 — it resolves through the same four-layer precedence as every
other knob on this policy, never a hardcoded number or directive baked into a prompt string:

| Depth | What it does |
|---|---|
| `off` | Restores pre-`EN.4.E` behavior exactly: no contact-acquisition directive is added to either prompt at all. The schema still carries `contacts`; a run simply reports none. |
| `standard` | Directs the run to visit the company's own contact-bearing surfaces: contact/about/team page, footer, `mailto:`/`wa.me` links. |
| `deep` | Everything `standard` does, plus a sweep of public LinkedIn/Instagram/Facebook profiles and a named-decision-maker hunt. |

`max_fetches` caps the number of *extra* page loads (beyond the run's normal search/fetch
activity) spent on contact acquisition. The depth only ever changes the *prompt* — the emitted
JSON schema always describes `contacts` identically at every depth (see
[`ResearchContact` — the contact shape](#researchcontact--the-contact-shape)), so `detect_kind` and
the okf-core mapping are stable regardless of setting, and the declared graph shape never rewires
around `MergeContactsNode` either (see [Graph shape](#graph-shape)).

**The resolved depth is stamped** into each research node's result on `ctx.nodes`, so `EN.4.0`
telemetry can attribute per-run acquisition cost to the setting that caused it, the same way
`model_tiers` is attributed.

**A run resolved to `off` behaves exactly as the pre-`EN.4.E` workflow did**: it still writes its
opportunity with `url`/`links` lifted via `MaterializeDocNode`, and still walks
`MergeContactsNode` — which finds an empty collected `contacts[]` and no-ops cleanly, making no
seam call (see [Contacts: extraction contract and the two-step write](#contacts-extraction-contract-and-the-two-step-write)).

### Named profiles

Three built-in bundles in `profiles.rs` (`profile_by_name`), looked up first in
`planning/harness.json` → `research_agent.profiles[name]`, then in this built-in set:

| Name | Tradeoff |
|---|---|
| `baseline` | Explicit no-op control: Sonnet on both stages, normal verbosity, prompt cache off, `standard`/`standard` contact enrichment at 4 fetches — spelled out for clarity, matches the built-in default. |
| `cheap-fast` | `haiku` on both stages, terse output, prompt caching on, contact enrichment `off`/`off` at 0 fetches. |
| `thorough` | `opus` on both stages, verbose output, contact enrichment `deep` for `research`/`standard` for `prospect` at 8 fetches — prospecting deliberately stays `standard` even here so a broad sweep never multiplies deep enrichment across dozens of leads. |

**The cost story, stated explicitly.** Contact acquisition is extra spend on top of a run's normal
search/fetch activity: `cheap-fast` turns it off entirely (0 extra fetches, matching its
cost-floor intent for every other knob on this policy); `baseline` spends a moderate, capped
amount (`standard` depth, 4 fetches) on both stages; `thorough` spends the most, but only on the
`research` stage (`deep`, 8 fetches) — `prospect` stays capped at `standard` so a wide prospecting
sweep across dozens of leads cannot silently multiply the most expensive acquisition depth across
every one of them. Per-run overrides (`policy.contact_enrichment` or a custom `harness.json`
profile) can set any combination directly.

| | `research` (company) | `prospect` | `max_fetches` |
|---|---|---|---|
| built-in default | `standard` | `standard` | 4 |
| `baseline` profile | `standard` | `standard` | 4 |
| `cheap-fast` profile | `off` | `off` | 0 |
| `thorough` profile | `deep` | `standard` | 8 |

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
- **`ctx.nodes["MaterializeDocNode"]`** — the ingest write's result stamp:
  `{materialized, dry_run, model: "opportunity", paths, warnings}`, naming the Opportunity `.md`
  path written or updated under `business/docs/opportunities/`. See
  [materialize-doc-node.md § Result stamp](materialize-doc-node.md#result-stamp).
- **`ctx.nodes["MergeContactsNode"]`** — the terminal contact-merge's result stamp, mirroring
  `MaterializeDocNode`'s shape. On the normal zero-contact path this reports a stamped no-op (no
  `DocMaterializer` call made); when contacts were collected, it reports the same
  `{materialized, dry_run, paths, warnings}` shape returned by the `OpportunityEdit::MergeContacts`
  seam call.

## Scope notes

- **Node count is fixed at five** (as of `EN.4.E`) — `ResearchModeRouterNode`,
  `CompanyResearchNode`, `ProspectingResearchNode`, `MaterializeDocNode`, `MergeContactsNode`.
  There is no setup/worktree node; each research node resolves its own worktree path from an
  upstream `SetupWorktreeNode` result if present in `ctx.nodes`, falling back to
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
  `crates/engine-core/tests/research_agent_contacts_e2e.rs` (`EN.4.E`) extends this to the
  contact-merge step: it drives the real `Workflow::run` walk (real `MevDocMaterializer`, real
  `okf-core` parsing of the written `.md` back into an `Opportunity` — never raw string matching)
  against a `tempfile::tempdir()` corpus for both modes, proving `company_url` -> `url`,
  `sources[]` -> `links[]`, and each surfaced contact land in the written document; that a
  second identical run is idempotent (no duplicate contact, no duplicate link, and a
  same-name/second-channel contact unions rather than duplicating); that a zero-contact run
  writes the doc and skips the merge call entirely; and that a `cheap-fast` (`contact_enrichment`
  `off`) run still writes `url`/`links` and walks `MergeContactsNode` as a clean no-op.
