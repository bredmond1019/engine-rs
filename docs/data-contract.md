---
type: Reference
title: engine-rs ⇄ Orchestrator Data Contract (Consumer)
description: engine-rs's pinned view of the orchestrator's versioned data contract — how each contract field maps to engine-rs's Rust types, and how engine-rs's own HTTP/write surface tracks the same contract it ports. The canonical contract lives in the orchestrator (Python) repo.
doc_id: data-contract
layer: [engine]
project: engine-rs
status: active
keywords: [data contract, orchestrator, PostgreSQL, node_runs, field mappings, v1.3.0, cancellation, abort, budget gate, engine-contract, event read api, ingest]
related: [architecture, D6-cancellation-and-budget-semantics, D20-shared-data-contract]
---

# Data Contract (Consumer View)

**Pinned Contract Version: 1.3.0**

The **canonical, authoritative** contract is owned by the orchestrator:
`orchestrator/docs/data-contract.md`. This file is engine-rs's *consumer* view — it pins the
version engine-rs is built against and maps each contract field to engine-rs's Rust types. When
the canonical contract bumps, re-pin the version here and update the mappings. The `/log-work`
checklist in both repos prompts this.

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

The canonical contract's v1.2.0 adds a third run-level annotation, `metadata.failure` — written by
the orchestrator's Celery worker when a workflow raises inside `process_incoming_event`, on a
fresh session that survives the enclosing transaction's rollback: `{ "failure": { "failed": true,
"error": "<ExcType>: <msg>", "at": "<iso8601>" } }`. Like `cancellation` and `budget`, it lives in
the existing `TaskContext::metadata: serde_json::Value` free-form field — no `engine_contract`
Rust type changes shape. engine-rs's own execution path (`Workflow::run_with`) does not yet stamp
`metadata.failure` on a raising run; whether it should is future work, not this re-pin — `§6`'s
`pending|running|success|failed` `NodeRunStatus` vocabulary is unchanged either way.

---

## HTTP surface parity

`engine-serve` (`crates/engine-serve/src/http.rs`, `abort.rs`) exposes the **same** routes as the
canonical contract's §7, so a caller can target either runtime:

| Method | Path | engine-rs handler |
|---|---|---|
| `POST` | `/events/` | `http::post_events` — `X-API-Key` gated, dispatches + records live state + enqueues the durable write |
| `GET` | `/health` | `http::health` |
| `GET` | `/workflows` | `http::list_workflows` |
| `GET` | `/workflows/{type}/graph` | `http::workflow_graph` — `404` for an unregistered type |
| `POST` | `/events/{run_id}/abort` | `abort::abort_run` (EN.2.B) — same `X-API-Key` gate; `401`/`404`/`202` per the canonical contract §7 |

The canonical contract's v1.2.0 adds a sixth route, `GET /events/{event_id}` (`X-API-Key` gated,
`404` for unknown/malformed ids, `200 {event_id, workflow_type, status, created_at, updated_at,
task_context}` with `status` derived server-side), implemented in the orchestrator's own Python
API (`OR.Y`) — **not** in `engine-serve`. `POST /events/` also gains an `event_id` field on its
`202` body there. Neither is ported to `engine-serve`/`http::post_events` by this re-pin; adding
the matching route and response field to engine-rs's own HTTP surface (to keep the two runtimes
interchangeable per the "same HTTP surface" goal above) is future work, tracked separately from
this pin.

`POST /events/{run_id}/abort` is backed by `abort::RunRegistry`, a per-run `CancellationToken`
registry: `post_events` mints and registers a token alongside the freshly-minted `run_id` before
running, and deregisters it once the run ends (success, failure, or cancellation) so a later abort
against a finished `run_id` correctly 404s rather than triggering a token nobody checks anymore.

The canonical contract's v1.3.0 adds two more routes, `POST /ingest/proposal` and
`POST /ingest/artifact` (`OR.Q`), implemented only in the orchestrator's own Python API
(`app/api/` — mounted beside `/events`, `/health`, `/workflows`) — **not** in `engine-serve`, and
not planned to be: these are ingest-direction routes engine-rs *calls*, not routes it needs to
serve for runtime interchangeability. `/ingest/proposal` is pinned exactly to the payload
`EN.4.C`'s `PersistToBrainNode` (`crates/engine-core/src/workflows/proposal_generator/persist_to_brain.rs`,
built) asserts against a stub — `{ artifact_id, company_name, doc_type, section, content, roadmap }` —
returning `200 { artifact_id, chunks_written }`; both routes reuse the same `X-API-Key` gate as
`POST /events/` and reject a malformed body with a typed `422` (never `500`). `PersistToBrainNode`
now has a live target to POST to instead of its `HttpPost` stub, but still POSTs to a hardcoded
placeholder `BRAIN_INGEST_URL` constant rather than this route — pointing it at the real Synapse
`/ingest/proposal` endpoint is unfinished follow-on work (see `planning/decisions/D9-engine-brain-boundary.md`).

---

## Re-pin checklist (when the canonical contract bumps)

1. Read the canonical changelog (`orchestrator/docs/data-contract.md` § Versioning); update the
   **Pinned Contract Version** above.
2. Update the field-mapping tables here.
3. Update affected Rust types (`engine-contract::events`, `engine-contract::task_context`) and, if
   the shape change is behavioral, the corresponding `engine-core`/`engine-serve` logic.
4. Re-run `crates/engine-contract/tests/round_trip.rs` against a freshly captured orchestrator
   fixture (see `docs/scripts.md` in the orchestrator repo for the emit script) — do not hand-edit
   the fixture.
5. Note it in `planning/status.md`.

---

## Changelog (this pin)

| Pinned At | Date | Change |
|---|---|---|
| 1.0.1 | 2026-07-02 | Retroactive: `engine-contract`'s types (`EventsRow`, `TaskContext`, `NodeRun`, `NodeRunStatus`, `Usage`) were built matching canonical 1.0.1 during EN.0.B, but this consumer doc did not exist yet (Gap 2, `core/_planning/engine-rs/orchestrator-contract-conformance/notes.md`). Backfilled here rather than left undocumented. |
| 1.1.0 | 2026-07-16 | Re-pin to 1.1.0. Registers the canonical's v1.1.0 additions, both introduced by engine-rs's own EN.2.B: `POST /events/{run_id}/abort` (§ HTTP surface parity above) and the `metadata.cancellation` / `metadata.budget` run-level annotations (§ above). No `engine_contract` Rust type changed shape — both additions live in the existing `TaskContext::metadata: serde_json::Value` free-form field, per D6. |
| 1.2.0 | 2026-07-24 | Re-pin from 1.1.0 to 1.2.0 (`OR.Y`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.2.0 additions, none yet ported to `engine-serve`: the orchestrator's own `GET /events/{event_id}` read route and the `event_id` field on `POST /events/`'s 202 body (§ HTTP surface parity above), and the `metadata.failure` run-level annotation (§ Run-level `metadata` annotations above). No `engine_contract` Rust type changed shape — `metadata.failure` lives in the existing free-form `metadata` field, per D6. Porting the read route and `event_id` field to `engine-serve` for HTTP-surface parity is future work. |
| 1.3.0 | 2026-07-24 | Re-pin from 1.2.0 to 1.3.0 (`OR.Q`, orchestrator-side; not an engine-rs block). Registers the canonical's v1.3.0 additions, orchestrator-only (§ HTTP surface parity above): `POST /ingest/proposal` and `POST /ingest/artifact`, both `X-API-Key` gated with a typed `422` on malformed bodies. `/ingest/proposal` gives `EN.4.C`'s `PersistToBrainNode` a live endpoint matching the payload it already stubs — `{artifact_id, company_name, doc_type, section, content, roadmap}` → `200 {artifact_id, chunks_written}`. No `engine_contract` Rust type changed shape; these are ingest-direction routes engine-rs calls, not routes `engine-serve` serves, so no HTTP-surface-parity gap opens. `EN.4.C` (built, see [proposal-generator-workflow.md](proposal-generator-workflow.md)) still POSTs to a hardcoded placeholder URL rather than this route; wiring `PersistToBrainNode` to POST here for real remains open follow-on work. |
