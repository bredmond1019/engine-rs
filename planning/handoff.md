---
type: Handoff
created: 2026-07-03
---

# Handoff — EN.2.A still paused; async-node question captured, needs a decision

> **For the next agent:** Read this immediately after `/prime`. Delete this file once consumed.

## What we're doing and why

`engine-rs` is porting the Python `orchestrator` engine core to Rust (the parallel-pilot
rewrite, D42). **Phase 1 is fully done** — EN.1.A/B/C all closed and merged; `bastion serve`
can trigger workflows over HTTP, hold live run state in memory, and durably record runs to
Postgres. The next block is **EN.2.A — Claude Code step node**, and it is **still paused** —
carried forward from the prior session, unchanged: it hinges on which transport
`ClaudeCodeStep` uses to invoke Claude Code, and the user wants to review an external Claude
Code Rust SDK on GitHub against the existing `claude-sdk-rs` before deciding (see `carryover[]`
in `planning/state.json` — nothing new resolved that gate this session).

This session added a **second, related decision** that also needs resolving before `EN.2.A`'s
spec gets written: whether `engine-core`'s `Node` trait should become `async fn`. The user
asked a direct question — "where do we stand on getting things async, the real leverage of
Rust" — and a two-agent research pass established that **neither** the Rust nor the Python
engine core does real `async`/`await` concurrency at the node level today (Python's concurrency
is thread pools + Celery worker processes, not `asyncio`). That means a fully-sync
`Node::process` is a faithful port, not a regression — but it also means the opportunity to
make Rust genuinely exceed Python (via true async I/O concurrency for things like spawning a
Claude Code subprocess) is still on the table and unexploited. This is directly relevant to
`EN.2.A` because spawning Claude Code is inherently an async operation; if the trait stays sync,
`ClaudeCodeStep` inherits the same thread-blocking ceiling Python has. The full comparison (real
file paths, struct/function signatures, line numbers for both codebases) is captured in
**`planning/async-node/notes.md`** — read that file before making any transport or trait-signature
decision for `EN.2.A`.

## Completed this session

- Answered a sequence of the user's questions comparing `engine-rs` to the Python `orchestrator`:
  what "Execution Core" (Phase 1) encompasses, how the two relate (D42 parallel-pilot, per-workflow
  graduation), what's built vs. not (workflows/node types/Celery/RAG), whether an end-to-end
  integration test exists (`crates/engine-serve/tests/dispatch_integration.rs` — confirmed: yes,
  dispatch → HTTP → live-state → durable-write mapping all exercised together, no live Postgres),
  and the async/concurrency question above. No code was changed for these — pure research +
  synthesis, backed by direct file reads and two background research agents (one surveying
  orchestrator's workflows/nodes/API/Celery/RAG wiring, one confirming the exact sync/async posture
  of both `Workflow.run`/`Node.process`/`AgentNode`/`ParallelNode`/the Celery task/the FastAPI route).
- Created **`planning/async-node/notes.md`** (via `/capture async-node`, then populated with real
  verified content rather than the skill's default empty scaffold, at the user's explicit request):
  a full file/struct/function map of both engine cores' current (a)synchronicity, a side-by-side
  concurrency-model table, and the open questions that block a `Node`-trait decision.
- Added a one-line pointer to the brain's `planning/backlog.md` (`## Active`, dated 2026-07-03,
  `repo:engine-rs type:research status:idea`) linking to the notes file — **this edit lives in the
  brain repo (`agentic-portfolio/`), not this repo, and is not part of this repo's `/commit`.**
- Ran `mev emit-state --write` after the backlog edit; it surfaced a **pre-existing, unrelated**
  error: `core/orchestrator/planning/state.json` is malformed JSON. Not caused by this session,
  not touched — flagged to the user, no fix attempted (out of scope for this repo).
- Added a third `carryover[]` entry to this repo's `planning/state.json` (see Durable State Updates)
  and fixed a schema violation in the process — the first draft used `related: ["async-node"]`
  (a bare string), but `carryover[].related` must be `depends_on`-style edge objects
  (`{type:"block",...}`/`{type:"external",...}`) per `core/planning/state-schema.md`; since the
  entry doesn't point at a block or external dependency, `related` was **omitted** instead
  (correct per the schema: "Omit when it points at nothing concrete"). Re-ran `mev emit-state
  --write` to confirm this repo's `state.json` is now schema-clean.
- Confirmed the prior session's `planning/handoff.md` (pointing at the Claude-Code-Rust-SDK
  review) had already been consumed/deleted before this session started — its content is fully
  reflected in `log.md`'s "Paused EN.2.A spec generation..." entry and in the two existing
  `carryover[]` entries, so nothing was lost.

## Remaining work

1. **Still the top blocker, unchanged from before:** review the Claude Code Rust SDK on GitHub
   the user wants to compare (URL not yet provided — ask for it first). Compare against
   `claude-sdk-rs` (`agentic-portfolio/claude-sdk-rs/`, brain cache
   `docs/projects/claude-sdk-rs.md`) — feature surface, session/launcher API, cost/token
   reporting, cancellation (kill-on-drop), maintenance/health.
2. **New this session, also gating `EN.2.A`:** decide whether `Node::process` becomes `async fn`
   before writing `EN.2.A`'s spec. Read `planning/async-node/notes.md` in full — it has the exact
   touched-surface list if the answer is yes (`node.rs`, `workflow.rs`, `routing.rs`,
   `parallel.rs`, every existing `Node` impl, `http.rs`'s `web::block` wrapper). This decision and
   the transport decision (#1) are related but separate — transport picks *how* `ClaudeCodeStep`
   talks to Claude Code; this picks whether the `Node` trait itself can `.await` that call.
3. **Then** resume `/generate-tasks EN.2.A`, honoring both transport-related `carryover[]`
   constraints already recorded (`transport-decision-uses-d4-not-d3`,
   `claude-sdk-rs-not-on-disk`) plus whatever the async-node decision (#2) implies for the block's
   file list.
4. **Housekeeping (carried forward, still open):** PR #1 (EN.1.C) is open on GitHub but `main`
   already has the local fast-forward merge — decide whether to close it as already-merged and
   push `main`, or reconcile GitHub's view.
5. **Optional, different repo:** the brain's `core/orchestrator/planning/state.json` is malformed
   JSON (`E_STATE_MALFORMED_JSON`, surfaced by `mev emit-state --write` this session). Not
   `engine-rs`'s concern to fix, but worth a heads-up if working in `orchestrator` next.

## Durable State Updates

- `planning/state.json` `carryover[]` — **one new entry**, two unchanged:
  - **New:** `resolve-async-node-question-before-en2a` (`kind: deferred`) — decide `Node::process`
    sync-vs-async before `EN.2.A`'s spec; points at `planning/async-node/notes.md` for full context.
  - Unchanged from prior session: `transport-decision-uses-d4-not-d3` (`kind: constraint`),
    `claude-sdk-rs-not-on-disk` (`kind: known_issue`) — neither cleared this session (no transport
    decision was made).
- `planning/async-node/notes.md` — new file (via `/capture`), doc_id `async-node`, populated with
  real content (not a placeholder scaffold) per the user's explicit request. Cross-linked from the
  brain backlog.
- `mev emit-state --write` run twice this session (once triggering the schema-fix above, once to
  confirm clean). `focus.next` still correctly points at `EN.2.A` (unchanged — it was already
  correct from the prior session).
- No block `tasks.json` created or changed.

## Open questions / choices

- **The GitHub Claude Code Rust SDK URL** — still not provided. Ask first thing (carried forward).
- **Async `Node` trait or not** — the new question this session surfaced. Not decided; framed but
  not resolved in `planning/async-node/notes.md`'s Open Questions section.
- **Transport primacy** — still unresolved by design, output of the SDK review (carried forward).
- Whether the async-node decision, if "yes," becomes its own decision file (`D5`, since `D3`/`D4`
  are already spoken for) or folds directly into `EN.2.A`'s scope — noted as an open question in
  the notes file itself, not yet answered.

## Context the next agent needs

Everything durable is in `carryover[]` and `planning/async-node/notes.md`; this file just points
at them. Do not re-derive the Rust/Python concurrency comparison from scratch — it's already fully
documented with file paths and line numbers in the notes file. The brain-level `backlog.md` edit
and the `orchestrator` state.json error are both in a different repo (`agentic-portfolio/`, not
`engine-rs`) and are not part of this repo's commit.

## First command after `/prime`

Read `planning/async-node/notes.md` in full, then ask the user for the GitHub URL of the Claude
Code Rust SDK they want reviewed — both decisions (async trait, transport) block `EN.2.A` and
should likely be resolved together before `/generate-tasks EN.2.A` resumes.
