---
type: Reference
title: LinkedIn Post Workflow
description: How the LINKEDIN_POST workflow works — reading real fleet work (git/log/decisions) into traceable draft candidates, the brand-rubric self-critic loop, event schema, tunable LinkedInPostPolicy, triggering, and reading outputs
doc_id: linkedin-post-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [linkedin-post, workflow, graph, policy, traceability, brand-critic, work-source, self-critic]
related: [architecture, content-pipeline-workflow, sdlc-flow-policy, data-contract]
---

# LinkedIn Post Workflow

`LINKEDIN_POST` (block `EN.5.G`) is a policy-aware workflow that turns a date range of **real
fleet work** — git commits, `log.md` entries, and new `planning/decisions/` files — into drafted
LinkedIn post candidates, each gated through a brand-rubric self-critic loop before it is
considered done. It is built on the shared `EN.4.0` policy framework (see
[sdlc-flow-policy.md](sdlc-flow-policy.md) for that framework's mechanics — this doc only covers
how `LINKEDIN_POST` configures and uses it).

Source: `crates/engine-core/src/workflows/linkedin_post/` (`mod.rs`, `schema.rs`, `work_source.rs`,
`policy.rs`, `profiles.rs`, `draft.rs`, `brand_critic.rs`, `revise.rs`, `graph.rs`), registered
from `crates/engine-serve/src/workflows.rs` (`register_linkedin_post` →
`register_builtin_workflows`).

## What this page is for

You are here to trigger a `LINKEDIN_POST` run, tune what it costs or how hard it tries, or read a
finished run's output. The sections below cover, in order: what the graph actually does
(traceability, the critic loop), the event you POST to start a run, the policy knobs and named
profiles, how to trigger a run, and where to read the result.

## Quickstart

Trigger a run over the same `engine-serve` HTTP surface every other workflow uses (see
[How to trigger a run](#how-to-trigger-a-run) below for the full request):

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "LINKEDIN_POST",
  "data": { "since": "2026-08-17", "until": "2026-08-24" }
}
```

Then read the result off `ctx.nodes` for the run (see [Reading outputs](#reading-outputs)) — this
workflow writes no dedicated state file of its own.

## Traceability is a type invariant, not a prompt instruction

The block's core requirement — every drafted post must trace to real work, never a fabricated
claim — is enforced at the type level, not by asking the model nicely. A
`PostCandidate` (`crates/engine-core/src/workflows/linkedin_post/schema.rs`) with an empty
`sources` field **fails to deserialize**: `Deserialize` is hand-implemented over a private shadow
type and rejects the value with an error naming the traceability requirement. A model reply that
claims compliance in prose but supplies no sources cannot become a value of this type.

A `WorkSource` (same file) is one real-work
artifact a candidate points back to — a `commit`, a `log-entry`, or a `decision`
(`WorkSourceKind`, kebab-case on the wire).

## Graph shape

```
WorkSourceNode -> PostDraftNode -> PostCandidateSelectNode -> BrandCriticNode -> CriticRouterNode
  -> { TranslateNode | IncrementCriticIterationNode -> ReviseNode -> BrandCriticNode }  // back-edge
```

Eight nodes: `WorkSourceNode`, `PostDraftNode`, `PostCandidateSelectNode`, `BrandCriticNode`,
`CriticRouterNode`, `IncrementCriticIterationNode`, `ReviseNode`, `TranslateNode`.

- **`WorkSourceNode`** — gathers `Vec<WorkSource>` for the event's `since`..`until` date range:
  git commits, `log.md` entries, and new `planning/decisions/` files, across every repo in
  `event.repos` (defaults to `["."]`, this repo, when omitted — there is no seam onto
  `brain.toml`'s `[[repos]]` table yet, so an accurate full-fleet default is a known limitation).
  Runs entirely over an injectable `CommandRunner` + file/dir-reader seam, so tests never touch a
  real git checkout. An empty or inverted date range (`since > until`) short-circuits before any
  seam is called.
- **`PostDraftNode`** — a model node (`ClaudeCodeStep`) that proposes `candidate_count` post
  candidates from the gathered sources, carrying `business/docs/brand.md`'s voice constraints in
  the prompt. Enforces traceability twice: the prompt asks the model not to emit an empty-sources
  candidate, and `process()` additionally filters any that slip through. Also surfaces any
  model-flagged `unsupported_claims` rather than silently dropping them.
- **`PostCandidateSelectNode`** — deterministic; bridges `PostDraftNode`'s `{candidates: [...]}`
  array into the single `{draft, sources}` shape the critic/revise nodes below expect, selecting
  the first/primary candidate. Multi-candidate fan-out through the critic loop is out of this
  workflow's current scope.
- **`BrandCriticNode`** — evaluates the candidate against `brand.md`'s six-check anti-slop rubric.
  Three of the six checks (rhetorical-contrast setup, bold-bullet triplet, stacked em-dash) run as
  a deterministic text scan before any model call; the remaining three (hedge phrases, summary
  filler, the read-aloud test) are the model's judgment. Stamps `capped: true` onto its own
  `ctx.nodes` result when the loop's iteration cap is reached with the pass still failing.
- **`CriticRouterNode`** — reads `BrandCriticNode`'s stored `verdict`/`capped` and routes to
  `TranslateNode` on pass (or cap), or into the revise loop on fail.
- **`IncrementCriticIterationNode` → `ReviseNode`** — bumps the loop counter, then applies the
  critic's issues to produce a corrected draft (sources unchanged), routing back to
  `BrandCriticNode` for re-evaluation. Reuses `content_pipeline`'s
  `CriticEvaluation`/`CriticVerdict`/`IncrementCriticIterationNode` types directly.
- **`TranslateNode`** — terminal. No-ops when `policy.translate_enabled = false` (the node stays in
  the declared graph either way — standing rule 6: policy never rewires the node set).

## Event schema (`LinkedInPostEventSchema`)

```json
{
  "since": "2026-08-17",
  "until": "2026-08-24",
  "repos": ["engine-rs", "bastion"],
  "candidate_count": 3,
  "policy": { "max_critic_iterations": 5 },
  "profile": "thorough"
}
```

| Field | Required | Meaning |
|---|---|---|
| `since` | yes | ISO-8601 date — start of the work window. |
| `until` | yes | ISO-8601 date — end of the work window (inclusive). |
| `repos` | no | Repos to read from. Defaults to `["."]` (this repo) when omitted — see `WorkSourceNode` above. |
| `candidate_count` | no | How many post candidates to propose for this run. Defaults to `3`. Distinct from the policy layer's `candidate_count` knob below — this is the per-run request. |
| `policy` | no | Per-event `PartialLinkedInPostPolicy` override (EN.4.0 convention). |
| `profile` | no | Named profile — `"baseline"` \| `"cheap-fast"` \| `"thorough"`, or a `harness.json` `linkedin_post.profiles[name]` entry. |

## Policy: `LinkedInPostPolicy`

Four-layer resolution, high-to-low precedence: per-run event `policy` override, a named `profile`
bundle, `planning/harness.json`'s `linkedin_post.policy` defaults, then the built-in default.
Mirrors `content_pipeline`'s policy module, generalized over the shared `crate::policy` plumbing
(EN.4.0) — this module hand-writes no separate `merge_opt`/`Overlay`/`resolve` trio.

| Knob | Built-in default | What it controls |
|---|---|---|
| `model_tiers.draft` / `.critic` / `.translate` | all `Sonnet` | Per-stage model tier for `PostDraftNode`, `BrandCriticNode`, `TranslateNode`. |
| `local` | `LocalConfig::default()` | Config for the `local` tier, used by whichever stage resolves to `ModelTier::Local`. |
| `max_critic_iterations` | `3` | Cap on the bounded brand-critic revise loop. Hard ceiling `10` (`MAX_CRITIC_ITERATIONS_CEILING`) — a caller value above this is **rejected**, never clamped. |
| `candidate_count` | `3` | Policy-layer default for how many candidates `PostDraftNode` proposes, when the event omits its own `candidate_count`. Hard ceiling `20` (`MAX_CANDIDATE_COUNT_CEILING`), same reject-not-clamp discipline. |
| `translate_enabled` | `true` | Whether the `TranslateNode` pass runs at all; `false` routes it to its no-op path. |

Full field table and validation: `crates/engine-core/src/workflows/linkedin_post/policy.rs`. Named
profiles (`baseline`/`cheap-fast`/`thorough`) and the harness defaults section: `planning/harness.json`'s
`linkedin_post` block.

## How to trigger a run

Same HTTP surface as every other `engine-serve` workflow (`docs/cli.md`; see
[sdlc-flow-workflow.md](sdlc-flow-workflow.md#how-to-trigger-a-run) for the full auth/mounting
story):

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "LINKEDIN_POST",
  "data": {
    "since": "2026-08-17",
    "until": "2026-08-24",
    "profile": "cheap-fast"
  }
}
```

`GET /workflows` lists `LINKEDIN_POST` once `register_linkedin_post`/`register_builtin_workflows`
has run; `GET /workflows/LINKEDIN_POST/graph` returns the declared schema above.

## Reading outputs

- **`ctx.nodes["WorkSourceNode"]`** — the gathered `Vec<WorkSource>` for the run's date range.
- **`ctx.nodes["PostDraftNode"]`** — `{candidates: [PostCandidate, ...]}`, plus any
  `unsupported_claims` the model flagged.
- **`ctx.nodes["PostCandidateSelectNode"]`** — the single `{draft, sources}` shape selected for the
  critic loop.
- **`ctx.nodes["BrandCriticNode"]`** — the current-pass `CriticEvaluation`
  (`verdict`/`confidence`/`issues`/`iteration`), plus `capped: true` when the loop's cap was hit
  with the pass still failing.
- **`ctx.nodes["ReviseNode"]`** — the corrected `{draft, sources}` after applying the critic's
  issues (sources unchanged from the candidate under review).
- **`ctx.nodes["TranslateNode"]`** — `{translated_markdown}` when `translate_enabled` was `true`;
  a no-op stamp otherwise.

This workflow has no dedicated `linkedin-post-state.json` telemetry writer of its own.

## Scope notes

- **Node count is eight**, invariant across every policy setting — `translate_enabled = false`
  no-ops `TranslateNode` in place rather than removing it from the graph (standing rule 6).
- **No Local-tier rewire.** `registry_for_policy` ignores its `policy` argument and returns the
  plain `registry()` unchanged: none of this workflow's composed nodes
  (`PostDraftNode`/`BrandCriticNode`/`ReviseNode`/`TranslateNode`) expose a `with_meta_transport`
  hook, unlike `CONTENT_PIPELINE`/`PROPOSAL_GENERATOR`. This is deliberate, not a stub.
- **No corpus write.** Unlike `CONTENT_PIPELINE`/`RESEARCH_AGENT`, this workflow has no
  `MaterializeDocNode`/`PersistToBrainNode` — a drafted post is not brain-persisted knowledge; it
  is a candidate for a human to actually post.
- **`event.repos` full-fleet default is a known limitation** — see `WorkSourceNode` above.
