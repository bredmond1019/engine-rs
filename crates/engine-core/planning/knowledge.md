---
type: Reference
title: engine-core (crate-local) Knowledge
description: Distilled, durable knowledge for the stray crate-local planning root at crates/engine-core/planning — how it works, conventions, and an architecture digest for the blocks executed here.
doc_id: knowledge
layer: [engine]
project: engine-rs
status: active
keywords: [knowledge, conventions, architecture, semantic memory, durable, engine-core, crate-local-planning]
related: [memory, archive-index]
---

# Knowledge — crates/engine-core/planning (crate-local)

Distilled, **durable** knowledge for the three SDLC blocks that were executed with this crate
subdirectory as their planning root (`EN.5.D-policy-dispatch-seam`, `EN.5.F-async-run-lifecycle`,
`EN.6.A-egress-dispatch`), rather than the repo's canonical `planning/` (which is a symlink to
`core/_planning/engine-rs/` in the company brain). See `memory.md`'s "Gotchas" note on why this
directory exists at all. Most of the narrative outcome of these three blocks is *also* captured in
`core/_planning/engine-rs/status.md` and `knowledge.md` (the canonical engine-rs planning root); the
entries below capture detail specific to these blocks' worklogs that is not already promoted there.

## How it works

_Architecture digest — the main components and how they fit together._

- **`ChannelTransport` egress seam (`EN.6.A`).** `engine-core` exposes `OutboundAction`/`OutboundBody`/`ChannelSendReceipt` + a `ChannelTransport` trait, with `StubChannelTransport` (test recording) and `UnwiredChannelTransport` (named per-channel `delivered:false` stub) as the two non-live impls, mirroring the `HttpPost` seam shape. `WorkflowTriggerDispatch` is the live impl for the `WorkflowTrigger` channel: it POSTs `{workflow_type, data}` to `/events/` with an `X-API-Key` header, enforces an 8-hop `chain_depth` cap (read from the event, incremented, refused at/above `MAX_CHAIN_DEPTH=8` before any network call), and prefers an injected in-process `Dispatcher` over the HTTP loopback fallback. The in-process path is fire-and-forget via `tokio::task::spawn_blocking` running a fresh current-thread runtime — not a direct `.await` — because `Workflow::run`'s `OnProgress` is `!Send` and cannot cross an await point inside the `Send`-bound `#[async_trait]` `ChannelTransport::send`. `channel_transport_live()` routes `WorkflowTrigger` through this seam and every other channel to `UnwiredChannelTransport`.
  source: EN.6.A-egress-dispatch/sdlc/worklog.md (tasks 1-2) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **`ActionDispatchNode` (`EN.6.A`).** The terminal `CONTENT_PIPELINE` egress node: builds a `Digest` reply `OutboundAction` when `envelope.reply_context` is present, a `TriggerWorkflow` action when the raw event carries a `trigger` request, sends each over the injectable seam, and never fails the run on a transport error (records `delivered:false` instead). Every stored receipt is stamped with the run's `envelope_id` — `TaskContext`/`metadata` carries no `run_id` concept at the `engine-core` level (that's an `engine-serve`-only, `live_state.rs` concept), so `envelope_id` is the correlation key that travels with a dispatched child event.
  source: EN.6.A-egress-dispatch/sdlc/worklog.md (task 3) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **Bounded live/terminal run retention (`EN.5.F`).** `LiveStateStore` moves a finished run out of its live map into a bounded 100-entry completed ring via `mark_terminal`/`get_record` (carrying `workflow_type`/`created_at`/`updated_at`); `get()` checks the live map first then falls back to the ring (so terminal runs still serve a snapshot), while `list_active()` reads only the live map. `record()`/`get()`/`remove()` kept byte-identical signatures so `http.rs`/`abort.rs`/bastion callers outside the task's scope stayed unbroken.
  source: EN.5.F-async-run-lifecycle/sdlc/worklog.md (task 1) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **Non-blocking trigger + readback + SSE (`EN.5.F`).** `POST /events/` spawns the run via `actix_web::rt::spawn` and returns `202 {run_id, event_id}` (event_id == run_id) immediately, seeding a default `Budget` from `ENGINE_RUN_MAX_COST_USD`/`ENGINE_RUN_MAX_TOKENS` via a memoized `OnceLock`. `GET /events/{event_id}` derives status (`running`/`succeeded`/`cancelled`/`budget_halted`/`failed`, in that precedence order) from `LiveStateStore` plus a module-local `live_run_metadata` side table (populated pre-spawn, cleared post-`mark_terminal`) — DB-free, 401/404 on bad key/unknown/malformed id, never 500. `GET /events/{event_id}/stream` is a per-run `tokio::sync::broadcast` tee (`crates/engine-serve/src/stream.rs`) with a terminal-frame cache so a late subscriber still gets one terminal frame instead of hanging.
  source: EN.5.F-async-run-lifecycle/sdlc/worklog.md (tasks 2-4) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **`policy::overlay::Overlay` collapse (`EN.5.D`).** A shared `Overlay` trait + `merge_opt`/`PartialLocalConfig`/`merge_local` in `crates/engine-core/src/policy/overlay.rs` replaced four hand-written, byte-identical merge trios duplicated across `workflows/{sdlc_flow,research_agent,diagnostic_intake,proposal_generator}/policy.rs`. `policy::resolve::merge_opt` stays the sole re-exported `merge_opt` (callers needing the overlay version use `policy::overlay::merge_opt` directly, avoiding an ambiguous-glob collision). `apply_override`'s former free-function body was inlined directly into each type's `Policy::apply` (binding `self` to `base`), since the task required deleting the free function while `Policy::apply` still had to exist. Workflow-specific nested-type merges (`merge_model_tiers`, `merge_close_out`, `merge_close_out_reuse`) stayed private per-workflow functions — only `LocalConfig` has a shared `Overlay` impl.
  source: EN.5.D-policy-dispatch-seam/sdlc/worklog.md (tasks 1-2) · date: 2026-07-25 · supersedes: — · freshness: 2026-08-01
- **`PolicyConfigSource` decouples policy resolution from a worktree path (`EN.5.D`).** `policy/profiles.rs` exposes `PolicyConfigSource::{Worktree, HarnessFile, Builtin}`; `_from`-suffixed siblings (`read_harness_policy_defaults_from`, `read_harness_profiles_from`, `resolve_profile_from`) are built on it, with the existing worktree-taking functions kept as thin wrappers over them. `PolicyConfigSource::harness_path()` returns `None` only for `Builtin`, so the `_from` functions short-circuit before any filesystem access in that case. A new `resolved_policy_strict` read errors instead of silently defaulting when the `ResolvedPolicy` stamp is absent/unparsable; the lenient `resolved_policy` (Default fallback) was kept alongside it until a later migration task deleted it and its callers.
  source: EN.5.D-policy-dispatch-seam/sdlc/worklog.md (task 3) · date: 2026-07-25 · supersedes: — · freshness: 2026-08-01

## Conventions

_Naming, patterns, and standing choices specific to this project._

- **`OutboundAction`/`ChannelTransport` seam additions go additive, never signature-breaking.** `HttpPost` gained `post_with_headers` (default delegates to `post`, ignoring headers) rather than changing `post`'s signature, keeping every existing call site (`persist_to_brain.rs`, `proposal_generator/persist_to_brain.rs`) and their tests untouched while still making the new header assertable via `StubHttpPost::last_headers()`.
  source: EN.6.A-egress-dispatch/sdlc/worklog.md (task 2) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
- **A seam's "carry the event through untouched" contract is load-bearing.** `WorkflowTriggerDispatch` forwards the event `Value` as-is (any `chain_depth`/correlation ids the caller already stamped travel through unmodified) and only reads/increments `chain_depth` itself — it never fabricates a `parent_run_id` or similar. Nodes built on top of the seam (e.g. `ActionDispatchNode`) follow the same rule: stamp `envelope_id` into the outgoing event's `data`, don't invent new correlation fields.
  source: EN.6.A-egress-dispatch/sdlc/worklog.md (tasks 2-3) · date: 2026-07-27 · supersedes: — · freshness: 2026-08-01
