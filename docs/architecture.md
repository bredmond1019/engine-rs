---
type: Reference
title: engine-rs Architecture
description: Module map, core types, and data flow for engine-rs — Bastion's native Rust execution engine.
doc_id: architecture
layer: [engine, console]
project: engine-rs
status: active
keywords: [architecture, module map, core types, data flow, Rust, workflow runtime, locale, rate card]
related: [docs-index, context, master-plan]
---

# engine-rs — Architecture

## Overview

`engine-rs` is a graph-validated workflow runtime that embeds directly in the `bastion serve`
daemon. It holds live run state in-memory (Engine and Console share one process, one language —
no DB poll on the local hot path) and asynchronously persists the data-contract `events` row to
Postgres at node boundaries as a durable record for crash-recovery, history, and remote-observer
catch-up (D42). It is a parallel-pilot rewrite of the Python `orchestrator` engine core
(`orchestrator/app/core/`), not a fork.

## Module Map

The Cargo workspace (EN.0.A) declares the member crates below. `engine-core` (EN.1.A),
`engine-contract` (EN.0.B), `engine-store` (EN.0.B), and now `engine-serve` (EN.1.C) hold real
types: dispatch, in-memory live state, the durable-write bridge, and the actix-web HTTP surface.
`term-core` and `term-attach` (`EN.9.A`) are two more members — tmux session-control ported from
`core/bastion`, split so the blocking attach path can never be pulled into `engine-core`/
`engine-serve` by additive feature unification. `term-core` (`tokio` feature) is now a real
`engine-core` dependency (`EN.9.D`) — see the `nodes/terminal/` entry below and
[terminal-driver.md](terminal-driver.md); `term-attach` is still linked from neither binary. See
[terminal-crates.md](terminal-crates.md).

```
engine-rs/
├── Cargo.toml            (workspace root — resolver 2, workspace.package, workspace.dependencies)
├── crates/
│   ├── engine-core/       ← node.rs (Node trait + NodeRegistry + as_router() hook; now also
│   │                         `Identified<N>`/`NodeExt::with_identity` for instance-backed node
│   │                         identity and `InputBinding`/`WithInput<N>`/`NodeExt::with_input_from`
│   │                         for declarative upstream-input bindings, EN.5.E task 1),
│   │                         loop_combinator.rs (`build_loop(LoopSpec) -> LoopCluster` — the
│   │                         reusable `{guard router, increment node, back-edge}` bounded-loop
│   │                         builder, generalizing the hand-written `sdlc_flow` retry idiom,
│   │                         EN.5.E task 2), dispatch.rs (Dispatcher — dual `workflow_registry` +
│   │                         `schema_registry` lookup by `workflow_type`, `WorkflowFactory`,
│   │                         `DispatchError::UnknownWorkflowType`/`PolicyResolutionFailed`; moved
│   │                         here from `engine-serve` in EN.5.E task 3, which now re-exports it),
│   │                         schema.rs
│   │                         (WorkflowSchema/NodeConfig), workflow.rs (Workflow pointer-walk
│   │                         runner + on_progress seam + Router-aware dispatch + new_validated() +
│   │                         run_with() cancellation/budget-aware entry point, EN.2.B),
│   │                         routing.rs (Router trait + dispatch_route()), parallel.rs
│   │                         (ParallelNode fan-out/merge), validate.rs (WorkflowValidator graph
│   │                         validator), cancellation.rs (CancellationToken, watch-backed,
│   │                         + stamp_cancelled(), EN.2.B), budget.rs (Budget config + BudgetLedger
│   │                         + pre-dispatch check() gate, EN.2.B), nodes/ (claude_code_step.rs —
│   │                         ClaudeCodeStep, a reusable Node wrapping core/claude-code-rs's
│   │                         execute(), EN.2.A; now cancellation-aware via
│   │                         with_cancellation_token(), EN.2.B; http_post.rs — the injectable
                         `HttpPost` trait seam + `reqwest`-backed live impl + `StubHttpPost` test
                         double, EN.4.C, used by `proposal_generator::PersistToBrainNode` to POST
                         a finished artifact to Synapse's brain-ingest endpoint; channel_transport.rs
                         — the injectable `ChannelTransport` egress seam `ActionDispatchNode` (EN.6.A)
                         and `research_agent::ResearchIngressDispatchNode` (EN.6.E) call to deliver
                         outbound actions to the channel that originated them / self-feed
                         `CONTENT_PIPELINE`;
                         doc_materializer.rs — the injectable `DocMaterializer` seam
                         `MaterializeDocNode` calls to plan + write a `BrainDocModel`-shaped
                         artifact into the Brain corpus via `mev`/`okf-core` in-process, live impl
                         + `StubDocMaterializer` test double, EN.7.A task 3 (extended EN.7.B task 2
                         with `edit_opportunity`/`OpportunityEdit::{SetStage,AddAction}` over
                         mev's `plan_set_stage`/`plan_add_action`); materialize_doc.rs —
                         `MaterializeDocNode` itself, the generic writer node every future pipeline
                         appends, EN.7.A task 4 (gains an ordered `with_source_nodes` upstream
                         read-preference, EN.7.B task 1); opportunity_edit.rs —
                         `OpportunityEditNode`, the generic node driving `edit_opportunity` for a
                         configured `OpportunityEditOp` (`SetStage`/`AddAction`) read off
                         `ctx.event`, EN.7.B task 3), terminal/ — `TerminalSessionNode`
                         (`session.rs`: ensure/stamp/lease-acquire a tmux session via `term-core`'s
                         `TerminalDriver`) + `TerminalObserveNode` (`observe.rs`: capture_pane +
                         `term-core::detect`, then `pane.rs`'s `bound_pane_tail` bound/redact/hash),
                         plus `identity.rs` (`session_name_for`, the `HasSessionInput` builder
                         trait) and `pane.rs` (pure pane-bounding/redaction helpers), EN.9.D; plus
                         `predicate.rs` (`AwaitPredicate` — `Marker`/`Detect`/`Regex`/`Silence`/
                         `ExitCode` — and the pure `evaluate()` over a caller-collected
                         `Observation`, with `marker_path(out, nonce)` the single source of truth
                         for the `{out}.{nonce}.done` marker format), `send.rs`
                         (`TerminalSendNode` — org-floor command refusal, `SessionLease::renew`
                         re-verification, a per-session `tokio::sync::Mutex` held across the
                         check+send, and `send_id` back-edge idempotency recorded in a tmux
                         user-option), and `await_node.rs` (`TerminalAwaitNode` — a bounded,
                         cancellable poll over `AwaitPredicate` with its own timeout and a
                         four-layer-resolved `AwaitPolicy`/`poll_interval_ms`/`timeout_ms`, stamped
                         into `ctx.nodes` on every non-cancelled return), EN.9.E), brain_root.rs (`resolve_brain_root`/
                         `resolve_brain_root_from` — `ENGINE_BRAIN_ROOT` env var, else
                         `mev::brain::config::find_brain_root` walking up from cwd for
                         `brain.toml`, typed `BrainRootError`, EN.7.A task 2), locale.rs —
                         `Locale`/`Currency`/`MoneyRange`/`RateSheet`/`RateCard`, the two-sheet,
                         firewalled rate card + language directive threaded through the
                         diagnostic funnel's event schemas, EN.4.F), suspend.rs —
│   │                         `PauseSignal` (a clearable, watch-backed two-way flag,
│   │                         deliberately not `CancellationToken`) + the
│   │                         `metadata.suspension` marker (`stamp_suspended`/
│   │                         `stamp_resumed`/`request_suspension`/`read_suspension`/
│   │                         `is_suspended`), the convergence point for both
│   │                         suspension origins (operator pause, `SuspendNode`),
│   │                         EN.6.F task 1; `nodes/suspend.rs` — `SuspendNode`, the
│   │                         workflow-authored half of suspend/resume (`enabled`/
│   │                         `with_predicate`/`with_reason_label`, default-off
│   │                         in-place no-op patterned on `MaterializeDocNode::
│   │                         with_enabled`), only *requests* suspension via
│   │                         `suspend::request_suspension` — finalizing the walk
│   │                         stop belongs to `Workflow::walk`, EN.6.F task 5; `nodes/fan_out.rs`
│   │                         — `FanOutNode` (`EN.6.G` task 1), builds N `with_identity`-wrapped
│   │                         instances of one node type via a builder closure and runs them
│   │                         through `ParallelNode`, plus an `impl Node for Box<dyn Node>` (added
│   │                         here, not `node.rs`) so `NodeExt::with_identity` is callable on the
│   │                         builder's boxed output; `FanOutNode::branch_identity(base_name, i)` is
│   │                         the public `"{base_name}[{i}]"` helper `AggregateNode::for_fan_out`
│   │                         reuses to derive matching identities; `nodes/aggregate.rs` —
│   │                         `AggregateNode`, joins N `ctx.nodes` entries into one
│   │                         deterministically-ordered array by declared identity order (not
│   │                         `HashMap` iteration order); evals/ (`EN.5.B` tasks 1-3) — pure,
│   │                         corpus-free eval scoring generalized from Synapse's OR.K2 scorer
│   │                         library: scorers.rs (`score_deterministic`/`score_structural`/
│   │                         `score_reference_based`, free functions over `serde_json::Value`/
│   │                         `&str` returning `ScoreResult`), case.rs (`EvalCase` — `ScorerKind`
│   │                         + dot-path selector + expected value), slice.rs (`EvalSlice` —
│   │                         named `EvalCase` collection grouped by domain/model/profile
│   │                         mirroring `PolicyAggregate`'s grouping shape; `EvalSlice::score`
│   │                         produces per-case `CaseReport`s and an overall pass-rate in
│   │                         `SliceReport`), runner.rs (`run_slice` — scores an `EvalSlice`
│   │                         against real captured SDLC-flow telemetry by importing `EN.4.0`'s
│   │                         `aggregate_state_files`/`extract_policy_telemetry` directly, no
│   │                         second aggregation path; reduces the resulting `PolicyAggregate`
│   │                         rows to one JSON record via a field-less `UnitPolicy` grouping
│   │                         key; `coding_slice()` — a concrete slice scoring
│   │                         `PolicyAggregate`'s own serialized fields))
│   ├── engine-contract/   ← data-contract serde types (events.rs: EventsRow/NodeRun/
│   │                         NodeRunStatus/Usage; task_context.rs: TaskContext), matching
│   │                         orchestrator data-contract.md v1.1.0 byte-for-byte (see
│   │                         docs/data-contract.md for the full pin)
│   ├── engine-store/      ← postgres.rs: sqlx::PgPool connect/insert_event/update_event/
│   │                         get_event for the durable `events` record
│   └── engine-serve/      ← bastion serve embedding (EN.1.C): dispatch.rs (a thin re-export of
│   │                         `engine_core::dispatch` — Dispatcher/DispatchError/WorkflowFactory
│   │                         moved to `engine-core` in EN.5.E task 3 so every existing
│   │                         `engine_serve::dispatch::*` import site keeps resolving unchanged),
│   │                         live_state.rs (LiveStateStore —
│   │                         in-memory Arc<RwLock<HashMap<RunId, TaskContext>>> record/get/
│   │                         list_active/remove, no-DB-poll read path for the local Console; now
│   │                         also mark_terminal/get_record, EN.5.F — moves a finished run out of
│   │                         the live map into a bounded 100-entry completed ring so a terminal
│   │                         run's snapshot survives for HTTP readback),
│   │                         durable.rs (DurableHandle/spawn_durable_writer/durable_on_progress —
│   │                         mpsc-bridged async writer mapping on_progress TaskContext snapshots to
│   │                         engine_contract::EventsRow, inserting the first PENDING snapshot per
│   │                         run and updating subsequent ones; self-skips Postgres I/O when no
│   │                         pool/DATABASE_URL is configured), http.rs (actix-web surface: POST
│   │                         /events/ with X-API-Key gating dispatch + live-state + durable-write
│   │                         (now spawns the run and returns 202 {run_id, event_id} immediately,
│   │                         EN.5.F), GET /health, GET /workflows, GET /workflows/{type}/graph,
│   │                         GET /events/{event_id} (server-derived status readback, EN.5.F)),
│   │                         stream.rs (GET /events/{event_id}/stream — per-run
│   │                         tokio::sync::broadcast SSE tee with a terminal-frame cache for late
│   │                         subscribers, EN.5.F), abort.rs
│   │                         (POST /events/{run_id}/abort, X-API-Key gated, EN.2.B — backed by a
│   │                         per-run CancellationToken RunRegistry minted/registered/deregistered
│   │                         around each post_events run_with call), suspend.rs (process-global
│   │                         pause-signal map + bounded FIFO suspended-run index behind
│   │                         `OnceLock<RwLock<..>>`, `take_for_resume`/`clear_resuming`'s
│   │                         atomic read-and-set double-resume guard, and `spawn_run` — the one
│   │                         shared spawn/exit-fork every trigger AND resume handler calls so the
│   │                         terminal-vs-suspended cleanup logic never drifts between the two
│   │                         entry points, EN.6.F task 8/9/10), resume.rs (the run-control surface
│   │                         is now pause/resume/abort rather than abort alone: POST
│   │                         /events/{run_id}/pause, POST /events/{event_id}/resume — rehydrates
│   │                         the Workflow from the ORIGINAL trigger payload via
│   │                         `without_seeded_nodes()`, rebuilds the BudgetLedger from the
│   │                         marker's snapshot, and continues from the stored `resume_at`
│   │                         pointer — and GET /events/suspended (registered before
│   │                         `{event_id}` so the literal path isn't swallowed by the uuid
│   │                         extractor), EN.6.F task 11), schedule.rs (`EN.6.G` task 2 — `ScheduleEntry`/
│   │                         `ScheduleRegistry`, a thin adapter over `engine_core::cron`'s `tick()`;
│   │                         `load_schedule_entries` reads `planning/harness.json`'s
│   │                         `schedule.entries[]`; `dispatch_scheduled_entry` builds a
│   │                         `Schedule`-typed `IngressEnvelope` per fire and dispatches it in-process
│   │                         via `dispatch_with_event` + `spawn_run` — no self-directed HTTP call;
│   │                         see [§ Schedule Source](#schedule-source-en6g) below and
│   │                         [cron-primitive.md](cron-primitive.md))
│   ├── term-core/          ← tmux session-control + agent-detection, ported verbatim from
│   │                         `core/bastion`'s `src/sessions/{tmux,model,claude_state}.rs` and
│   │                         `src/detect/` (`EN.9.A`) — no `attach_session`/`suspend_and_attach`,
│   │                         no `anyhow`; not linked by `engine-core` or `engine-serve` in this
│   │                         block (that wiring is `EN.9.B`). See
│   │                         [terminal-crates.md](terminal-crates.md)
│   └── term-attach/        ← `attach_session`/`suspend_and_attach` only — split from `term-core`
│                              so no cargo feature-unification path can ever pull the blocking
│                              attach code into the async `engine-core`/`engine-serve` binary
│                              (`EN.9.A`). See [terminal-crates.md](terminal-crates.md)
└── tests/                 ← round-trip + integration fixtures
    (crates/engine-core/tests/workflow_runner.rs — fixture 3-node linear workflow integration test;
    crates/engine-core/tests/parallel.rs — ParallelNode fan-out/merge integration tests;
    crates/engine-core/tests/validator.rs — WorkflowValidator + router-aware Workflow::run
    integration tests (valid/rejected schemas, router back-edge dispatch);
    crates/engine-contract/tests/round_trip.rs — fixture byte-for-byte serde round-trip;
    crates/engine-store/tests/postgres_round_trip.rs — `#[ignore]`d live Postgres round-trip (CI has
    no Postgres, per EN.0.A); run explicitly with `cargo test -p engine-store -- --ignored` and
    `DATABASE_URL` set — an unset `DATABASE_URL` at that point is a hard failure, not a silent skip;
    crates/engine-serve/tests/dispatch_integration.rs — headline EN.1.C integration test: live-state
    read with no DB query, byte-identical durable EventsRow mapping for a fixture 2-node workflow,
    and 422 for an unregistered workflow_type;
    crates/engine-core/tests/it/fan_out_aggregate.rs (module of the single `tests/it/main.rs`
    binary, per CLAUDE.md rule 8) — `EN.6.G` task 3: a real `Workflow::new_validated` + `.run()`
    graph (FanOut -> Aggregate -> a persist stub node) proving no last-write-wins collision across
    same-type fan-out branches;
    crates/engine-serve/tests/schedule.rs — `EN.6.G` task 3: a `ScheduleRegistry.tick()` fire
    dispatching one persist-shaped payload and one outbound-action-shaped record through the
    non-blocking `spawn_run` path, over a real tempdir-backed `FileCronStore`;
    crates/engine-core/tests/it/evals_slice.rs (module of the single `tests/it/main.rs` binary,
    per CLAUDE.md rule 8) — `EN.5.B` task 3: proves `run_slice` against a fixture
    `tests/fixtures/eval_coding_state.json` SDLC-flow state file, scoring `coding_slice()`
    through the real `aggregate_state_files` path end-to-end)
```

## Injectable Seams

Three `crates/engine-core/src/nodes/*` seams share one shape — a trait, a live implementation
backed by the real external dependency, and a recording test stub — so production callers reach
for the live impl while the gated `cargo test` suite injects the stub and never performs the real
I/O:

| Seam | Trait / live impl | Test stub | Used by | Boundary |
|---|---|---|---|---|
| `http_post.rs` (`EN.4.C`; harvest-gated `EN.7.C`) | `HttpPost` / `ReqwestHttpPost` (`http_post_live()`) | `StubHttpPost` | `proposal_generator::PersistToBrainNode`, `content_pipeline::PersistToBrainNode`, `nodes::harvest_approve::HarvestApproveNode` | POSTs a finished artifact to Synapse's brain-ingest endpoint (`OR.Q`); as of `EN.7.C`, `content_pipeline::PersistToBrainNode`'s push is governed by `nodes::harvest_gate::HarvestGate` (`off`/`in_process`/`approval`, built-in default `off`) — see [harvest-gate.md](harvest-gate.md); as of `EN.8.A`, that same `HarvestGate` also declares an `operator::OperatorChannel` (`notification`/`session-<slug>`) — see [operator-payload-contract.md](operator-payload-contract.md) |
| `channel_transport.rs` (`EN.6.A`) | `ChannelTransport` / `channel_transport_live()` | `StubChannelTransport` | `content_pipeline::ActionDispatchNode`, `research_agent::ResearchIngressDispatchNode` (`EN.6.E`) | Delivers outbound actions (digest replies, workflow-trigger chaining) back to the channel that originated the run, or self-feeds a finished `RESEARCH_AGENT` run into `CONTENT_PIPELINE` |
| `doc_materializer.rs` (`EN.7.A`, edit ops `EN.7.B`/`EN.4.E`) | `DocMaterializer` / `MevDocMaterializer` (`doc_materializer_live()`) | `StubDocMaterializer` | `nodes::MaterializeDocNode`, `nodes::OpportunityEditNode` (`EN.7.B`), `nodes::merge_contacts::MergeContactsNode` (`EN.4.E`) | Plans + writes a `BrainDocModel`-shaped artifact into the Brain corpus as a source `.md` document via `mev`/`okf-core` in-process (D53's fourth boundary-test channel); `EN.7.B` extends the seam with `edit_opportunity` (`OpportunityEdit::SetStage`/`AddAction`, over mev's `plan_set_stage`/`plan_add_action`) for editing an already-written opportunity; `EN.4.E` adds a third `OpportunityEdit::MergeContacts` variant (over mev's `plan_merge_contacts`) so `RESEARCH_AGENT`'s `MergeContactsNode` can merge extracted contacts into an already-written opportunity |
| `crates/engine-serve/src/orphan.rs` (`EN.9.C`) | `OrphanLister` / `PgOrphanLister` (`orphan_lister_live()`) | `RecordingOrphanLister` | `crate::orphan::reconcile_orphans` (the boot sweep) | Lists `events` rows whose `task_context.metadata.completion` is absent past a policy-resolved age (`engine-store::list_orphan_candidates`), so the crash-recovery sweep is testable with no database; not one of the three `crates/engine-core/src/nodes/*` seams above — it lives in `engine-serve` and lists rows rather than dispatching an action. See [orphan-recovery.md](orphan-recovery.md) |

See [materialize-doc-node.md](materialize-doc-node.md) for the `DocMaterializer` seam and
`MaterializeDocNode` in detail, [opportunity-edit-workflows.md](opportunity-edit-workflows.md) for
the `edit_opportunity` operation and `OpportunityEditNode`,
[content-pipeline-workflow.md](content-pipeline-workflow.md) for `HttpPost` and `ChannelTransport`
in their workflow context, and [harvest-gate.md](harvest-gate.md) for the `HarvestMode`/
`HarvestGate` gate fronting the `http_post.rs` seam and the `HARVEST_APPROVE` completion
micro-workflow. The operator-facing half that *drives* those pending records —
`APPROVE_AND_RUN` (`EN.8.D`), which drains them into the depth-limited operator queue, records each
decision in the approval ledger, and executes only a matched-digest approval — is documented in
[approve-and-run-workflow.md](approve-and-run-workflow.md).

**Materialize -\> harvest ordering guarantee.** In `CONTENT_PIPELINE`, `MaterializeDocNode` always
runs upstream of `PersistToBrainNode` in the declared graph, and the harvest gate never changes
that order or `MaterializeDocNode`'s own behavior: the materialized `.md` is written identically
in every harvest mode (`off`/`in_process`/`approval`). The gate only changes what
`PersistToBrainNode` does with the finished payload afterward — push now, skip (rely on the
freshness reindex), or defer to a `pending` record for `HARVEST_APPROVE`. A failed harvest push
therefore never costs the run its already-written source document (D53).

## Schedule Source (`EN.6.G`)

`crates/engine-serve/src/schedule.rs` turns a durable cron fire (`engine_core::cron`, `EN.6.M`)
into a workflow dispatch, without any HTTP self-call:

- `ScheduleEntry` — one registered entry: its normalized `CronSchedule`, target `workflow_type`,
  optional `profile`, and caller-supplied `data` merged into the dispatched event.
- `load_schedule_entries(harness_path)` — reads `planning/harness.json`'s `schedule.entries[]`
  array (a sibling `_comment` key documents the knob, matching the existing
  `<workflow_key>.profiles` convention), normalizing each entry's `cron_expr`/`timezone`/`every_ms`
  via `engine_core::cron::normalize_schedule`. A missing file or missing `schedule` key is an empty
  `Vec`, not an error; a present-but-malformed one is `LoadScheduleError`.
- `build_seeded_registry(harness_path, store_path)` — the seeding caller
  (`EN.ticket.cron-schedule-startup-wiring`): opens a `FileCronStore`, upserts one `CronRecord` per
  loaded entry using the **load-time** `next_fire_at` anchor (`ScheduleEntry.next_fire_at`;
  recomputing it against a later `now` would skew the first fire), and registers each entry's
  workflow metadata. **Re-seeding a store that already holds a record for the same `cron_id`
  preserves that record's `last_fired_at`/`next_fire_at`**, so a restart neither re-fires nor skips.
- `spawn_schedule_loop(harness_path, state)` — the interval driver, mirroring
  `crate::durable::spawn_durable_writer`'s "engine-serve owns the loop, the embedder calls it" seam.
  `tokio::spawn`s a `tokio::time::interval` loop that runs each `tick` on `spawn_blocking` (the tick
  persists to disk) and returns `Ok(Some(ScheduleLoopHandle))`. **Zero configured entries returns
  `Ok(None)` and spawns nothing at all.** Two knobs, both read from `harness.json`'s `schedule`
  block with behavior-stable defaults: `poll_interval_ms` (default `15_000`, deliberately under the
  `60_000ms` floor `every_ms` enforces so the fastest legal entry cannot be missed) and `store_path`
  (default: `cron-store.json` beside `harness.json`).
- `ScheduleRegistry` — wraps a `CronStore` (seeded with one `CronRecord` per entry via
  `FileCronStore::upsert`, since the `CronStore` trait itself has no insert/upsert by design) plus
  the `ScheduleEntry` metadata attached via `register`. `ScheduleRegistry::tick(now, dispatch)`
  delegates every firing/catch-up mechanic to `engine_core::cron::store::tick`, calling `dispatch`
  for each due, registered entry (a due record with no matching registration fires
  `FireOutcome::Silent` defensively rather than panicking).
- `dispatch_scheduled_entry(state, entry, fired_at)` — builds a `Schedule`-typed `IngressEnvelope`
  (`SourcePayload::WorkflowTrigger { workflow_type, event }`) and dispatches it through the exact
  non-blocking sequence `crate::http::post_events` uses: `dispatch_with_event` -> mint `run_id` ->
  register cancellation token + pause signal -> `crate::suspend::spawn_run` — never a self-directed
  HTTP call. Always returns `FireOutcome::Reported` (naming the run on success or the dispatch
  failure otherwise), never `Silent` — a dispatch attempt always has something to report.

`ScheduleRegistry` is deliberately **not** an `AppState` field — it follows the existing
`default_budget_from_env`/`live_run_metadata` precedent (process-global, not struct fields) to
avoid an immediate cross-repo compile break for `bastion`, which constructs `AppState` over an
unpinned path dependency. See [cron-primitive.md](cron-primitive.md) for the underlying
`CronSchedule`/`CronStore`/`tick()` primitive and `crates/engine-serve/tests/schedule.rs` for the
end-to-end proof (one `ScheduleRegistry.tick()` fire dispatching one persist-shaped payload and one
outbound-action-shaped record through `spawn_run`, plus a configured entry driven all the way to an
observed dispatch through `spawn_schedule_loop`).

> **The loop is spawnable but unspawned.** No live process calls `spawn_schedule_loop` — the call
> site belongs in `bastion`'s `serve/mod.rs` alongside `spawn_durable_writer`, which is a different
> repo and therefore a separate block. Until it lands, a configured `schedule.entries[]` entry never
> fires, with no error and no log line. Anything downstream that depends on a schedule (e.g. the
> deferred newsletter digest) needs that block too, not just this one.

## Build & CI

Async runtime + persistence: `tokio` + `sqlx` (postgres, runtime-tokio, tls-rustls) — see
`planning/decisions/D2-async-runtime-choice.md`. `engine-store` carries `sqlx` as a real
dependency for its Postgres layer; `engine-contract` carries `chrono`/`uuid` for the data-contract
types; `engine-core` carries `async-trait` and `futures` as real dependencies (EN.2.0), and `tokio` as a real dependency
(promoted from dev-only in EN.2.B — `CancellationToken::cancelled()` is public async API, not
test-only code; see `planning/decisions/D6-cancellation-and-budget-semantics.md`).
`engine-serve` (EN.1.C) now carries `chrono`, `sqlx`, `actix-web`, and
`async-trait` (EN.2.0) as real dependencies alongside `tokio` — `actix-web` is the HTTP framework
choice, see `planning/decisions/D3-http-framework-choice.md`. `engine-core` also carries
`claude-code-rs` (a workspace path dependency on the sibling `core/claude-code-rs`) as a real
dependency (EN.2.A) — see `planning/decisions/D4-claude-code-transport-choice.md`.

CI (`.github/workflows/ci.yml`) runs on every push (all branches) and on pull requests, running
the same four gate commands as `planning/harness.json`: `cargo fmt --check`,
`cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.

## Core Types

- `Node` (trait, `engine-core::node`, `#[async_trait::async_trait]`) — `async fn process(&self, ctx:
  TaskContext) -> Result<TaskContext, NodeError>` + `fn name(&self) -> &str`; identity = the
  implementing type's own `name()` string, ported from `orchestrator/app/core/nodes/base.py`.
  Bounded `Send + Sync` so boxed trait objects work across async boundaries (EN.2.0).
- `NodeError` (`engine-core::node`) — a `{ message: String }` struct implementing `Display` +
  `std::error::Error`; carried into the node's `NodeRun.error` on failure.
- `NodeRegistry` (`engine-core::node`) — `HashMap<String, Box<dyn Node>>` keyed by `Node::name()`,
  with `register`/`get`/`contains`/`len`/`is_empty`, so the runner can resolve the next node to
  execute by identity string.
- `WorkflowSchema` / `NodeConfig` (`engine-core::schema`, serde `Serialize`/`Deserialize`) — the
  declarative graph description: `WorkflowSchema { workflow_type, start_node, nodes:
  HashMap<String, NodeConfig> }`; `NodeConfig { identity, connections: Vec<String> }` with a
  `next()` helper returning `connections[0]`. `WorkflowSchema::start()` resolves the start node's
  `NodeConfig`; `next_after(identity)` resolves a node's `connections[0]` next-node identity.
  Plain nodes still walk only `connections[0]`; router nodes (below) select the next node at
  runtime instead, including undeclared back-edges.
- `Router` (trait, `engine-core::routing`, supertrait of `Node`) — `fn route(&self, ctx:
  &TaskContext) -> Option<String>` for runtime next-node selection; `Node::as_router(&self) ->
  Option<&dyn Router>` is a default `None` hook nodes override to be detected by the registry as
  routers. `dispatch_route(&dyn Router, &TaskContext) -> Option<String>` is a thin dispatch
  helper wrapping `router.route(ctx)`.
- `ParallelNode` (`engine-core::parallel`) — fans out over a declared `Vec` of branch nodes via
  `futures::future::join_all` (EN.2.0; polled in-place on the current task, so borrowed
  `&self.branches` needs neither `Send` nor `'static`), deep-copies the `TaskContext` per branch,
  and merges `nodes`/`node_runs` back with deterministic last-write-wins semantics (later branch
  in declared order wins on key collision); the first branch `NodeError` encountered in declared
  order is propagated as the `ParallelNode`'s own error, with no partial merge on branch failure.
- `FanOutNode` / `AggregateNode` (`engine-core::nodes::fan_out` / `engine-core::nodes::aggregate`,
  `EN.6.G` task 1) — the fan-out/join pair for running N instances of *one* node type in parallel
  without a `ParallelNode` same-type collision. `FanOutNode` takes a builder closure and a count,
  wraps each built instance with `NodeExt::with_identity` under `FanOutNode::branch_identity(base_name,
  i)` (`"{base_name}[{i}]"`), and runs the set through `ParallelNode` — distinct identities are what
  make `ParallelNode`'s last-write-wins merge safe here, since each branch's `ctx.nodes` key is
  unique. `AggregateNode::for_fan_out` derives the same `branch_identity` sequence to read each
  branch's `ctx.nodes` entry back out, joining them into one array ordered by declared identity
  order (not `HashMap` iteration order, which is unspecified). An `impl Node for Box<dyn Node>` was
  added in `fan_out.rs` (not `node.rs`, which the task's scope excluded) so `with_identity` — which
  requires `Self: Sized` — is callable on the builder closure's `Box<dyn Node>` output. See
  `crates/engine-core/tests/it/fan_out_aggregate.rs` (listed in the Module Map's `tests/` entry
  above) for a full `Workflow::run` proving the no-collision property end-to-end.
- `WorkflowValidator` / `ValidationError` (`engine-core::validate`) — static graph-shape checks
  run before execution: BFS reachability from `start_node`, DFS cycle detection that skips edges
  declared out of router nodes (routers are exempt so runtime back-edges are legal), and a
  fan-out arity guard rejecting non-router nodes with more than one declared connection. Router
  classification is via `NodeRegistry` lookup + `Node::as_router().is_some()`.
- `Workflow` (`engine-core::workflow`) — pointer-walk runner (not a topo-scheduler); pairs a
  `NodeRegistry` with a `WorkflowSchema`. `async fn run(event, on_progress) -> Result<TaskContext,
  WorkflowError>` (EN.2.0) seeds every declared node PENDING, emits the initial snapshot via
  `on_progress`, then walks `current_node` — resolving router nodes via `Router::route(ctx)` and
  plain nodes via `next_after` (`connections[0]`) — until `None`, ported from `workflow.py`;
  `node_context` (the RUNNING → SUCCESS/FAILED envelope) is likewise async, `.await`ing each
  node's `process`. `Workflow::
  new_validated(registry, schema)` is a fallible constructor that runs `WorkflowValidator::
  validate` first and rejects an invalid schema; the existing infallible `Workflow::new` is
  unchanged. `async fn run_with(event, on_progress, RunOptions) -> Result<TaskContext,
  WorkflowError>` (EN.2.B) is the cancellation/budget-aware entry point `run()` now delegates to:
  at each node boundary, before dispatch, it checks an optional `CancellationToken` and consults an
  optional `Budget` ledger, halting the walk (nodes not yet reached stay Pending) and stamping the
  reason into `TaskContext::metadata` — via `cancellation::stamp_cancelled` for a cancel, or the
  private `stamp_budget_halt` (keyed `BUDGET_METADATA_KEY = "budget"`) for a budget halt — while
  still returning `Ok(TaskContext)` with the accumulated state, mirroring how a node's own `Err`
  is handled. `RunOptions { cancellation_token: Option<CancellationToken>, budget: Option<Budget>
  }` (`#[derive(Default)]`) carries the two optional gates; `run()` itself is unchanged for
  existing callers (`engine-serve/http.rs` and pre-EN.2.B tests).
- `CancellationToken` (`engine-core::cancellation`) — a `tokio::sync::watch`-backed cooperative
  cancel signal (not `AtomicBool`+`Notify`): `cancel()` calls `tx.send_replace(true)` rather than
  `tx.send(true)`, since `watch::Sender::send` silently no-ops with zero live receivers (the case
  right after `new()`) — `send_replace` updates the retained value unconditionally, so a cancel
  issued before any `cancelled()` waiter subscribes is still observed. `async fn cancelled(&self)`
  awaits the first `true` value via `Receiver::changed`, race-free against the
  check-then-subscribe-then-await pattern. `stamp_cancelled(&mut Value)` merges a `"cancellation"`
  key into `TaskContext::metadata` (preserving other metadata keys) rather than overwriting it.
  Promoted `tokio` from a dev-dependency to a real `engine-core` dependency (EN.2.B) since
  `cancelled()` is now public async API, not test-only code — see
  `planning/decisions/D6-cancellation-and-budget-semantics.md`.
- `Budget` / `BudgetLedger` (`engine-core::budget`, EN.2.B) — `Budget` is a config struct (token
  and/or cost caps); `BudgetLedger` accumulates spend from each completed node's `NodeRun.usage`
  (tokens) plus an optional per-call `cost_usd`, folded in separately since `engine_contract::Usage`
  carries no cost field per the data contract. **EN.4.0:** `Workflow::run_with` now supplies that
  `cost_usd` itself — after each node completes, `node_cost_usd(&ctx, &identity)` (`workflow.rs`)
  reads the node's own `ctx.nodes[identity]["cost_usd"]` (the same field shape `ClaudeCodeStep`
  writes, and that `policy::telemetry::total_cost_usd` reads for SDLC's cost-bearing stages) and
  folds it into `ledger.record(...)` alongside token usage, so `Budget::max_cost_usd` actually
  gates a run the same way `max_total_tokens` already did. A node with no `cost_usd` in its output
  contributes `None` (token-only accounting), so behavior is unchanged when no cost cap is set.
  `check()` is the pre-dispatch gate `Workflow::run_with` calls before each node: returns
  `Allow` or `Halt(BudgetHaltReason)` when accumulated spend is *reached* (`>=`) the configured
  cap — a cap hit exactly by the last completed node stops the walk before the node that would
  exceed it. `BudgetHaltReason::to_json()` renders `{cap, spent, limit}` for the metadata stamp;
  `budget.rs` itself never mutates a `TaskContext` — `Workflow::run_with` owns the write.
  Absent `Budget` config, `check()` always allows.
- `OnProgress<'a>` (`engine-core::workflow`, `type OnProgress<'a> = Box<dyn FnMut(&TaskContext) +
  'a>`) — the injected persistence seam invoked at every node boundary (initial seed, RUNNING
  entry, SUCCESS/FAILED exit). This block only defines the signature; EN.1.C wires it to Postgres.
- `WorkflowError` (`engine-core::workflow`) — a `{ message: String }` struct for graph-shape
  failures (e.g. an unresolvable node identity); distinct from `NodeError` — a node's own failure
  is captured in its `NodeRun` and does not short-circuit `run()` with an `Err`.
- `Identified<N>` / `NodeExt::with_identity` (`engine-core::node`, EN.5.E task 1) — a delegating
  wrapper that overrides `Node::name()` with an owned instance string while forwarding
  `process`/`as_router` to the wrapped node unchanged, constructed via a blanket
  `NodeExt::with_identity` extension method (no existing `impl Node` block needs editing). This is
  instance-backed node identity: the same node *type* can be registered more than once, under
  distinct identities, in one graph or across graphs.
- `InputBinding` / `WithInput<N>` / `NodeExt::with_input_from` (`engine-core::node`, EN.5.E task 1)
  — a declarative binding from a node to the identity of the upstream node whose `ctx.nodes` entry
  it should read, replacing a hardcoded `NODE_NAME` const imported from another module.
  `InputBinding` is meant to be held as a struct field by individual node authors (mirroring the
  `with_transport`/`with_http_post`/`with_clock` builder convention) and resolved via
  `InputBinding::resolve` inside `process`; `WithInput<N>` is a generic wrapper-based alternative
  (via `NodeExt::with_input_from`) for callers who want the binding without authoring a bespoke
  per-struct builder. An unbound `InputBinding` (the `default()`) falls back to a caller-supplied
  default, so existing nodes are unaffected until they opt in.
- `LoopSpec` / `LoopCluster` / `build_loop` (`engine-core::loop_combinator`, EN.5.E task 2) — a
  reusable builder for the `{guard router, increment node, back-edge}` cluster idiom (generalized
  from the hand-written `sdlc_flow::graph`/`task_loop` retry loop; the task-loop drain branch now
  also carries a `FinalValidationNode` run-level validation gate, `EN.3.E` — see
  [sdlc-flow-workflow.md](sdlc-flow-workflow.md)). `build_loop(LoopSpec) ->
  LoopCluster` returns two boxed nodes (a guard `Router` and an increment node, both identity-
  derived via `with_identity` so distinct-prefix clusters coexist in one registry) plus their
  declared `NodeConfig` connections, ready to merge into a `NodeRegistry`/`WorkflowSchema`. The
  guard reads the increment node's stored iteration count off `ctx.nodes` and routes either back to
  the increment node (continue) or to `LoopSpec::exit_to` (cap reached, or `exit_predicate`
  satisfied); the increment node is the cluster's only state-mutating member, and is itself a
  `Router` (unconditionally routing to `body_entry`) purely so the back-edge is a runtime router
  edge — `WorkflowValidator`'s DFS cycle check skips both hops, per D42.
- `Locale` / `Currency` / `MoneyRange` / `RateSheet` / `RateCard` (`engine-core::locale`,
  `EN.4.F`) — a client's market segmentation, threaded through the diagnostic funnel's event
  schemas (`ResearchAgentEventSchema`, `DiagnosticIntakeEventSchema`,
  `ProposalGeneratorEventSchema`) as a `#[serde(default)]` field, `Locale::PtBr` by default.
  `Locale::currency()` is a total, infallible mapping to which of `business/docs/rates.md`'s two
  rate sheets applies; `Locale::language_name()`/`language_directive(locale)` produce the
  per-run prompt-body fragment a model node splices in (never into a `STABLE_SYSTEM_PROMPT` —
  `CLAUDE.md` rule 6's cache-breakpoint clause) to select the language its prose is written in,
  with an explicit carve-out never to translate a literal contact string. `RateCard::sheet(locale)`
  is the only accessor onto a `RateSheet`'s `MoneyRange`s (`diagnostic`/`project`/`retainer`) and
  `hourly_floor`; `RateCard::load_from(&PolicyConfigSource)` reads the `rate_card` section of
  `harness.json`, falling back to the ported `business/docs/rates.md` figures
  (`RateCard::default()`) when the section is absent, and hard-erroring — never silently
  defaulting — on a malformed or currency-mismatched section (`RateSheet::validate`). **The
  firewall invariant**: this module defines no conversion between `Currency::Brl` and
  `Currency::Usd` anywhere — no rate constant, no helper, no test — matching `rates.md`'s "never
  quoted in the same conversation, never cross-converted" rule; see
  [proposal-generator-workflow.md § Locale and the firewalled rate card](proposal-generator-workflow.md#locale-and-the-firewalled-rate-card)
  for how `ProposalWriterNode`/`ProposalReviseNode` consume it to populate
  `FirstEngagement.investment` deterministically instead of letting the model author a price.
  `Locale` is a per-client attribute, not a cost/latency/quality tradeoff, so it is deliberately
  **not** a knob on any workflow's `Policy` struct or named profile bundle.
- `Dispatcher` (`engine-core::dispatch`, re-exported from `engine-serve::dispatch` as of EN.5.E
  task 3) — dual-registry (`workflow_registry` + `schema_registry`)
  lookup keyed by `workflow_type`; `register` takes a boxed `WorkflowFactory` closure. As of
  `EN.5.D`, `WorkflowFactory` is `Box<dyn Fn(&serde_json::Value) -> Result<Workflow, String> + Send
  + Sync>` — it receives the triggering event's `data` payload so a registration can resolve its
  own policy (the four-layer `builtin < harness < profile < event` precedence, `crate::policy`
  framework) and assemble a policy-dependent registry (`registry_for_policy`) *before* the
  `Workflow` is built, rather than the old zero-argument factory
  (`Box<dyn Fn() -> Workflow + Send + Sync>`), which could only ever build the same default-policy
  graph regardless of what the event asked for. `dispatch_with_event(workflow_type, event)` is the
  primary entry point: it resolves the registration and hands `event` to its factory, returning
  `DispatchError::UnknownWorkflowType` for an unregistered type or
  `DispatchError::PolicyResolutionFailed(message)` — distinct, and surfaced as a different HTTP
  status by `post_events` — when the factory's own policy resolution fails against `event` (e.g. an
  unknown `profile` name, a malformed inline `policy` override). `dispatch(workflow_type)` is a thin
  convenience wrapper calling `dispatch_with_event` with an empty (`Null`) event, kept for callers
  with no event payload in hand. Every policy-resolving builtin registration
  (`engine-serve::workflows::register_{sdlc_flow,research_agent,diagnostic_intake,
  proposal_generator,approve_and_run}`) resolves policy against a workflow-appropriate
  `policy::PolicyConfigSource` — `SDLC_FLOW` (which runs embedded in a real repo checkout) uses
  `PolicyConfigSource::Worktree(current_dir)`; as of `EN.3.K`, that worktree root is resolved per
  run from the event's `repo` registry slug (falling back to the process's cwd only when `repo` is
  absent — see `docs/sdlc-flow-workflow.md`) rather than from the process's cwd unconditionally;
  the other four (channel/API-shaped, no repo
  checkout at dispatch time) use `PolicyConfigSource::Builtin` (builtin + profile + event layers
  only, no filesystem access) — so a worktree-free workflow never falls back to
  `std::env::current_dir()` to resolve its policy. The resolved policy is seeded into the run's
  initial `ctx.nodes` under `policy::RESOLVED_POLICY_IDENTITY` (via `Workflow::with_seeded_nodes`),
  so policy is resolved **once per run at dispatch** — no node re-resolves it (and re-reads
  `harness.json`) inside its own `process()`. See
  `planning/decisions/D11-policy-dispatch-seam.md`. `register_opportunity_set_stage` /
  `register_opportunity_add_action` (`OPPORTUNITY_SET_STAGE` / `OPPORTUNITY_ADD_ACTION`, `EN.7.B`
  task 6) are the first `register_builtin_workflows` entries with no policy layer at all —
  `OpportunityEditNode` calls no model, so their `WorkflowFactory`s resolve no
  `PolicyConfigSource` and seed no policy stamp; `register_harvest_approve` (`HARVEST_APPROVE`,
  `EN.7.C` task 7) follows the same no-policy pattern, since `HarvestApproveNode` is also
  model-free; `register_terminal_probe` (`TERMINAL_PROBE`, `EN.9.D` task 5) is the same no-policy
  shape again — `TerminalSessionNode`/`TerminalObserveNode` call no model and read no
  `harness.json` — and `register_builtin_workflows` now populates eleven workflow types in total.
- `LiveStateStore` (`engine-serve::live_state`) — in-memory `Arc<RwLock<HashMap<RunId, TaskContext>>>`
  (`RunId = uuid::Uuid`, matching `EventsRow.id`) with `record`/`get`/`list_active`/`remove`; the
  local Console's no-DB-poll read path for live run state. `mark_terminal` (EN.5.F) moves a
  finished run's snapshot out of the live map into a bounded 100-entry completed ring
  (`COMPLETED_RUN_RETENTION`) instead of dropping it, and `get_record` reads back that snapshot
  plus `workflow_type`/`created_at`/`updated_at`/a `terminal` flag; `get` checks the live map
  first, then falls back to the completed ring, so a terminal run keeps serving its last snapshot,
  while `list_active` only reads the live map so terminal runs are excluded.
- `DurableHandle` / `spawn_durable_writer` / `durable_on_progress` (`engine-serve::durable`) — an
  mpsc-bridged async durable-write seam mapping `on_progress` `TaskContext` snapshots to
  `engine_contract::EventsRow`: inserts the first (all-PENDING) snapshot per run via
  `engine_store::insert_event`, updates subsequent snapshots via `update_event`/`touch`, and
  self-skips Postgres I/O (does not fail) when no pool/`DATABASE_URL` is configured. The pure
  `message_to_row(message, created_at, updated_at) -> EventsRow` mapping is tested directly for a
  byte-identical contract shape without a live Postgres connection.
- HTTP surface (`engine-serve::http`, actix-web) — `configure(cfg)` registers routes shared by the
  serve binary and the test harness: `GET /health`, `GET /workflows` (list registered workflow
  types), `GET /workflows/{type}/graph` (schema graph for a type), `POST /events/` (X-API-Key
  gated; dispatches the event, records live state, and enqueues the durable write), and (EN.5.F)
  `GET /events/{event_id}` (X-API-Key gated readback of a run's canonical shape, served from
  `LiveStateStore` with no DB query — `404` for an unknown or malformed id).
  `post_events` mints the `run_id`/`event_id` (they are always equal — both the `events.id`
  primary key), then spawns the run via `actix_web::rt::spawn` and returns `202 {run_id,
  event_id}` immediately instead of awaiting `workflow.run_with` (EN.5.F; previously awaited
  in-request, EN.2.B) — the spawned task marks the run terminal in `LiveStateStore` on every exit
  path and no longer surfaces a run failure as HTTP `500`; a failed run is now only observable
  through the `GET /events/{event_id}` readback (`status: "failed"`) or the SSE terminal frame
  below. That same minted `run_id` is now stamped into `TaskContext.metadata` via
  `RunOptions::run_id` (EN.6.J) before the first node dispatches, so a `SDLC_FLOW` run's
  `sdlc-flow-state.json` can be joined back to the engine run that produced it; `suspend::spawn_run`
  also uses the post-walk context to write a terminal `"blocked"` status into that file (via
  `wrap_up::write_terminal_blocked_state`) when the walk ends in a node failure instead of leaving
  it `"running"` forever. `post_events` mints a `CancellationToken` per run, registers it in `RunRegistry` keyed by
  `run_id` for the duration of the run, and deregisters it unconditionally after `run_with`
  returns (`Ok` or `Err`) — so a finished run's `run_id` 404s on a later abort call rather than
  staying abort-able forever. Every HTTP-triggered run is seeded with a default `Budget` read from
  `ENGINE_RUN_MAX_COST_USD` (default `5.0`) / `ENGINE_RUN_MAX_TOKENS` (default unset).
- SSE stream (`engine-serve::stream`, EN.5.F) — `GET /events/{event_id}/stream`, a third fan-out
  registered inside `post_events`'s `on_progress` closure alongside `LiveStateStore::record` and
  the durable writer. A per-run `tokio::sync::broadcast` sender is registered in a process-global
  registry keyed by `run_id`; subscribers get every `TaskContext` snapshot as an SSE frame. A
  small terminal-frame cache means a subscriber that connects *after* a run has already finished
  still receives a one-shot terminal frame (reusing `http::derive_terminal_status`, `pub(crate)`)
  and closes cleanly, rather than hanging on an unpublished channel.
- Abort endpoint (`engine-serve::abort`, EN.2.B) — `POST /events/{run_id}/abort`, gated by the
  same `check_api_key` (widened from private to `pub(crate)`) as `/events/`, backed by
  `RunRegistry` (a registry of live per-run `CancellationToken`s). Looks up `run_id`: `401` on a
  missing/invalid API key, `404` if the run isn't currently registered (unknown or already
  finished), `202 Accepted` on success (matching `post_events`'s existing `202` convention) —
  calls `token.cancel()`, which `Workflow::run_with` observes at the next node boundary.
- `TaskContext` — `{event, nodes: {<ClassName>: output}, metadata, node_runs: {<ClassName>: NodeRun}}`
  — the preserved data-contract shape (see `docs/data-contract.md`, pinned to canonical v1.1.0).
- `NodeRun` — `status` (`pending|running|success|failed`), `started_at`/`completed_at`, `error`,
  `input`, `usage` (`{input_tokens, output_tokens, model}` for LLM nodes). Stamped RUNNING →
  SUCCESS/FAILED by the framework-owned `node_context` envelope in `workflow.rs`, not by the node
  itself.
- `ClaudeCodeStep` (`engine-core::nodes::claude_code_step`, EN.2.A) — a reusable `Node` that spawns
  a Claude Code session via `claude_code_rs::execute` and maps its `Outcome` into the node's
  `TaskContext::nodes` output (`{content, cost_usd, model, structured}` — `structured` is
  `outcome.structured_output`, the SDK's parsed JSON when the caller set `config.json_schema` and
  the model's reply matched it, else `null`) and `NodeRun.usage`. Constructed with a
  fixed prompt (`new`) or a prompt built fresh from the live `TaskContext` on each call
  (`with_prompt_builder`); its subprocess call goes through an injectable `Transport` closure
  (`with_transport`) so the gated test suite stubs it instead of spawning a real `claude` process.
  Per `planning/decisions/D4-claude-code-transport-choice.md`, this node owns none of the
  subprocess/argv/parse logic — that surface belongs entirely to `core/claude-code-rs`.
  Also carries an optional `CancellationToken` (`with_cancellation_token`, EN.2.B): `process()`
  races it against the awaited transport future via `tokio::select!`, and on a cancellation win
  drops the in-flight future and returns `Ok(ctx)` unchanged (rather than a `NodeError`) — a `Node`
  never touches its own `NodeRun` status, so this lets `node_context` mark the node `Success` and
  defers the actual cancelled-terminal-state stamping to `Workflow::run_with`'s per-boundary
  cancellation check before the next node dispatch.

  **Model attribution (as of the 2026-07-16 SDK fix, claude-code-rs D2).** The `claude` CLI has no
  top-level `model` field; it reports a *map* of models (`modelUsage`), since one call can bill
  several. `content` comes from the SDK's `text`, and `model` from
  `Outcome::primary_model()` — an SDK-side heuristic (cost, then output tokens, then key order) that
  returns `None` when no model ran. Because `engine_contract::Usage::model` is a required `String`
  (the orchestrator data contract's shape, §6), this node supplies the literal `"unknown"`
  when the SDK reports none. That fallback lives here, at the seam, rather than loosening a contract
  type that `bastion` also reads — see `docs/data-contract.md` §6 and D20.

  In practice `"unknown"` is a defensive backstop on the default transport: `modelUsage` is empty
  only on the CLI's error envelope, and `claude_code_rs::execute` now returns `Err(Error::Api)` for
  that case, so the node fails before stamping usage. It remains reachable via a custom `Transport`,
  and would become reachable by default if the CLI ever emitted a success envelope with no
  `modelUsage`.

## Data Flow

1. `bastion serve` receives a trigger via the actix-web HTTP surface (`POST /events/`, X-API-Key
   gated) — from local CLI, remote BastionUI over Tailscale, or an orchestrator-equivalent event
   POST. A live run can be cancelled mid-flight via `POST /events/{run_id}/abort` (EN.2.B, same
   API-key gate), which resolves the run's registered `CancellationToken` and calls `cancel()`.
2. `Dispatcher::dispatch_with_event(workflow_type, &body.data)` resolves the event to a registered
   `Workflow` via the dual registry (`workflow_registry` + `schema_registry`), feeding the
   triggering event's `data` to the registration's policy-aware `WorkflowFactory` (`EN.5.D`) so it
   can resolve its own policy and assemble the policy-dependent registry before the `Workflow` is
   built. Returns `DispatchError::UnknownWorkflowType` (surfaced as HTTP 422) for an unregistered
   type, or `DispatchError::PolicyResolutionFailed` (also HTTP 422, naming the offending profile)
   when the factory's policy resolution fails against `data` — e.g. an unknown `profile` name.
3. `Workflow::run` seeds all nodes declared in the `WorkflowSchema` PENDING in `TaskContext::node_runs`,
   emits the initial in-memory snapshot via `on_progress`, which fans out to both:
   - `LiveStateStore::record` — the in-memory run-state map the local Console reads with no DB poll.
   - `durable_on_progress` — the mpsc-bridged async writer that inserts the first (all-PENDING)
     snapshot as the durable `events` row via `engine_store::insert_event` before the first node runs.
4. The pointer-walk runs each node inside the framework-owned `node_context` envelope (RUNNING →
   SUCCESS/FAILED + `started_at`/`completed_at` timing), following `connections[0]` for plain
   nodes or `Router::route(ctx)` for router nodes (including undeclared runtime back-edges),
   invoking `on_progress` after every transition; a node returning `Err` halts the walk but
   `run()`/`run_with()` still return `Ok(TaskContext)` with the accumulated state. Each subsequent
   snapshot updates `LiveStateStore` and is persisted via `update_event`/`touch` on the durable
   writer. Before each node dispatch, `run_with` (EN.2.B) also checks the run's optional
   `CancellationToken` and optional `Budget` ledger, halting the walk the same way a node error
   does (still `Ok`, nodes not yet reached stay Pending) and stamping the reason
   (`metadata.cancellation` or `metadata.budget`) into `TaskContext::metadata`.
   Every completed (or halted) run's `run_with` also stamps a workflow-agnostic
   `policy::telemetry::RunTelemetry` snapshot into `metadata.run_telemetry` (`EN.5.D`) — wall-clock,
   token/cost totals, review verdicts, and `model_tier_used` harvested from whatever identities
   `ctx.nodes` carries by that point. `model_tier_used` prefers each stage's **observed** transport
   stamp (`ctx.nodes[stage]["transport"]["tier"]`, written by `ClaudeCodeStep`/
   `openai_compat_transport`'s tier-aware `MetaTransport` seam) over the resolved policy's intent,
   so a `local`-tier stage that silently fell back to cloud (endpoint unreachable) is reported as
   what actually ran, not what the policy asked for.
5. Local Console reads live state directly via `LiveStateStore::get`/`list_active` (in-memory,
   no DB poll); remote observers (BastionUI) subscribe to serve's `GET /workflows` /
   `GET /workflows/{type}/graph` read-API rather than polling Postgres. The durable writer
   self-skips Postgres I/O (without failing the request) when no pool/`DATABASE_URL` is configured.
