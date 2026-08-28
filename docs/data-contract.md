---
type: Reference
title: engine-rs Data Contract (Canonical)
description: The canonical, authoritative data contract for the engine-rs execution runtime — how each contract field maps to engine-rs's Rust types, and how engine-rs's HTTP/write surface implements the contract it authors. Synapse (orchestrator) and bastion are pinning consumers.
doc_id: data-contract
layer: [engine]
project: engine-rs
status: active
keywords: [data contract, orchestrator, PostgreSQL, node_runs, field mappings, v1.8.0, cancellation, abort, budget gate, engine-contract, event read api, ingest, async lifecycle, sse, run readback, recall, walk, pulse, locale, rate card, investment shape, campaign, campaign_id]
related: [architecture, D6-cancellation-and-budget-semantics, brain:D20-shared-data-contract, brain:D78-engine-rs-owns-the-data-contract]
---

# Data Contract

**Contract Version: 1.8.0**

Per [D78](file:///Users/brandon/Dev/agentic-portfolio/docs/decisions/D78-engine-rs-owns-the-data-contract.md)
(2026-08-21), **this document is the canonical, authoritative data contract.** D78 superseded D20's
ownership clause: engine-rs now authors the contract, and `orchestrator` (soon Synapse) and
`bastion` are its *pinning consumers* — each re-pins its own consumer-view doc
(`orchestrator/docs/data-contract.md`, `bastion/docs/data-contract.md`) to the version stated here
whenever it changes. This file maps each contract field to engine-rs's Rust types. When this
contract bumps, update the version and mappings here first, then note the change in
`planning/status.md` so consumers know to re-pin. The `/log-work` checklist in all three repos
prompts this.

> **engine-rs's role differs from a pure read-only consumer like `bastion`.** `bastion` only reads
> `events.task_context` from PostgreSQL. `engine-rs` is a **parallel-pilot implementer** of the
> contract (D42) — it *ports* the same `task_context`/`NodeRun`/`Usage`/`EventsRow` shapes into
> native Rust types (`engine-contract`), *writes* its own `events` rows via `engine-store` as an
> alternate execution runtime (not a fork of the orchestrator's data), and exposes the **same**
> HTTP surface (`engine-serve`) so a caller — `bastion`, an orchestrator-equivalent trigger, or
> BastionUI — can talk to either runtime interchangeably. As of v1.1.0, engine-rs's own EN.2.B
> block is what *introduced* the `POST /events/{run_id}/abort` endpoint and the
> `metadata.cancellation` / `metadata.budget` run-level annotations into the canonical contract —
> engine-rs is this pin's originating implementation, not just a consumer of someone else's
> addition. See `planning/decisions/D6-cancellation-and-budget-semantics.md`.

---

## Conformance, not polling

engine-rs does not read the orchestrator's PostgreSQL rows at runtime — it has no read path back
into the canonical contract at all. What it pins instead is **shape conformance**:
`crates/engine-contract/tests/round_trip.rs` deserializes a real, code-path-captured orchestrator
fixture (`tests/fixtures/research_agent_task_context.json`, emitted by the orchestrator's
`scripts/emit_task_context_fixture.py`, not hand-authored by engine-rs) into `TaskContext` and
re-serializes it, asserting semantic JSON equality with no field/casing/type drift. A second test
byte-diffs engine-rs's checked-in copy against the orchestrator-owned original when a sibling
checkout is present, as an ongoing drift guard. See
`core/_planning/engine-rs/orchestrator-contract-conformance/notes.md` for the full provenance
story (Gap 1, closed 2026-07-16, commit `33dca04`).

---

## Field mappings

### `events` row (contract §4) → `engine_contract::EventsRow`

| Contract (`events`) | engine-rs (`crates/engine-contract/src/events.rs`) |
|---|---|
| `id` (UUID) | `EventsRow.id: Uuid` |
| `workflow_type` (`varchar(150)`) | `EventsRow.workflow_type: String` |
| `data` (JSON, run input) | `EventsRow.data: serde_json::Value` |
| `task_context` (JSON, full execution state) | `EventsRow.task_context: TaskContext` |
| `created_at` | `EventsRow.created_at: DateTime<Utc>` |
| `updated_at` | `EventsRow.updated_at: DateTime<Utc>` |

Persisted via `engine-store::postgres::{insert_event, update_event, get_event}` on the D2-pinned
`sqlx::PgPool` stack — see `planning/decisions/D2-async-runtime-choice.md`.

### `task_context` JSON (contract §5) → `engine_contract::TaskContext`

| Contract | engine-rs (`crates/engine-contract/src/task_context.rs`) |
|---|---|
| `event` (parsed event payload) | `TaskContext.event: serde_json::Value` |
| `nodes[<ClassName>]` (per-node output) | `TaskContext.nodes: HashMap<String, serde_json::Value>` |
| `metadata` (workflow-level metadata) | `TaskContext.metadata: serde_json::Value` |
| `node_runs[<ClassName>]` (per-node execution envelope) | `TaskContext.node_runs: HashMap<String, NodeRun>` |

Node identity (contract §1) is the join key across `nodes` and `node_runs` in both the canonical
Python shape and engine-rs's Rust shape — engine-rs's own node names come from `Node::name()`
(`engine-core::node::Node`), which callers set to match the class-name convention.

### `NodeRun` envelope (contract §6) → `engine_contract::NodeRun`

| Contract | Type | engine-rs |
|---|---|---|
| `status` | `pending\|running\|success\|failed` (lowercase) | `NodeRun.status: NodeRunStatus` — `#[serde(rename_all = "lowercase")]`, rejects any other casing |
| `started_at` | ISO-8601 UTC \| null | `NodeRun.started_at: Option<DateTime<Utc>>` |
| `completed_at` | ISO-8601 UTC \| null | `NodeRun.completed_at: Option<DateTime<Utc>>` |
| `error` | string \| null | `NodeRun.error: Option<String>` |
| `input` | any \| null | `NodeRun.input: Option<serde_json::Value>` |
| `usage` | object \| null | `NodeRun.usage: Option<Usage>` |

`status`/`started_at`/`completed_at` are stamped by the framework-owned `node_context` envelope in
`crates/engine-core/src/workflow.rs`, never by a `Node` implementation itself — see
`docs/architecture.md` § Core Types.

### `Usage` (contract §6) → `engine_contract::Usage`

| Contract | engine-rs |
|---|---|
| `input_tokens` (int \| null) | `Usage.input_tokens: Option<u64>` |
| `output_tokens` (int \| null) | `Usage.output_tokens: Option<u64>` |
| `model` (string, required) | `Usage.model: String` |

`model` is a required `String` on the wire even though the underlying Claude Code SDK can report no
model (`Outcome::primary_model()` returns `None`); `ClaudeCodeStep` supplies the literal `"unknown"`
in that case rather than loosening the contract type — see `docs/architecture.md` §
`ClaudeCodeStep` and `planning/decisions/D20-shared-data-contract.md` (brain-level contract
ownership decision).

### Run-level `metadata` annotations (new in v1.1.0)

Two run-terminal outcomes are spelled as structured `TaskContext::metadata` keys rather than new
`NodeRunStatus` variants (`§6` stays exactly `pending|running|success|failed` — a new status value
would be a MAJOR contract bump; see `planning/decisions/D6-cancellation-and-budget-semantics.md`):

- **Cancelled** — `crate::cancellation::stamp_cancelled` merges:
  ```jsonc
  { "metadata": { "cancellation": { "cancelled": true, "at": "<iso8601>" } } }
  ```
  Nodes not yet reached when the walk halts stay `NodeRunStatus::Pending` on their own `NodeRun`
  entry; only `metadata.cancellation` marks the run itself.
- **Budget-halted** — the private `stamp_budget_halt` in `crates/engine-core/src/workflow.rs`
  (keyed `BUDGET_METADATA_KEY = "budget"`) merges:
  ```jsonc
  {
    "metadata": {
      "budget": {
        "halted": true,
        "reason": { "cap": "max_total_tokens" | "max_cost_usd", "spent": <number>, "limit": <number> }
      }
    }
  }
  ```
  The cap configuration itself (`engine_core::Budget { max_total_tokens: Option<u64>, max_cost_usd:
  Option<f64> }`) is run-configuration passed via `RunOptions`, not a persisted `events` column — it
  surfaces in `metadata.budget` only once a halt actually occurs.

Both are produced by `Workflow::run_with` (`crates/engine-core/src/workflow.rs`) at the node
boundary, before dispatching the next node — see `docs/architecture.md` § Core Types.

**`metadata.completion` (`EN.9.C`, engine-rs-side).** Mirroring `metadata.cancellation`/
`metadata.budget`/`metadata.suspension` exactly, every terminal exit — success, node error,
cancellation, and budget halt — now stamps a `completion` key into the same `final_ctx` snapshot
the durable writer persists, keyed with the same status vocabulary `derive_terminal_status`
reports for that snapshot (`succeeded|failed|cancelled|budget_halted`):

```jsonc
{ "metadata": { "completion": { "terminal": true, "status": "succeeded", "at": "<rfc3339>" } } }
```

`crate::completion::stamp_completion` writes this in `crates/engine-serve/src/suspend.rs` at both
terminal exits (`:467`, `:485`), before `live.mark_terminal`; the suspend path never stamps it — a
suspended run is not terminal, and marking it complete would hide it from the crash-recovery sweep
below. **This is canonical contract text as of 1.7.0** — no `engine_contract` Rust type changes
shape (the annotation lives in the existing free-form `TaskContext::metadata` field, per D6), but
the `metadata.completion` key and the derived-`status` rule that consumes it are now part of the
contract this document defines, not an engine-rs-side footnote. It exists because there is
no `status` column on `events` (contract §4) and status-derivation alone cannot distinguish a clean
finish from a run that crashed mid-walk (a crash before any failure marker is written also derives
as `"succeeded"`): the marker's *absence*, not its content, is what `engine-store`'s
`list_orphan_candidates` query and `engine-serve`'s boot sweep (`crate::orphan::reconcile_orphans`)
key on to find crash-stranded runs. See [orphan-recovery.md](orphan-recovery.md) for the full
sweep, the stale-run alarm, and the policy knobs.

The canonical contract's v1.2.0 adds a third run-level annotation, `metadata.failure` — written by
the orchestrator's Celery worker when a workflow raises inside `process_incoming_event`, on a
fresh session that survives the enclosing transaction's rollback: `{ "failure": { "failed": true,
"error": "<ExcType>: <msg>", "at": "<iso8601>" } }`. Like `cancellation` and `budget`, it lives in
the existing `TaskContext::metadata: serde_json::Value` free-form field — no `engine_contract`
Rust type changes shape. engine-rs's own execution path (`Workflow::run_with`) does not yet stamp
`metadata.failure` on a raising run; whether it should is future work, not this re-pin — `§6`'s
`pending|running|success|failed` `NodeRunStatus` vocabulary is unchanged either way.

### Campaign identity (`§8`, new in v1.8.0, `EN.11.E`)

A **campaign** is the parent identity for the N runs of one orchestration chain — the unit the
stop button (`EN.11.F`), resume (`EN.11.H`), the journal, and `EN.11.G`'s cost rollup all need to
address, and which had no addressable subject before this version.

**`campaign_id: uuid` is a first-class, named, versioned key — it is deliberately NOT a
`metadata` annotation**, unlike `cancellation` / `budget` / `completion` / `suspension` /
`failure` above. Those four live in the free-form `TaskContext::metadata` field because they are
engine-internal bookkeeping about a single run's own lifecycle. A campaign id is different in
kind: it is the cross-run join key a consumer must be able to find and rely on without parsing an
undocumented, unversioned free-form blob. `bastion` vendors its own budget/cost logic and reads
this contract, not engine-rs's internals (HQ D24) — burying `campaign_id` in `metadata` alongside
the lifecycle markers would make it undiscoverable to exactly the consumer this contract exists
to serve. Keep it out of `metadata` on every future edit to this section, even when it would be
mechanically convenient to add it there.

- **On a child run.** Every run spawned as one step of a chain carries `campaign_id` as a
  top-level key of its own `event` JSON (`events.data` / `task_context.event`), seeded by
  `sdlc_flow_event` alongside the existing `repo` / `spec_slug` / `use_worktree` keys. Rust side:
  `engine_core::workflows::orchestration::execute::FlowInvocation::campaign_id: Uuid` (non-
  optional — every chain-spawned run states its campaign, the same discipline as `use_worktree`).
- **On the parent run.** The single `ORCHESTRATION`-type run that drives the chain is itself an
  ordinary HTTP-triggered run, present in `LiveStateStore` like any other. It resolves its own
  `campaign_id` (reusing one supplied on the triggering event, so a resumed/operator-restarted
  chain rejoins the same campaign rather than minting a second identity; otherwise a fresh v4
  uuid) and stamps it — plus one member entry per executed step, each carrying `repo`,
  `block_id`, `use_worktree`, `cost_usd`, and `total_tokens` — into its own `ctx.nodes` result
  under `campaign_members`. This is what makes the parent run the addressable subject for the
  whole campaign without a child-run registry or any relational table (`events` stays "one row
  per workflow run").
- **`cost_usd` stays tri-state**, `Option<f64>` end to end, in every member entry and in the
  rolled-up total below — a step that reported no cost round-trips as JSON `null`, never `0.0`.
  Collapsing that distinction is the exact `total_cost_usd: -0.0` bug `ExecutionOutcome::cost_usd`'s
  doc comment warns about, and a campaign rollup is where a silent zero would do the most damage.
  `total_tokens` sums as a plain `u64` — an absent token figure is a true zero.

See `GET /campaigns/{id}` below (§ HTTP surface parity) for the read side.

---

## HTTP surface parity

`engine-serve` (`crates/engine-serve/src/http.rs`, `abort.rs`) exposes the **same** routes as the
canonical contract's §7, so a caller can target either runtime:

| Method | Path | engine-rs handler |
|---|---|---|
| `POST` | `/events/` | `http::post_events` — `X-API-Key` gated, dispatches + records live state + enqueues the durable write |
| `GET` | `/health` | `http::health` — `200 {status: "ok", build: {git_sha, built_at}}`, the compile-time identity of the binary answering the request (`engine_core::build_info::{GIT_SHA, BUILT_AT}`, `EN.11.A`) — not the currently-deployed Mini binary if a newer one hasn't restarted the process yet |
| `GET` | `/workflows` | `http::list_workflows` |
| `GET` | `/workflows/{type}/graph` | `http::workflow_graph` — `404` for an unregistered type |
| `POST` | `/events/{run_id}/abort` | `abort::abort_run` (EN.2.B) — same `X-API-Key` gate; `401`/`404`/`202` per the canonical contract §7 |
| `GET` | `/events/{event_id}` | `http::get_event` (EN.5.F) — `X-API-Key` gated (401); `404` for an unknown or malformed id; `200 {event_id, workflow_type, status, created_at, updated_at, task_context}`, `status` derived server-side |
| `POST` | `/events/{run_id}/pause` | `suspend::pause_run` (EN.6.F) — engine-rs-only extension, no canonical counterpart; `X-API-Key` gated (401); `404` for an unknown/finished run; `409` if already suspended; otherwise sets the run's `PauseSignal` and returns `202 {run_id, status: "pausing"}` (idempotent against a run that is pausing but not yet suspended) |
| `POST` | `/events/{event_id}/resume` | `suspend::resume_run` (EN.6.F) — engine-rs-only extension, no canonical counterpart; `X-API-Key` gated (401); `404` for an unknown or non-suspended run; `409` for a concurrent resume already in flight; `422` for a policy-resolution failure or an unresolvable `resume_at`; otherwise `202 {run_id, event_id, status: "resuming", resume_at}` |
| `GET` | `/events/suspended` | `suspend::list_suspended` (EN.6.F) — engine-rs-only extension, no canonical counterpart; `X-API-Key` gated (401); `200 [{run_id, workflow_type, created_at, suspended_at, resume_at, reason}]`, newest first; registered ahead of `{event_id}` so the literal path isn't swallowed by the uuid extractor |
| `GET` | `/campaigns/{id}` | `http::get_campaign` (`EN.11.E` task 5) — the campaign readback (§ Campaign identity above); `X-API-Key` gated (401); `404` for a malformed id **and** for an unknown campaign, mirroring `get_event`'s convention rather than a new one; `200 {campaign_id, runs: [...], total_cost_usd, total_tokens, possibly_truncated}` — `total_cost_usd` is `null` when no member reported a cost (never `0.0`), `total_tokens` sums as `u64`, and `possibly_truncated` is `true` when the completed-run ring (`COMPLETED_RUN_RETENTION = 100`) may have evicted an earlier campaign member rather than silently presenting a short list as the whole campaign |
| `GET` | `/campaigns/{id}/journal` | `journal::get_campaign_journal` (`EN.12.D`) — **engine-rs-only extension, no canonical counterpart**: the campaign's durable `JournalRow` log (`StepIntegrated`/`StepBailed`/`GateRefused`/`StateWriteVerificationFailed`/`BudgetHalted`/`ResolvedPolicy`), read straight from Postgres via `engine_store::list_journal_rows_for_campaign`, not from `LiveStateStore`; `X-API-Key` gated (401); `404` uniformly for an unknown/malformed campaign id **and** for a deployment with no `DATABASE_URL` configured (no in-memory journal store exists) — the route cannot distinguish "never happened" from "not persisted here" |

The canonical contract's v1.2.0 route `GET /events/{event_id}` and the `event_id` field on
`POST /events/`'s `202` body are now **ported** to `engine-serve` (EN.5.F). `POST /events/` spawns
the run instead of awaiting it (`http::post_events`, via `actix_web::rt::spawn` — the current-thread
arbiter, since `OnProgress` is not `Send`) and returns `202 {run_id, event_id}` immediately;
`event_id` always equals `run_id` — both are the `events.id` primary key. `GET /events/{event_id}`
reads back the canonical shape quoted above, serving only from the in-memory `LiveStateStore`
(task 1's bounded completed-run ring, `COMPLETED_RUN_RETENTION = 100`, plus the still-live map) —
there is no Postgres fallback; CI has no `DATABASE_URL` and this route must stay DB-free.

engine-rs also exposes `GET /events/{event_id}/stream` (`crate::stream::stream_event`), an
**engine-rs-only extension** with no counterpart in the canonical contract (so it is not a parity
gap in either direction, in either runtime). It is `X-API-Key` gated, nests under the run it
streams — mirroring the existing `POST /events/{run_id}/abort` convention — and emits
`text/event-stream` frames over a `tokio::sync::broadcast` tee fed by `on_progress`: one frame per
node transition plus a terminal frame, after which the stream ends. Its `known`-id check uses the
same three-tier lookup as `GET /events/{event_id}` below (live map, then the terminal record ring,
then `live_run_metadata()`) so a client that opens the stream in the window after `POST /events/`
registers the run but before the first `on_progress` snapshot lands does not get spuriously 404'd
(EN.3.G).

**Server-derived `status` (`http::derive_terminal_status`).** A non-terminal run always reads back
`"running"`. For a terminal run, checked in order against the retained snapshot:

| condition | status |
|---|---|
| `metadata.cancellation.cancelled == true` (`engine_core::stamp_cancelled`) | `cancelled` |
| `metadata.budget.halted == true` (`engine_core::workflow::stamp_budget_halt`) | `budget_halted` |
| `metadata.failure.failed == true` (contract v1.2.0's `metadata.failure`, not currently stamped by engine-rs, checked defensively), or any `node_runs[..].status == NodeRunStatus::Failed` | `failed` |
| none of the above | `succeeded` |

**Semantic change: `POST /events/` no longer returns `500` on a failed run.** Before EN.5.F, a
failed run's error surfaced synchronously as `500 {error, run_id}` from the awaited handler. Now
that the run is spawned and the `202` response is sent before the run can fail, failure surfaces
asynchronously instead: through the `GET /events/{event_id}` readback (`status: "failed"`) and the
terminal SSE frame on `GET /events/{event_id}/stream`. `POST /events/` itself never returns `500`
for a run failure anymore.

**Default run budget (EN.5.F).** `post_events` no longer passes `budget: None` — every HTTP-triggered
run gets a default `Budget` read from the environment (`http::default_budget_from_env`, memoized):
`ENGINE_RUN_MAX_COST_USD` (default `5.0`) and `ENGINE_RUN_MAX_TOKENS` (default unset, i.e. no cap).
This is read directly from the environment inside `http.rs` rather than added as an `AppState`
field, because `bastion` constructs `engine_serve::http::AppState` as a struct literal over an
unpinned path dependency and any new public field would be an immediate cross-repo compile break
for zero gain.

`POST /events/{run_id}/abort` is backed by `abort::RunRegistry`, a per-run `CancellationToken`
registry: `post_events` mints and registers a token alongside the freshly-minted `run_id` before
spawning the run, and the spawned task deregisters it once the run ends (success, failure,
cancellation, or budget halt) so a later abort against a finished `run_id` correctly 404s rather
than triggering a token nobody checks anymore.

The canonical contract's v1.3.0 adds two more routes, `POST /ingest/proposal` and
`POST /ingest/artifact` (`OR.Q`), implemented only in the orchestrator's own Python API
(`app/api/` — mounted beside `/events`, `/health`, `/workflows`) — **not** in `engine-serve`, and
not planned to be: these are ingest-direction routes engine-rs *calls*, not routes it needs to
serve for runtime interchangeability. `/ingest/proposal` is pinned exactly to the payload
`EN.4.C`'s `PersistToBrainNode` (`crates/engine-core/src/workflows/proposal_generator/persist_to_brain.rs`,
built) asserts against a stub — `{ artifact_id, company_name, doc_type, section, content, roadmap }` —
returning `200 { artifact_id, chunks_written }`; both routes reuse the same `X-API-Key` gate as
`POST /events/` and reject a malformed body with a typed `422` (never `500`). **`EN.6.K` task 3**
retires the hardcoded placeholder `BRAIN_INGEST_URL` const both persist nodes carried: the target
URL is now `{BrainConfig::base_url}/ingest/proposal` (or `/ingest/artifact`, below), resolved from
`BrainConfig::from_env` (`BRAIN_API_URL`/`BRAIN_API_KEY`), and both nodes now send the `X-API-Key`
header this route requires — `PersistToBrainNode::with_config`/`with_url` override it for tests.
`content_pipeline::persist_to_brain::PersistToBrainNode` is re-pointed the same way, but to
`POST /ingest/artifact`, not `/ingest/proposal`: Synapse has never served the `/ingest/learning`
route that node's `BRAIN_INGEST_URL` const previously named. It maps the shared `LearningArtifact`
payload shape (`{artifact_id, channel_type, source_ref, summary, digest_markdown, entities,
language}`) into `/ingest/artifact`'s generic envelope: `{artifact_id, doc_type:
"learning-artifact", content: digest_markdown, metadata: {channel_type, source_ref, entities,
language, summary}}` — the optional `section`/`project`/`title`/`description` envelope fields are
omitted (a `LearningArtifact` carries no data for them), and Synapse currently discards `metadata`
entirely (acceptable: `content` is what gets embedded). `HarvestApproveNode`
(`crates/engine-core/src/nodes/harvest_approve.rs`), which replays a pending harvest's stored
`payload` to its stored `url`, now sends the same `X-API-Key` header too — it is no longer the one
unauthenticated door into `POST /ingest/*`.

**`metadata.suspension` (EN.6.F, engine-rs-side).** Mirroring `metadata.cancellation`/
`metadata.budget`, a suspended run's `TaskContext.metadata` carries a `suspension` key: `{suspended,
at, resume_at, reason, origin_identity, ledger: {total_tokens, total_cost_usd}, resume_count,
requested}`. `reason` is `"operator_pause"` or `"suspend_node"` — the two origins (`POST
/events/{run_id}/pause` and a workflow-authored `SuspendNode`) converge on this one marker and one
`resume_at` pointer. The key is never deleted on resume (`stamp_resumed` flips `suspended: false`,
resets `requested`, and increments `resume_count`), so a resumed run's final `EventsRow` round-trips
identical in shape to an uninterrupted run's. No `engine_contract` Rust type changed shape — like
`metadata.cancellation`/`metadata.budget`/`metadata.failure`, this lives entirely in the existing
free-form `TaskContext::metadata: serde_json::Value` field, per D6. See
[suspend-resume.md](suspend-resume.md) for the full field-by-field shape and both suspension
origins.

**Semantic change: `durable.rs`'s writer now upserts on a suspend/resume, not just an initial
insert.** Before EN.6.F, `spawn_durable_writer`'s per-run write path was insert-then-update once,
ending at one terminal write. A suspended exit is not terminal, so the same run's `events` row can
now legitimately receive a fresh round of `on_progress` writes after a resume — the existing
insert-first/update-thereafter logic already handled this correctly (it keyed on `run_id`
already-seen, not on "not yet terminal"), but it is worth calling out explicitly here: a suspended-
then-resumed run's durable row is written to more than once across its lifetime in a way no
pre-EN.6.F run ever was.

The canonical contract's v1.4.0 adds three more routes, `GET /recall`, `GET /walk`, and
`GET /pulse` (`OR.Q2`) — the read half of the D51 HTTP adapter whose write half (`POST
/ingest/*`) landed in v1.3.0, implemented only in the orchestrator's own Python API (`app/api/
read.py`), **not** in `engine-serve`, and not planned to be: these are corpus-read routes
engine-rs could *call* as a client, not routes it needs to *serve* for runtime interchangeability
(the two runtimes' interchangeability contract is about `events`/`task_context`, not the Brain
corpus). All three reuse the same `X-API-Key` gate as `POST /events/` and reject a missing/
malformed query param with a typed `422` (never `500`). No `engine_contract` Rust type changes
shape — `RecallNode` (`crates/engine-core/src/nodes/brain_client.rs`, `EN.6.K`) is engine-rs's
first client of the three, over `GET /recall`; `GET /walk` and `GET /pulse` still have no engine-rs
caller. Wiring a hybrid workflow to ground a proposal draft in existing corpus content via
`RecallNode` before persisting it through `POST /ingest/proposal` remains open follow-on work.

The canonical contract's v1.5.0 adds an optional `authored_at: datetime | null` field to both
ingest routes, `POST /ingest/proposal` and `POST /ingest/artifact` (`OR.ticket.corpus-reconcile`).
It is additive and backward-compatible — omitting it, or sending `null`, preserves the pre-existing
server-side `datetime.now()` fallback exactly, so `PersistToBrainNode`'s current payload stays
valid unchanged. It sits alongside (and does not contradict) the `EN.4.F` note below about the
structured `investment` / `authored_locale` shape `PersistToBrainNode` now embeds in `roadmap`:
whoever wires the real ingest URL should send both — the new `authored_at` stamp *and* the new
`roadmap` shape.

The canonical contract's v1.6.0 changes `GET /recall`'s response semantics (`OR.K2`), and this is
the one that matters for **`EN.6.K`** — the Brain read-client seam, engine-rs's *first* real
`GET /recall` consumer. **`EN.6.K` must be built against 1.6.0 semantics, not 1.4.0:**

- **`score` is a similarity where higher is always better**, on every path — `1.0` for an exact-id
  match, `1.0 - cosine distance` for semantic, unchanged fused similarity for hybrid. Under 1.4.0
  the exact-id and semantic paths returned a raw cosine *distance* (`0.0` for an exact-id match,
  lower-is-better). A `RecallNode` that sorts ascending or thresholds with `score < x` — the
  1.4.0-correct direction — ranks and filters results **backwards with no error**. Re-verify the
  comparison direction of every `score` use.
- **`via` may be any of `exact-id | semantic | hybrid | structural | keyword | memory`.** The
  vocabulary widened: the hybrid path now reports per-candidate provenance instead of collapsing
  everything to a bare `"hybrid"` tag. Any Rust type deserializing `via` must tolerate all six (an
  exhaustive enum built on the 1.4.0 vocabulary will fail to parse a hybrid result).

Field names, types, and the `q`/`limit`/`hybrid` query params are unchanged — the canonical flags
the change Minor for that reason — and no `engine_contract` Rust type changes shape, since
`GET /recall` is a route engine-rs *calls* as a client, not one `engine-serve` serves.

---

## Authoring checklist (when this contract bumps)

This is now the authoring checklist for engine-rs, the canonical repo (D78) — not a pointer to read
someone else's changelog.

1. Land the engine-rs-side implementation (Rust type or route change) first; the contract text
   documents shipped behavior, it does not precede it.
2. Update the **Contract Version** above and add a changelog row explaining the change and why the
   bump is MAJOR/MINOR/PATCH.
3. Update the field-mapping tables here to describe the new/changed shape.
4. Re-run `crates/engine-contract/tests/round_trip.rs` against a freshly captured fixture — do not
   hand-edit the fixture.
5. Note it in `planning/status.md`.
6. **Tell consumers to re-pin.** `orchestrator/docs/data-contract.md` and
   `bastion/docs/data-contract.md` each pin a version of this contract; when it bumps, both must
   update their own pinned-version line and field mappings to match. Engine-rs cannot edit those
   files directly (different repos, different lanes) — record the obligation (e.g. an `OPEN` item
   in the relevant orchestration-run notes) so it reaches the other lanes.

---

## Consumer re-pin obligations

This section is the fixture standing in for `EN.11.E`'s final acceptance criterion — "orchestrator/
and bastion/ re-pin to engine-rs as consumers" — which is itself declared `gateable: false`: its
evidence lives in two other repos' git indexes, and no engine-rs check (`cargo nextest`, `mev`,
`bastion validate-brain`) can read across a repo boundary to confirm it (D64). engine-rs's
obligation ends at leaving this canonical document correct and naming what the consumers owe;
engine-rs may not edit either consumer file directly (different repos, different lanes — see
`out_of_scope` on this block's record).

As of this 1.8.0 bump, both pinning consumers still declare 1.7.0 and must re-pin:

| Consumer file | Must move to | Authority |
|---|---|---|
| `core/orchestrator/docs/data-contract.md` (`**Contract Version: 1.7.0**` observed 2026-08-21) | `1.8.0` | [D78](file:///Users/brandon/Dev/agentic-portfolio/docs/decisions/D78-engine-rs-owns-the-data-contract.md) |
| `core/bastion/docs/data-contract.md` (`**Pinned Contract Version: 1.7.0**` observed 2026-08-21) | `1.8.0` | [D78](file:///Users/brandon/Dev/agentic-portfolio/docs/decisions/D78-engine-rs-owns-the-data-contract.md) |

Each re-pin must also absorb the campaign-identity addition documented above (§ Campaign identity)
into that consumer's own field-mapping tables. The obligation is carried to the other lanes as one
`OPEN` item per consumer repo in
`planning/orchestration-run/autonomous-foundation/notes.md` — see that file for the exact wording
and the observed-version disclaimer (an observation of another repo's working tree at a moment in
time, not a gate).

---

## Changelog

| Version | Date | Change |
|---|---|---|
| 1.0.1 | 2026-07-02 | Retroactive: `engine-contract`'s types (`EventsRow`, `TaskContext`, `NodeRun`, `NodeRunStatus`, `Usage`) were built matching canonical 1.0.1 during EN.0.B, but this consumer doc did not exist yet (Gap 2, `core/_planning/engine-rs/orchestrator-contract-conformance/notes.md`). Backfilled here rather than left undocumented. |
| 1.1.0 | 2026-07-16 | Re-pin to 1.1.0. Registers the canonical's v1.1.0 additions, both introduced by engine-rs's own EN.2.B: `POST /events/{run_id}/abort` (§ HTTP surface parity above) and the `metadata.cancellation` / `metadata.budget` run-level annotations (§ above). No `engine_contract` Rust type changed shape — both additions live in the existing `TaskContext::metadata: serde_json::Value` free-form field, per D6. |
| 1.2.0 | 2026-07-24 | Re-pin from 1.1.0 to 1.2.0 (`OR.Y`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.2.0 additions, none yet ported to `engine-serve`: the orchestrator's own `GET /events/{event_id}` read route and the `event_id` field on `POST /events/`'s 202 body (§ HTTP surface parity above), and the `metadata.failure` run-level annotation (§ Run-level `metadata` annotations above). No `engine_contract` Rust type changed shape — `metadata.failure` lives in the existing free-form `metadata` field, per D6. Porting the read route and `event_id` field to `engine-serve` for HTTP-surface parity is future work. |
| — | 2026-07-27 | Not a re-pin — no canonical contract change. `EN.7.B` registers two new `workflow_type` values POSTable at the existing `POST /events/` route: `OPPORTUNITY_SET_STAGE` and `OPPORTUNITY_ADD_ACTION` (see [opportunity-edit-workflows.md](workflows/opportunity-edit.md)). Both are engine-rs-side workflow types only — no new HTTP route, and no `engine_contract` Rust type changed shape (their event payloads are ordinary `data: serde_json::Value` on `POST /events/`, same as every other `workflow_type`). Pinned Contract Version stays 1.4.0. |
| 1.3.0 | 2026-07-24 | Re-pin from 1.2.0 to 1.3.0 (`OR.Q`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.3.0 additions, orchestrator-only (§ HTTP surface parity above): `POST /ingest/proposal` and `POST /ingest/artifact`, both `X-API-Key` gated with a typed `422` on malformed bodies. `/ingest/proposal` gives `EN.4.C`'s `PersistToBrainNode` a live endpoint matching the payload it already stubs — `{artifact_id, company_name, doc_type, section, content, roadmap}` → `200 {artifact_id, chunks_written}`. No `engine_contract` Rust type changed shape; these are ingest-direction routes engine-rs calls, not routes `engine-serve` serves, so no HTTP-surface-parity gap opens. `EN.4.C` (built, see [proposal-generator-workflow.md](workflows/proposal-generator.md)) still POSTs to a hardcoded placeholder URL rather than this route; wiring `PersistToBrainNode` to POST here for real remains open follow-on work. |
| 1.3.0 | 2026-07-27 | `EN.5.F` (engine-rs-side; not a canonical re-pin — **Pinned Contract Version stays 1.3.0**, this ports an already-canonical route rather than adding a new one). Ports the canonical v1.2.0 `GET /events/{event_id}` route and the `event_id` field on `POST /events/`'s `202` body into `engine-serve` (§ HTTP surface parity above): `POST /events/` now spawns the run and returns `202 {run_id, event_id}` immediately instead of awaiting it, and no longer returns `500` on a run failure — failure now surfaces through the `GET /events/{event_id}` readback (`status: "failed"`) and the terminal SSE frame. Adds `GET /events/{event_id}/stream`, an engine-rs-only SSE extension with no canonical counterpart. Sets a default HTTP-path `Budget` read from `ENGINE_RUN_MAX_COST_USD` (default `5.0`) / `ENGINE_RUN_MAX_TOKENS` (default unset). `LiveStateStore` now retains the most recent 100 completed runs (`COMPLETED_RUN_RETENTION`) in a bounded ring for the readback. No `engine_contract` Rust type changed shape. |
| 1.4.0 | 2026-07-27 | Re-pin from 1.3.0 to 1.4.0 (`OR.Q2`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.4.0 additions, orchestrator-only (§ HTTP surface parity above): `GET /recall`, `GET /walk`, `GET /pulse` — the read half of the D51 HTTP adapter whose write half (`POST /ingest/*`) landed in v1.3.0, thin `X-API-Key`-gated adapters over the orchestrator's `app/brain/` read core. No `engine_contract` Rust type changed shape; these are corpus-read routes engine-rs could call as a client, not routes `engine-serve` serves, so no HTTP-surface-parity gap opens. No engine-rs workflow calls any of the three today; wiring one (e.g. grounding a proposal draft via `GET /recall` before persisting through `POST /ingest/proposal`) remains open follow-on work. |
| — | 2026-07-28 | Not a re-pin — **Pinned Contract Version stays 1.4.0.** `EN.4.F` (engine-rs-side) changes the shape of the `roadmap` field `PersistToBrainNode` embeds in its `POST /ingest/proposal` payload (§ HTTP surface parity above): `AutomationRoadmap.recommendation.investment` moves from a model-authored free-text `String` to a structured `{currency, min, max, basis}` object (`locale::MoneyRange`), deterministically populated from the two-sheet, firewalled `RateCard` rather than invented by the model; `AutomationRoadmap` also gains an `authored_locale` field (`"pt-BR"` \| `"en-US"`) stamped from the run's requested locale. **No Pinned Contract Version bump**, for two reasons: (1) `roadmap` is documented in the canonical contract only as opaque `"the full structured AutomationRoadmap"` — its internal shape is engine-rs's own type, not one of the versioned `events`/`task_context`/`NodeRun`/`Usage` shapes this pin tracks, so no `engine_contract` Rust type changes; (2) `PersistToBrainNode` still POSTs to the hardcoded placeholder `BRAIN_INGEST_URL`, not Synapse's live `POST /ingest/proposal` route, so no real wire contract is broken today. The distinction matters for whoever wires the real endpoint next: when that happens, Synapse's ingest handler should expect the new `investment`/`authored_locale` shape, not the old free-text `investment` string — call that out explicitly in whatever change wires the real URL. See [proposal-generator-workflow.md](workflows/proposal-generator.md) for the full shape and the rate-card lookup that produces it. |
| — | 2026-07-30 | Not a re-pin — **Pinned Contract Version stays 1.4.0.** `EN.6.F` (engine-rs-side) adds three engine-rs-only routes with no canonical counterpart (§ HTTP surface parity above): `POST /events/{run_id}/pause`, `POST /events/{event_id}/resume`, and `GET /events/suspended`, plus the `metadata.suspension` run-level annotation (§ Run-level `metadata` annotations above) recording a suspended run's resume pointer, origin, and pre-suspend budget-ledger snapshot. Mirrors the abort (`EN.2.B`) and stream (`EN.5.F`) precedent exactly: no `engine_contract` Rust type changed shape, no new `NodeRunStatus` variant (D6) — `suspension` lives entirely in the existing free-form `TaskContext::metadata` field. `durable.rs`'s writer now upserts across a suspend/resume cycle rather than writing a single terminal update (§ above). See [suspend-resume.md](suspend-resume.md) for the full marker shape and both suspension origins. |
| 1.5.0 | 2026-08-01 | Re-pin from 1.4.0 to 1.5.0 (`OR.ticket.corpus-reconcile`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.5.0 addition, orchestrator-only (§ HTTP surface parity above): `POST /ingest/proposal` and `POST /ingest/artifact` gain an optional `authored_at: datetime \| null`, threaded to the written `brain_documents` rows. Additive and backward-compatible — omitted or `null` preserves the server-side `datetime.now()` fallback exactly, so `PersistToBrainNode`'s existing payload stays valid unchanged; sending a real `authored_at` is an opt-in improvement for `EN.6.K`'s ingest-client hardening. Sits alongside the 2026-07-28 `EN.4.F` row below (structured `investment` / `authored_locale` inside `roadmap`) rather than superseding it: whoever wires the real ingest URL should send both. No `engine_contract` Rust type changed shape — these are ingest-direction routes engine-rs calls, not routes `engine-serve` serves, so no HTTP-surface-parity gap opens. |
| 1.6.0 | 2026-08-01 | Re-pin from 1.5.0 to 1.6.0 (`OR.K2`, orchestrator-side; not an engine-rs block) — **the consequential one for `EN.6.K`**. `GET /recall`'s response semantics change (§ HTTP surface parity above): `score` is now a similarity where **higher is always better** on every path (`1.0` exact-id, `1.0 - cosine distance` semantic, unchanged fused similarity for hybrid), where 1.4.0 returned a raw cosine *distance* on the exact-id/semantic paths (`0.0` exact-id, lower-is-better); and `via`'s vocabulary widens from `exact-id \| semantic \| hybrid` to also include `structural \| keyword \| memory` (per-candidate hybrid provenance, previously collapsed to a bare `"hybrid"`). Field names, types, and the `q`/`limit`/`hybrid` query params are unchanged, and no `engine_contract` Rust type changes shape — `GET /recall` is a route engine-rs calls as a client, not one `engine-serve` serves. **`EN.6.K` (the Brain read-client seam — engine-rs's first `GET /recall` consumer, and therefore the first thing this polarity can bite) must be built against 1.6.0 semantics:** `RecallNode` sorts/thresholds `score` **descending / higher-is-better**, and any type deserializing `via` must tolerate all six values or it will fail to parse a hybrid result. A 1.4.0-era comparison direction ranks results backwards with no error — this re-pin exists specifically to close that window before `EN.6.K` runs. |
| 1.7.0 | 2026-08-13 | `EN.9.C` (engine-rs-side) adds the `metadata.completion` run-level annotation (§ Run-level `metadata` annotations above) as canonical contract text, stamped by `crate::completion::stamp_completion` at every terminal exit in `crates/engine-serve/src/suspend.rs`, plus an `engine-store` query (`list_orphan_candidates`) and an `engine-serve` boot sweep (`crate::orphan::reconcile_orphans`) that use the marker's absence to find and fail crash-stranded runs, and a stale-run alarm on age-past-threshold `running`/`suspended` runs. Mirrors the `cancellation`/`budget`/`suspension` precedent exactly: no `engine_contract` Rust type changed shape, no new `NodeRunStatus` variant (D6) — `completion` lives entirely in the existing free-form `TaskContext::metadata` field. **Corrected 2026-08-21 (D78, this task):** originally recorded as "Not a re-pin — Pinned Contract Version stays 1.6.0"; that was true only from the outgoing orchestrator-owned canonical's perspective. Engine-rs already implemented and shipped this annotation on 2026-08-13, so absorbing it into this now-canonical document is a version bump to 1.7.0, not new code. See [orphan-recovery.md](orphan-recovery.md) for the full marker shape, the sweep, the alarm, and the policy knobs. |
| — | 2026-08-24 | Not a re-pin — **Pinned Contract Version stays 1.8.0.** `EN.12.D` (engine-rs-side) adds one engine-rs-only route with no canonical counterpart (§ HTTP surface parity above): `GET /campaigns/{id}/journal`, reading a campaign's durable `JournalRow` decision log (`StepIntegrated`/`StepBailed`/`GateRefused`/`StateWriteVerificationFailed`/`BudgetHalted`/`ResolvedPolicy`) from Postgres via a new `engine-store::list_journal_rows_for_campaign`. Mirrors the pause/resume (`EN.6.F`) and abort (`EN.2.B`) precedent exactly: no `engine_contract::events` type changed shape — `JournalRow`/`JournalDecisionKind` are new, separate `engine-contract::journal` types, not additions to the existing `EventsRow`/`TaskContext`/`NodeRun` shapes this contract pins, so no re-pin is triggered. `durable.rs`'s writer channel widens to a `DurableItem::{Snapshot,Journal}` enum to carry both event-snapshot and journal writes (§ above); the self-skip-when-no-`DATABASE_URL` convention is preserved for journal writes and for the new read route alike. |
| 1.8.0 | 2026-08-21 | `EN.11.E` (engine-rs-side, D78 canonical) adds campaign identity (§ Campaign identity above) — a `campaign_id: uuid` first-class key naming the parent for the N runs of one chain, and a new `GET /campaigns/{id}` route (§ HTTP surface parity above) returning the campaign's runs with an `EN.11.G` cost/token rollup. **MINOR, not MAJOR**: the addition is a new key on the run's own `event`/`nodes` shape plus a wholly new, additive route; no existing field changes shape, and `NodeRunStatus`'s `pending|running|success|failed` vocabulary is untouched — the same reasoning 1.7.0's row above records for why that bump was minor. `campaign_id` is deliberately NOT a `TaskContext::metadata` annotation, unlike `cancellation`/`budget`/`completion`/`suspension`/`failure` — see § Campaign identity for why. `engine_contract`'s typed structs (`EventsRow`, `TaskContext`, `NodeRun`, `Usage`) are unchanged; the campaign id lives in the existing free-form `event: serde_json::Value` and `nodes: HashMap<String, serde_json::Value>` fields, not in a new Rust-typed column. Both pinning consumers (`orchestrator`/Synapse, `bastion`) still declare 1.7.0 as of this bump — see § Consumer re-pin obligations above and the corresponding `OPEN` items in `planning/orchestration-run/autonomous-foundation/notes.md`. |
| — | 2026-08-24 | Not a re-pin — **Pinned Contract Version stays 1.8.0.** `EN.6.K` task 3 (engine-rs-side, consumer-notes only) closes the two live client bugs flagged when task 1 registered `GET /recall`'s v1.4.0/v1.6.0 rows above. Both persist nodes (`proposal_generator::persist_to_brain` and `content_pipeline::persist_to_brain`) drop their hardcoded `BRAIN_INGEST_URL` placeholder consts in favor of `BrainConfig::from_env` (`BRAIN_API_URL`/`BRAIN_API_KEY`) and now send the `X-API-Key` header `POST /ingest/*` requires; `HarvestApproveNode`'s replayed POST does too. `content_pipeline::persist_to_brain::PersistToBrainNode` is re-pointed from the nonexistent `/ingest/learning` to the real `POST /ingest/artifact`, mapping the shared `LearningArtifact` shape into that route's generic envelope (§ HTTP surface parity above has the full field mapping). No `engine_contract` Rust type changed shape and no canonical route or field was added — this is engine-rs finally calling the routes Synapse has served since v1.3.0/v1.5.0, not a new contract surface, so the Pinned Contract Version does not move. |
| — | 2026-08-24 | Not a re-pin — **Pinned Contract Version stays 1.8.0.** `EN.4.D` registers a new `workflow_type` value POSTable at the existing `POST /events/` route: `DELIVERABLE_RENDER` (see [deliverable-render-workflow.md](workflows/deliverable-render.md)). Engine-rs-side workflow type only — no new HTTP route, and no `engine_contract` Rust type changed shape (its event payload, carrying an inline `AutomationRoadmap`, is ordinary `data: serde_json::Value` on `POST /events/`, same as every other `workflow_type`). Mirrors the 2026-07-27 `OPPORTUNITY_SET_STAGE`/`OPPORTUNITY_ADD_ACTION` row above. |
