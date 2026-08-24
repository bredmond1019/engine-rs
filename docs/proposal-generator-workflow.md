---
type: Reference
title: Proposal Generator Workflow
description: How the PROPOSAL_GENERATOR workflow works — the seven-node research-to-persist graph, event schema, tunable ProposalGeneratorPolicy, the engine-brain persist boundary, triggering, and reading outputs
doc_id: proposal-generator-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [proposal-generator, workflow, graph, policy, automation-roadmap, persist-to-brain, http-post, review-router, locale, rate card, firewall, investment, authored_locale]
related: [architecture, research-agent-workflow, diagnostic-intake-workflow, sdlc-flow-policy, data-contract, D9-engine-brain-boundary]
---

# Proposal Generator Workflow

`PROPOSAL_GENERATOR` (block EN.4.C) is a policy-aware, seven-node workflow that turns a
company name (plus, optionally, an `EN.4.B` diagnostic-intake evidence contract) into a
client-facing `AutomationRoadmap` — scored, ranked, drafted, reviewed, and POSTed to the
company brain (Synapse) as durable knowledge. It is built on the EN.4.0 shared policy
framework (see [sdlc-flow-policy.md](sdlc-flow-policy.md) for that framework's mechanics —
this doc only covers how `PROPOSAL_GENERATOR` configures and uses it) and reuses
`research_agent`'s `CompanyBrief` type and `diagnostic_intake`'s `DiagnosticIntake` type as
upstream inputs.

Source: `crates/engine-core/src/workflows/proposal_generator/` (`mod.rs`, `schema.rs`,
`policy.rs`, `profiles.rs`, `company_research.rs`, `opportunity_identifier.rs`, `writer.rs`,
`review.rs`, `review_router.rs`, `revise.rs`, `persist_to_brain.rs`, `graph.rs`), registered
from `crates/engine-serve/src/workflows.rs` (`register_proposal_generator` →
`register_builtin_workflows`). The injectable HTTP-POST seam `PersistToBrainNode` uses lives
in `crates/engine-core/src/nodes/http_post.rs`.

## Graph shape

```
ProposalCompanyResearchNode -> OpportunityIdentifierNode -> ProposalWriterNode
  -> ProposalReviewNode -> ProposalReviewRouterNode
  -> { PersistToBrainNode
     | ProposalReviseNode -> {loop guard/increment cluster}
         -> ProposalReviewNode (continue, capped) | PersistToBrainNode (cap reached) }
```

Nine nodes: the original seven plus a `{guard, increment}` pair (`EN.5.E` task 4) built from
`crate::loop_combinator::build_loop` (see [architecture.md](architecture.md#core-types)).
`ProposalReviewRouterNode` is a deterministic `Router` that reads `ProposalReviewNode`'s stored
verdict off `ctx.nodes` and routes to whichever branch matches — a `Router::route` takes
`&TaskContext` and cannot mutate it, so policy resolution and telemetry live in the model nodes
it routes between, not in the router itself. `ProposalReviseNode`'s single declared connection
now points at the loop cluster's guard identity rather than straight to `PersistToBrainNode`:
the guard's back-edge (via the increment node) re-enters `ProposalReviewNode` for another review
pass, capped at `REVISE_LOOP_MAX_ITERATIONS` (3, in `graph.rs`). Once `ProposalReviewNode`
re-runs, `ProposalReviewRouterNode` re-decides — a `pass` verdict routes straight to
`PersistToBrainNode` (bypassing the loop cluster entirely; this is how the loop exits on a pass
verdict), while a further `revise` verdict re-enters the cluster, up to the cap, which then forces
an exit straight to `PersistToBrainNode` regardless of verdict. `PersistToBrainNode` is the sole
terminal node — no forward connection.

| Node | Kind | What it does |
|---|---|---|
| `ProposalCompanyResearchNode` | **Model** (Sonnet by default, cloud-only) | `research`-stage entry node; wraps `ClaudeCodeStep` with `WebSearch` tools granted, producing a `CompanyBrief` (reused from `workflows::research_agent`). |
| `OpportunityIdentifierNode` | **Model** (Sonnet by default, Local-eligible) | `opportunity`-stage node. Scores automation candidates from `DiagnosticIntake` evidence when present on the event, else falls back to the upstream web brief. Recomputes each candidate's composite score and `PriorityTier` deterministically from the model's raw axis scores (never trusts the model's own arithmetic), sorts composite-descending, and stamps `{"candidates": [...]}` onto `ctx`. |
| `ProposalWriterNode` | **Model** (Sonnet by default, cloud-default) | `writer`-stage node. Drafts the four-section `AutomationRoadmap` from `OpportunityIdentifierNode`'s ranked candidates and `ProposalCompanyResearchNode`'s brief. |
| `ProposalReviewNode` | **Model** (Sonnet by default, Local-eligible) | `review`-stage node. Reviews the draft and stores a `pass`/`revise` verdict on `ctx`. When `ProposalGeneratorPolicy.review_mode == Skip`, short-circuits straight to `pass` before constructing a `ClaudeCodeStep` at all — zero model calls under `Skip`. |
| `ProposalReviewRouterNode` | Deterministic router | Reads `ProposalReviewNode`'s verdict; routes `pass` -> `PersistToBrainNode`, `revise` -> `ProposalReviseNode`. Fails closed to `revise` for any ambiguous/malformed verdict text. Its upstream/downstream identities are resolved through `InputBinding` values (not literal struct fields), since the e2e test constructs it as a bare `ProposalReviewRouterNode` unit struct. |
| `ProposalReviseNode` | **Model** (Sonnet by default, Local-eligible) | `revise`-stage node. Reads both the writer's draft and the reviewer's notes from `ctx.nodes` (via `InputBinding`, `EN.5.E`) to produce a corrected, validator-passing `AutomationRoadmap` under its own node identity. Its single declared connection points at the review/revise loop cluster's guard, not directly at `PersistToBrainNode` (see [Graph shape](#graph-shape)). |
| loop cluster guard/increment (`ProposalRevisionGuard`/`ProposalRevisionIncrement`, identity-derived) | Deterministic routers | `crate::loop_combinator::build_loop`'s cap-enforcing back-edge pair (`EN.5.E`): the guard routes back to `ProposalReviewNode` (continue, under `REVISE_LOOP_MAX_ITERATIONS`) or to `PersistToBrainNode` (cap reached); the increment node owns the iteration counter. |
| `PersistToBrainNode` | Deterministic, terminal | POSTs the finished roadmap — preferring `ProposalReviseNode`'s corrected draft over `ProposalWriterNode`'s original when present — to Synapse's brain-ingest endpoint over an injectable `HttpPost` seam. See [The engine↔brain persist boundary](#the-enginebrain-persist-boundary). |

`registry_for_policy(&ProposalGeneratorPolicy)` in `graph.rs` rewires whichever of the three
Local-eligible stages — `opportunity`, `review`, `revise` — the policy resolves to
`ModelTier::Local`, routing through `openai_compat_meta_transport_live` via each node's shared
`TransportSlot`/`with_meta_transport` (falling back to the real `claude` CLI transport on any
local-endpoint failure) so `model_tier_used` telemetry reflects the tier that actually ran, not
just the resolved policy's intent (`EN.ticket.wire-meta-transport-telemetry` task 4). It **never**
rewires `research`
(`ProposalCompanyResearchNode` wraps `ClaudeCodeStep` with `WebSearch`/`WebFetch` tools
granted, which a local single-shot endpoint cannot serve) or `writer` (cloud-default, no
`Local` dispatch branch exists for it at all) — this holds even if a policy sets every tier,
including `research`, to `Local`.

## Event schema (`ProposalGeneratorEventSchema`)

```json
{
  "company_name": "Acme Corp",
  "company_url": "https://acme.example",
  "diagnostic_intake": { "...": "an EN.4.B DiagnosticIntake, when a call already happened" },
  "locale": "pt-BR",
  "profile": "local-judgment"
}
```

| Field | Notes |
|---|---|
| `company_name` | Required. Seeds `ProposalCompanyResearchNode`'s web research and echoes into the deliverable's Section 1. |
| `company_url` | Optional. |
| `diagnostic_intake` | Optional `DiagnosticIntake` (re-exported from `workflows::diagnostic_intake`). When present, `OpportunityIdentifierNode` scores candidates from its `*_evidence` fields (`rubric.md §1`); when absent, it falls back to the web research brief. |
| `locale` | Optional `Locale` (`"pt-BR"` \| `"en-US"`), defaults to `"pt-BR"` when omitted (`EN.4.F`). Selects both the language `ProposalWriterNode`/`ProposalReviseNode` write their prose in and which `RateCard` sheet prices `FirstEngagement.investment` — see [Locale and the firewalled rate card](#locale-and-the-firewalled-rate-card). A per-client attribute, not a cost/latency/quality tradeoff, so it lives here on the event schema rather than on `ProposalGeneratorPolicy` (`CLAUDE.md` rule 6). |
| `policy` | Optional per-run `PartialProposalGeneratorPolicy` override — highest-precedence layer. |
| `profile` | Optional name of a built-in or `harness.json`-defined policy profile bundle. |

## Structured output: `AutomationRoadmap`

Four sections, mirroring `deliverable.md §2`:

1. **`SituationAndOpportunity`** — `company_name`, `business_type`, `team_size`,
   `painful_workflow_summary`, `candidate_count`.
2. **`RankedCandidate` list** — one row per scored automation candidate: `name`, `frequency`,
   `time_cost`, `buildability` (each 1-5 per `rubric.md`'s anchors), `composite`
   (`frequency*0.35 + time_cost*0.40 + buildability*0.25`), `tier` (`PriorityTier`), and a
   1-2 sentence `rationale`.
3. **`WorkflowProfile` list** — at most `MAX_TOP_PROFILES` (3) per-workflow detail pages:
   `name`, `today`, `proposed_solution`, `stack`, `rough_scope`, `expected_roi`.
4. **`FirstEngagement`** — `start_with` (the highest-scoring Quick Win, or the highest Core
   Build if there is no Quick Win, per `rubric.md §4`), `phase_1_scope`, `investment` (see
   [Locale and the firewalled rate card](#locale-and-the-firewalled-rate-card)), `how_it_works`,
   `call_to_action`.

`AutomationRoadmap` also carries `authored_locale: Locale` (`EN.4.F`) — the locale this
roadmap's prose was actually written in, distinct from the locale a later run might request;
`EN.4.D`'s renderer compares the two and refuses on a mismatch rather than emitting a
mixed-language document. Defaults to `Locale::PtBr` when absent, so a pre-`EN.4.F` roadmap
round-trips unchanged.

`PriorityTier` (`schema.rs`) is derived from the composite score, not asserted by the model:
`>= 4.0` → `QuickWin`, `2.5..=3.9` → `CoreBuild`, `< 2.5` → `Phase2`. `schema.rs` also carries
composite/sort/`≤3`-profile validators (`validate_composite_scores`,
`validate_candidates_sorted`, `validate_top_profiles_count`, `validate_automation_roadmap`)
and `automation_roadmap_json_schema()`, set on the underlying `claude_code_rs::Config` by
`ProposalWriterNode`/`ProposalReviseNode` the same way `research_agent`'s nodes set
`company_brief_json_schema()`.

## Locale and the firewalled rate card

`EN.4.F` replaces model-authored pricing with structured config the engine — not the model —
resolves. `FirstEngagement.investment` (`type Investment = crate::locale::MoneyRange`) is a
`{ currency, min, max, basis }` object, never a free-text string, and `ProposalWriterNode` /
`ProposalReviseNode` populate it deterministically:

```rust
let rate_card = RateCard::load_from(&PolicyConfigSource::Builtin)?;
let investment = investment_for(EngagementKind::from(&roadmap), rate_card.sheet(event.locale));
```

`RateCard::sheet(locale)` is the **only** accessor — there is deliberately no method that returns
or converts between both sheets. `RateCard::load_from` reads the `rate_card` section of
`planning/harness.json` (see `Notes` there for the ported `business/docs/rates.md` figures and
their provenance caveats); an absent file/section resolves to `RateCard::default()` (the same
figures, hardcoded as a behavior-stable fallback), and a *present but malformed* section is a
hard error rather than a silent fallback — once a human starts editing the section, this crate
trusts it or fails loudly.

**The firewall invariant.** The two sheets (`pt_br`/BRL, `en_us`/USD) are "never quoted in the
same conversation, never cross-converted" (`business/docs/rates.md`). `crate::locale` defines
**no** conversion between `Currency::Brl` and `Currency::Usd` anywhere — no rate constant, no
helper function, no test — and `RateSheet::validate()` rejects a rate card whose ranges don't all
carry their own sheet's currency. A future contributor asked to "show both currencies in one
proposal" should look here first: the absence of a conversion path is the feature, not an
oversight. `grep -rniE '(brl.*(to|into).*usd|usd.*(to|into).*brl|exchange_rate|convert_currency)'
crates/` staying empty is this invariant's human-runnable check (see the spec's Validation
Commands).

**Language, not just currency.** `event.locale` also drives the language the model writes all
prose in, via `crate::locale::language_directive(locale)` — spliced into the per-run prompt
*body* passed to `ClaudeCodeStep`, never into `ProposalWriterNode`'s `STABLE_SYSTEM_PROMPT` (which
stays byte-identical across locales, so provider-side prompt caching still hits regardless of
which locale a run requests — `CLAUDE.md` rule 6's cache-breakpoint clause). The directive's
contact carve-out is load-bearing: `EN.4.E`'s anti-fabrication contract means contacts are scraped
literals, and a translated email address or phone number is a wrong one.

`AutomationRoadmap.authored_locale` is stamped from `event.locale` **after** parsing the model's
structured reply — deterministically overwriting whatever the model itself emitted for that
field, mirroring how `investment` is engine-computed rather than model-authored. This closes the
loop for `EN.4.0` telemetry: a `RunTelemetry`/`PolicyAggregate` consumer can attribute observed
prose-language and pricing-currency cost/quality to the locale that actually caused it.

## Policy: `ProposalGeneratorPolicy`

Same four-layer precedence as `SdlcPolicy`/`ResearchAgentPolicy` — **per-run event `policy`
override > per-run event `profile` > `harness.json` `proposal_generator.policy` defaults >
built-in default** — resolved via the shared `crate::policy::resolve` framework. There is no
setup node, and (as of `EN.5.D`) no model node resolves policy for itself either:
`engine-serve::workflows::register_proposal_generator`'s `WorkflowFactory` resolves policy once,
at dispatch, via `profiles::resolve_policy_for_run_from(&event.data,
&PolicyConfigSource::Builtin)` (no repo checkout in hand at dispatch time) and seeds the result
into the run's initial `ctx.nodes`. Each model node reads that stamp with
`crate::policy::resolved_policy_strict(&ctx)` rather than re-resolving it.

Knobs:

| Field | Values | What it controls |
|---|---|---|
| `output_verbosity` | `terse` \| `normal` \| `verbose` | Verbosity directive added to model nodes' prompts. |
| `prompt_cache` | `bool` | Whether a stable system-prompt anchor is added for provider-side prompt caching. |
| `model_tiers.{research,opportunity,writer,review,revise}` | `sonnet` \| `haiku` \| `opus` \| `local` | Per-stage model tier. `research`/`writer` never actually resolve to `local` in practice — see [Graph shape](#graph-shape). |
| `local.{endpoint,model,constrained_json}` | string / string / bool | Local-endpoint config, applied when `opportunity`/`review`/`revise` resolve to `ModelTier::Local`. |
| `review_mode` | `full` \| `skip` | `full` (default) runs the review model call and honors its verdict; `skip` short-circuits `ProposalReviewNode` straight to `pass` with zero model calls, bypassing the revise branch entirely. |

Built-in default: `ProposalGeneratorPolicy::default()` — normal verbosity, all five tiers
`sonnet`, prompt cache off, `review_mode = Full`.

### Named profiles

Three built-in bundles in `profiles.rs` (`profile_by_name`), looked up first in
`planning/harness.json` → `proposal_generator.profiles[name]`, then in this built-in set:

| Name | Tradeoff |
|---|---|
| `baseline` | Explicit no-op control — all five tiers Sonnet, normal verbosity, prompt cache off, `review_mode = Full` — spelled out for clarity, matches the built-in default. |
| `local-judgment` | Rewires the three Local-eligible stages (`opportunity`, `review`, `revise`) to `ModelTier::Local`; leaves `output_verbosity`/`prompt_cache`/`writer`/`research` untouched (`None`) so it composes cleanly with other override layers. |
| `skip-review` | `review_mode = Skip` — no reviewer model call, roadmap persists straight from `ProposalWriterNode`'s draft. |

`planning/harness.json` carries a matching `proposal_generator.{policy,profiles}` section
(mirroring `sdlc.{policy,profiles}` — see
[sdlc-flow-policy.md](sdlc-flow-policy.md#2-planningharnessjson--sdlcpolicy-this-repos-defaults)
for the reader/precedence mechanics, identical here).

## The engine↔brain persist boundary

`PersistToBrainNode` (`persist_to_brain.rs`) is where this workflow crosses THE BOUNDARY TEST
(`CLAUDE.md`) — see
[`planning/decisions/D9-engine-brain-boundary.md`](../planning/decisions/D9-engine-brain-boundary.md)
for the full record. It builds

```json
{
  "artifact_id": "<fresh UUID v4, minted per persist call>",
  "company_name": "...",
  "doc_type": "automation_roadmap",
  "section": "full",
  "content": "<plain-language summary rendered from the roadmap>",
  "roadmap": { "...": "the full structured AutomationRoadmap, incl. authored_locale and a structured investment (EN.4.F)" }
}
```

and awaits an injectable `crate::nodes::http_post::HttpPost` seam (an `async_trait` object,
`Arc<dyn HttpPost>` — production code uses the `reqwest`-backed `http_post_live`; the gated
`cargo test` suite injects a `StubHttpPost` that records the last `(url, payload)` pair, so no
live network call happens in tests) to POST the payload. Non-2xx responses surface as a
`NodeError` — there is no fallback target for a failed brain push. On success it stamps
`{"posted": true, "status", "artifact_id", "response"}` onto `ctx`.

**Wired to the real endpoint (`EN.6.K`).** `PersistToBrainNode` no longer POSTs to a hardcoded
placeholder. The URL/`X-API-Key` header are resolved from `crate::nodes::brain_client::BrainConfig`
(`BrainConfig::from_env`, reading `BRAIN_API_URL`/`BRAIN_API_KEY`) joined with the fixed path
`/ingest/proposal` — the canonical target is Synapse's `POST /ingest/proposal` (brain block
`OR.Q`, pinned in [data-contract.md](data-contract.md)), which matches this payload shape exactly
and returns `200 {artifact_id, chunks_written}`. `ProposalGeneratorPolicy` still carries no
endpoint knob; `with_url(...)` and `with_config(...)` exist alongside `with_http_post(...)` so
tests and future callers can override the target without touching env.

Per THE BOUNDARY TEST, this node only POSTs — no embedding model is loaded, no `pgvector`
connection is opened, and no corpus table is written from this repo. What happens behind the
ingest endpoint (embedding the roadmap, writing the `BrainDocument` row, updating
`brain_edges`, retrieval-quality maintenance) is entirely Synapse's concern.

## How to trigger a run

Same HTTP surface as every other `engine-serve` workflow (`docs/cli.md`; see
[sdlc-flow-workflow.md](sdlc-flow-workflow.md#how-to-trigger-a-run) for the full auth/mounting
story):

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "PROPOSAL_GENERATOR",
  "data": { "company_name": "Acme Corp", "profile": "local-judgment" }
}
```

`GET /workflows` lists `PROPOSAL_GENERATOR` once
`register_proposal_generator`/`register_builtin_workflows` has run;
`GET /workflows/PROPOSAL_GENERATOR/graph` returns the declared schema above.

## Reading outputs

- **`ctx.nodes["OpportunityIdentifierNode"]`** — `{"candidates": [...]}`, the sorted, scored
  `RankedCandidate` list.
- **`ctx.nodes["ProposalWriterNode"]` / `ctx.nodes["ProposalReviseNode"]`** — the drafted /
  corrected `AutomationRoadmap`, plus usage.
- **`ctx.nodes["PersistToBrainNode"]`** — `{"posted": true, "status", "artifact_id",
  "response"}`, the brain-push result.

This workflow has no dedicated `proposal-generator-state.json` telemetry writer of its own —
unlike `research_agent`'s/`diagnostic_intake`'s terminal nodes, no `PROPOSAL_GENERATOR` node
persists a state file on completion (a `#[ignore]`-gated experiment harness in the e2e test
assembles a policy/telemetry/review_verdict/revised snapshot itself from the driven
`TaskContext` for aggregation purposes, but production runs do not write one).

## Scope notes

- **Node count is nine** as of `EN.5.E` — the original seven (`ProposalCompanyResearchNode`,
  `OpportunityIdentifierNode`, `ProposalWriterNode`, `ProposalReviewNode`,
  `ProposalReviewRouterNode`, `ProposalReviseNode`, `PersistToBrainNode`) plus the review/revise
  loop cluster's guard and increment nodes (`crate::loop_combinator::build_loop`). There is no
  setup/worktree node.
- **Reuses upstream types rather than redefining them**: `CompanyBrief` from
  `workflows::research_agent` (`ProposalCompanyResearchNode`'s output, and
  `OpportunityIdentifierNode`'s fallback input), `DiagnosticIntake` from
  `workflows::diagnostic_intake` (`OpportunityIdentifierNode`'s preferred input when present
  on the event).
- **No embedding/pgvector/corpus writes** — per THE BOUNDARY TEST (`CLAUDE.md`), this workflow
  only acquires, reasons, and POSTs; see
  [The engine↔brain persist boundary](#the-enginebrain-persist-boundary).
- **Out of scope for this block**: PDF render (EN.4.D — not yet built).
- **Hermetic test coverage**: `crates/engine-core/tests/proposal_generator_e2e.rs` drives the
  full nine-node chain (including the `EN.5.E` loop cluster) through both router branches (`pass`
  and `revise`) against stubbed
  transports + `HttpPost`, verifying `EventsRow` round-tripping, deliverable validators,
  evidence-vs-brief-fallback scoring, the `PersistToBrainNode` payload shape,
  `registry_for_policy`'s Local rewiring (and that `research`/`writer` are never rewired), and
  dispatcher registration (`is_registered("PROPOSAL_GENERATOR")`); a `#[ignore]`-gated
  experiment harness aggregates a self-assembled `proposal-generator-state.json` snapshot.
