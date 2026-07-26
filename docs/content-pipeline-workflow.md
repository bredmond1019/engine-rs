---
type: Reference
title: Content Pipeline Workflow
description: How the CONTENT_PIPELINE workflow works — the envelope-based, channel-agnostic thirteen-node graph, bounded self-critic loop, optional translation, event schema, tunable ContentPipelinePolicy, the engine-brain persist boundary, triggering, and reading outputs
doc_id: content-pipeline-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [content-pipeline, workflow, graph, policy, ingress-envelope, self-critic, translate, persist-to-brain, http-post]
related: [architecture, proposal-generator-workflow, research-agent-workflow, diagnostic-intake-workflow, sdlc-flow-policy, data-contract, D9-engine-brain-boundary]
---

# Content Pipeline Workflow

`CONTENT_PIPELINE` (block `EN.5.A`) is a policy-aware, thirteen-node workflow that turns any
channel-agnostic `IngressEnvelope` — a web article, a YouTube transcript, or a human/agent
channel message — into a summarized, optionally-translated digest, POSTed to the company brain
(Synapse) as durable knowledge. It rebuilds Synapse's original `CONTENT_PIPELINE` around the
shared `engine_contract::envelope::IngressEnvelope` contract (D51) so one graph serves every
ingress channel: today's web articles and YouTube transcripts, and Phase 6's Slack, Telegram,
WhatsApp, Email, `ResearchAgent`, and workflow-trigger channels — without a graph edit, because
routing is on `SourcePayload` kind, never on `ChannelType`. It is built on the `EN.4.0` shared
policy framework (see [sdlc-flow-policy.md](sdlc-flow-policy.md) for that framework's mechanics —
this doc only covers how `CONTENT_PIPELINE` configures and uses it).

Source: `crates/engine-core/src/workflows/content_pipeline/` (`mod.rs`, `schema.rs`, `policy.rs`,
`profiles.rs`, `source_router.rs`, `fetch_article.rs`, `fetch_transcript.rs`,
`normalize_channel_content.rs`, `summarize.rs`, `self_critic.rs`, `critic_router.rs`,
`increment_critic_iteration.rs`, `revise.rs`, `translate.rs`, `digest_render.rs`,
`persist_to_brain.rs`, `graph.rs`), registered from `crates/engine-serve/src/workflows.rs`
(`register_content_pipeline` → `register_builtin_workflows`). The channel-agnostic envelope
contract (`ChannelType`, `ReplyContext`, `SourcePayload`, `IngressEnvelope`) lives in
`crates/engine-contract/src/envelope.rs`. The injectable HTTP-POST seam `PersistToBrainNode` uses
lives in `crates/engine-core/src/nodes/http_post.rs`.

## Graph shape

```
SourceRouterNode -> { FetchArticleNode | FetchTranscriptNode | NormalizeChannelContentNode }
  -> SummarizeNode -> SelfCriticNode -> CriticRouterNode
  -> { TranslateSkipRouterNode -> { TranslateNode | DigestRenderNode }
     | IncrementCriticIterationNode -> ReviseNode -> SelfCriticNode }  // back-edge
TranslateNode -> DigestRenderNode -> PersistToBrainNode  // terminal in EN.5.A
```

Thirteen nodes. `SourceRouterNode` is a `Router` that parses/validates the inbound
`ContentPipelineInput`, resolves the run's policy (via `PolicyConfigSource::Builtin` — a channel
envelope carries no repo checkout), stamps the envelope + resolved policy onto `ctx`, and routes
purely on `SourcePayload` kind: `Url` → `FetchArticleNode`, `VideoId` → `FetchTranscriptNode`,
everything else (`ChannelMessage`, `TaskContextRef`, `WorkflowTrigger`) →
`NormalizeChannelContentNode`. All three converge on one `{title, text, source_ref}` shape that
`SummarizeNode` reads regardless of which one ran.

`CriticRouterNode` is likewise a `Router`: it reads `SelfCriticNode`'s stored `CriticEvaluation`
plus the resolved loop bounds and routes to `TranslateSkipRouterNode` (pass, confidence threshold
met, or iteration cap reached) or `IncrementCriticIterationNode` (revise, back-edge — bump the
counter, run `ReviseNode`, re-enter `SelfCriticNode`). `TranslateSkipRouterNode` is a third
`Router`: translate-on routes through `TranslateNode` first, translate-off skips straight to
`DigestRenderNode`. `PersistToBrainNode` is the sole terminal node — no forward connection (EN.6.A
wires an `ActionDispatchNode` after it later).

Per `routing.rs`'s D42 declared-acyclic / runtime-cyclic contract, the `ReviseNode ->
SelfCriticNode` back-edge is never walked by `WorkflowValidator`'s DFS cycle check because it is
reached only through `CriticRouterNode`'s runtime `Router::route` decision, not a declared
non-router connection.

| Node | Kind | What it does |
|---|---|---|
| `SourceRouterNode` | Deterministic router | Parses/validates `ContentPipelineInput`, resolves the run policy (`PolicyConfigSource::Builtin`), stamps the envelope + resolved policy snapshot onto `ctx`, and routes on `SourcePayload` kind (not `channel_type`). |
| `FetchArticleNode` | Deterministic, injectable fetch seam | Handles `SourcePayload::Url` via a live `reqwest` GET + naive `<title>` extraction (`ArticleFetch` trait; `StubArticleFetch` for tests). |
| `FetchTranscriptNode` | Deterministic, injectable fetch seam | Handles `SourcePayload::VideoId` via a `TranscriptFetch` trait; the live implementation fails closed with a descriptive error until a first-party YouTube transcript API is wired in — the seam is where later channel work plugs in a real implementation. |
| `NormalizeChannelContentNode` | Deterministic passthrough | Handles `ChannelMessage`/`TaskContextRef`/`WorkflowTrigger` payloads, building `source_ref` as `{channel_type}[:{sender_id}]` and extracting text from the payload's `text`/`inline`/`event_id`/`event` field. |
| `SummarizeNode` | **Model** (Sonnet by default, Local-eligible) | `summarize`-stage node. Reads whichever of the three converge nodes ran, applies policy tier/prompt-cache/verbosity shaping, and stores `{summary, entities, key_points}`. |
| `SelfCriticNode` | **Model** (Sonnet by default, Local-eligible) | `critic`-stage node. Reads the current summary (from `SummarizeNode` or, on a later pass, `ReviseNode`) and the loop iteration counter, stores a `CriticEvaluation` (`verdict`/`confidence`/`issues`/`iteration`). Fails closed to `Revise` on an ambiguous verdict. |
| `CriticRouterNode` | Deterministic router | The bounded self-critic loop's guard — see [Graph shape](#graph-shape) and [The bounded self-critic loop](#the-bounded-self-critic-loop). |
| `IncrementCriticIterationNode` | Deterministic | Bumps the durable iteration counter on the back-edge, forwards to `ReviseNode`. |
| `ReviseNode` | **Model** (Sonnet by default, Local-eligible) | `revise`-stage node. Reads the current summary and `SelfCriticNode`'s issues, produces a revised summary under its own identity for `SelfCriticNode`'s back-edge re-entry. |
| `TranslateSkipRouterNode` | Deterministic router | Reads `ContentPipelineInput.translate`; routes `true` → `TranslateNode`, `false` → `DigestRenderNode`. |
| `TranslateNode` | **Model** (Sonnet by default, Local-eligible) | `translate`-stage node. Translates the current summary to `target_lang` (defaults to `pt-BR`), stores `{translated_markdown}`. |
| `DigestRenderNode` | Deterministic | Deterministically assembles `ContentPipelineOutput`, including a UUID-v5-derived, retry-idempotent `artifact_id` (minted from a fixed namespace + `envelope_id`, so the same envelope always mints the same artifact id). |
| `PersistToBrainNode` | Deterministic, terminal | POSTs the finished digest as a `LearningArtifact` to Synapse's ingest endpoint over an injectable `HttpPost` seam. See [The engine↔brain persist boundary](#the-enginebrain-persist-boundary). |

`registry_for_policy(&ContentPipelinePolicy)` in `graph.rs` rewires whichever of the four
Local-eligible stages — `summarize`, `critic`, `revise`, `translate` — the policy resolves to
`ModelTier::Local`, routing through `openai_compat_transport_live` (falling back to the real
`claude` CLI transport on any local-endpoint failure). It never rewires the fetch/normalize/
render/persist stages — they carry no `ModelTier` field and are not model nodes at all.

## The bounded self-critic loop

`CriticEvaluation.iteration` is 0-based and counts revisions completed *before* the pass it is
stamped on, so a critic pass's 1-based ordinal is `iteration + 1`. `CriticRouterNode` exits the
loop (routes to `TranslateSkipRouterNode`) when any of:

- the critic's verdict is `Pass`;
- the critic's `confidence` meets or exceeds the resolved `critic_confidence_threshold`;
- the cap is reached: `iteration.saturating_add(1) >= max_critic_iterations` — this pins
  `max_critic_iterations = N` to **exactly N** critic passes when the cap (not a verdict/confidence
  exit) governs. Checking `iteration >= N` directly would let the loop run `N + 1` passes before
  capping.

Otherwise it routes to `IncrementCriticIterationNode` (the back-edge: bump the counter → run
`ReviseNode` → re-enter `SelfCriticNode`). The guard/increment cluster is hand-rolled (mirroring
`sdlc_flow::task_loop`'s `TriageRouterNode`/`IncrementAttemptNode` idiom), not built on
`crate::loop_combinator::build_loop` — the combinator's fixed, graph-build-time `max_iterations`
can't express this loop's per-run, policy-resolved cap.

## Event schema (`ContentPipelineInput`)

```json
{
  "envelope": {
    "envelope_id": "env-1",
    "channel_type": "web_article",
    "timestamp": "2026-07-25T00:00:00Z",
    "source": { "kind": "url", "url": "https://example.com/a" }
  },
  "translate": true,
  "target_lang": "es",
  "profile": "local-drafting"
}
```

| Field | Notes |
|---|---|
| `envelope` | Required `IngressEnvelope` (`engine_contract::envelope`) — see [The `IngressEnvelope` contract](#the-ingressenvelope-contract). |
| `translate` | Optional, defaults `false`. When `true`, `TranslateSkipRouterNode` routes through `TranslateNode` before `DigestRenderNode`. |
| `target_lang` | Optional, defaults `"pt-BR"`. Target language `TranslateNode` translates to when `translate` is `true`. |
| `policy` | Optional per-run `PartialContentPipelinePolicy` override — highest-precedence layer. |
| `profile` | Optional name of a built-in or `harness.json`-defined policy profile bundle. |

## The `IngressEnvelope` contract

`engine_contract::envelope` (D51, EN.5.A) is the channel-agnostic ingress contract every ingress
channel arrives as — routing is on the internally-tagged `SourcePayload` kind, never on
`ChannelType`, so adding a new channel later costs zero graph edits.

- **`ChannelType`** — `WebArticle`, `YouTubeTranscript` (serialized `youtube_transcript`), `Slack`,
  `Telegram`, `WhatsApp`, `Email`, `ResearchAgent`, `WorkflowTrigger`, `Web` (bastion-web surface),
  `Tui` (bastion terminal UI), `Schedule` (the engine's own scheduler), `Api` (a raw API call with
  no richer channel semantics).
- **`SourcePayload`** — internally tagged on `kind`: `Url { url }`, `VideoId { video_id }`,
  `ChannelMessage { text, attachments }`, `TaskContextRef { workflow_type, event_id, inline }`
  (a prior run's output as content, EN.6.E), `WorkflowTrigger { workflow_type, event }`.
- **`ReplyContext`** — `{ thread_id, conversation_id, channel_token }`, opaque to the pipeline;
  only the owning `ChannelTransport` (EN.6.*) interprets these fields.
- **`IngressEnvelope`** — `{ envelope_id, channel_type, sender_id, reply_context, timestamp,
  source, raw_payload }`. `raw_payload` carries the unmodified channel payload for audit/replay;
  pipeline nodes never parse it. `timestamp` is a byte-stable `String` (RFC 3339), not a typed
  date, to preserve the contract seam.

## Policy: `ContentPipelinePolicy`

Same four-layer precedence as `SdlcPolicy`/`ProposalGeneratorPolicy` — **per-run event `policy`
override > per-run event `profile` > `harness.json` `content_pipeline.policy` defaults > built-in
default** — resolved via the shared `crate::policy::resolve` framework, and built on `EN.5.D`'s
derived `crate::policy::Overlay` (no hand-written `merge_opt`/`merge_local`/`apply_override` trio).
There is no setup node: `SourceRouterNode` calls `profiles::resolve_policy_for_run_from` directly,
since a channel envelope has no repo checkout to derive a worktree from
(`PolicyConfigSource::Builtin`).

Knobs:

| Field | Values | What it controls |
|---|---|---|
| `output_verbosity` | `terse` \| `normal` \| `verbose` | Verbosity directive added to model nodes' prompts. |
| `prompt_cache` | `bool` | Whether a stable system-prompt anchor is added for provider-side prompt caching. |
| `model_tiers.{summarize,critic,revise,translate}` | `sonnet` \| `haiku` \| `opus` \| `local` | Per-stage model tier — all four stages are Local-eligible. |
| `local.{endpoint,model,constrained_json}` | string / string / bool | Local-endpoint config, applied when any of the four stages resolves to `ModelTier::Local`. |
| `max_critic_iterations` | `u32`, ceiling 10 | Bounded self-critic loop cap — see [The bounded self-critic loop](#the-bounded-self-critic-loop). |
| `critic_confidence_threshold` | `f64`, `[0, 1]` | Confidence exit threshold the loop also checks each pass. |

**Bounds are rejected, not clamped.** `validate_bounds` runs once on the fully-resolved policy
(after all four layers merge) and returns `Err` for `max_critic_iterations == 0` or `>
MAX_CRITIC_ITERATIONS_CEILING` (10), or `critic_confidence_threshold` outside `[0, 1]` — an
out-of-range value from any layer surfaces as a rejected run, never a silently accepted or
clamped one.

Built-in default: `ContentPipelinePolicy::default()` — normal verbosity, all four tiers `sonnet`,
prompt cache off, `max_critic_iterations = 3`, `critic_confidence_threshold = 0.8`.

### Named profiles

Three built-in bundles in `profiles.rs` (`profile_by_name`), looked up first in
`planning/harness.json` → `content_pipeline.profiles[name]`, then in this built-in set:

| Name | Tradeoff |
|---|---|
| `baseline` | Explicit no-op control — all four tiers Sonnet, normal verbosity, prompt cache off, default loop bounds (3 iterations, 0.8 confidence) — spelled out for clarity, matches the built-in default. |
| `local-drafting` | Rewires all four Local-eligible stages (`summarize`, `critic`, `revise`, `translate`) to `ModelTier::Local` with `constrained_json` on; leaves the other knobs untouched (`None`) so it composes cleanly with other override layers. |
| `fast-summarize` | Rewires only `summarize` to `ModelTier::Haiku`; every other knob untouched. |

`planning/harness.json` carries a matching `content_pipeline.{policy,profiles}` section (mirroring
`sdlc.{policy,profiles}` — see
[sdlc-flow-policy.md](sdlc-flow-policy.md#2-planningharnessjson--sdlcpolicy-this-repos-defaults)
for the reader/precedence mechanics, identical here).

## The engine↔brain persist boundary

`PersistToBrainNode` (`persist_to_brain.rs`) is where this workflow crosses THE BOUNDARY TEST
(`CLAUDE.md`) — D51: no embedding or pgvector in engine-rs, this seam only POSTs. It reads
`DigestRenderNode`'s stored `ContentPipelineOutput` plus the `source_ref` whichever of
`FetchArticleNode`/`FetchTranscriptNode`/`NormalizeChannelContentNode` converged on, and builds a
`LearningArtifact` payload:

```json
{
  "artifact_id": "<UUID v5, derived from envelope_id — retry-idempotent>",
  "channel_type": "web_article",
  "source_ref": "...",
  "summary": "...",
  "digest_markdown": "...",
  "entities": ["..."],
  "language": "pt-BR"
}
```

`language` is the event's `target_lang` when `translated_markdown` is present on the output,
`"en"` otherwise (the digest was never translated, so it's still in its original language). It
awaits an injectable `crate::nodes::http_post::HttpPost` seam (an `async_trait` object, `Arc<dyn
HttpPost>` — production code uses the `reqwest`-backed `http_post_live`; the gated `cargo test`
suite injects a stub that records the last `(url, payload)` pair, so no live network call happens
in tests) to POST the payload. Non-2xx responses (or a transport failure) surface as a
`NodeError` — there is no fallback target for a failed brain push. On success it stamps
`{"posted": true, "status", "artifact_id", "response"}` onto `ctx`.

**Not yet wired to a real endpoint.** `PersistToBrainNode::new()` currently POSTs to a hardcoded
placeholder `BRAIN_INGEST_URL` constant (`http://localhost:8000/ingest/learning`) —
`ContentPipelinePolicy` carries no endpoint knob. The canonical target is Synapse's `POST
/ingest/*` (brain block `OR.Q`). `with_url(...)` exists alongside `with_http_post(...)` so tests
and future callers can override the target without touching the constant.

Per THE BOUNDARY TEST, this node only POSTs — no embedding model is loaded, no `pgvector`
connection is opened, and no corpus table is written from this repo. What happens behind the
ingest endpoint is entirely Synapse's concern. Terminal in `EN.5.A`: no forward connection —
EN.6.A wires an `ActionDispatchNode` after it later.

## How to trigger a run

Same HTTP surface as every other `engine-serve` workflow (`docs/cli.md`; see
[sdlc-flow-workflow.md](sdlc-flow-workflow.md#how-to-trigger-a-run) for the full auth/mounting
story):

```
POST /events/
X-API-Key: <BASTION_ENGINE_API_KEY>
Content-Type: application/json

{
  "workflow_type": "CONTENT_PIPELINE",
  "data": {
    "envelope": {
      "envelope_id": "env-1",
      "channel_type": "web_article",
      "timestamp": "2026-07-25T00:00:00Z",
      "source": { "kind": "url", "url": "https://example.com/a" }
    },
    "profile": "local-drafting"
  }
}
```

`GET /workflows` lists `CONTENT_PIPELINE` once `register_content_pipeline`/
`register_builtin_workflows` has run; `GET /workflows/CONTENT_PIPELINE/graph` returns the declared
schema above.

## Reading outputs

- **`ctx.nodes["SummarizeNode"]` / `ctx.nodes["ReviseNode"]`** — `{summary, entities, key_points}`
  (or the revised equivalent).
- **`ctx.nodes["SelfCriticNode"]`** — the current-pass `CriticEvaluation`
  (`verdict`/`confidence`/`issues`/`iteration`).
- **`ctx.nodes["TranslateNode"]`** — `{translated_markdown}`, present only when `translate` was
  `true`.
- **`ctx.nodes["DigestRenderNode"]`** — the assembled `ContentPipelineOutput`
  (`artifact_id`, `source_channel`, `summary`, `entities`, `digest_markdown`, `digest_html`,
  `translated_markdown`).
- **`ctx.nodes["PersistToBrainNode"]`** — `{"posted": true, "status", "artifact_id",
  "response"}`, the brain-push result.

This workflow has no dedicated `content-pipeline-state.json` telemetry writer of its own.

## Scope notes

- **Node count is thirteen** as of `EN.5.A`: `SourceRouterNode`, `FetchArticleNode`,
  `FetchTranscriptNode`, `NormalizeChannelContentNode`, `SummarizeNode`, `SelfCriticNode`,
  `CriticRouterNode`, `IncrementCriticIterationNode`, `ReviseNode`, `TranslateSkipRouterNode`,
  `TranslateNode`, `DigestRenderNode`, `PersistToBrainNode`. There is no setup/worktree node.
- **Routes on `SourcePayload` kind, never `ChannelType`** — this is deliberate: adding a new
  channel later (Slack, Telegram, WhatsApp, Email — Phase 6) costs zero graph edits, only a new
  `SourcePayload` variant and, if needed, a new converge node.
- **No embedding/pgvector/corpus writes** — per THE BOUNDARY TEST (`CLAUDE.md`), this workflow
  only acquires, reasons, and POSTs; see
  [The engine↔brain persist boundary](#the-enginebrain-persist-boundary).
- **`FetchTranscriptNode`'s live path fails closed** — no first-party YouTube transcript API is
  wired into the workspace yet; `UnimplementedTranscriptFetch` errors descriptively rather than
  silently returning empty content. The `with_fetch` seam is where later channel work plugs in a
  real implementation.
- **Out of scope for this block**: real channel adapters (Slack/Telegram/WhatsApp/Email —
  Phase 6), the real Synapse `OR.Q` ingest endpoint, `ActionDispatchNode` (EN.6.A).
- **Hermetic test coverage**: `crates/engine-core/tests/content_pipeline_e2e.rs` drives the full
  `Workflow::run` walk loop through every branch (both fetch/normalize converge paths, the
  self-critic loop's pass/revise/cap exits, translate on/off, the persist payload shape, retry
  idempotency of `artifact_id`, `registry_for_policy`'s Local rewire, `EventsRow` round-tripping,
  dispatcher registration, and a `#[ignore]`-gated ranked-profile experiment harness).
