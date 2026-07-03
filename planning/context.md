---
type: LocalContext
title: engine-rs Project Context
description: Core context, governing principles, and documentation router for engine-rs.
doc_id: context
layer: [factory]
status: active
keywords: [project context, governing principles, orientation, planning router]
related: [status, master-plan, planning-index]
---

# CONTEXT — engine-rs

> **Read this first.** Stable orientation for engine-rs: *why* this body of work
> exists, the rules that govern how it is built, and a router to the rest of `planning/`.
> This file orients; it does not track. For state, open `status.md`. For why choices were
> made, open `decisions/`.

## What This Project Is

Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the orchestrator data contract to Postgres as a durable record.

<!-- Expand: 1–2 paragraphs on what it does and the destination/outcome it builds toward. -->

## Who Is Building It

Brandon Redmond, solo, as part of Bastion (the five-layer practice OS). `engine-rs` is a parallel
pilot per [D42](../../../docs/decisions/D42-rust-engine-parallel-pilot.md): the Python `orchestrator`
(`core/orchestrator/`) is the existing, working engine core (~1,100 LOC) this project ports to
idiomatic Rust, harvesting audited-reusable periphery from `workflow-engine-rs` (graph validator,
`RetryPolicy`, token/cost types, MCP client) and `claude-sdk-rs` (the Claude Code launcher, after its
repair pass) rather than depending on either wholesale.

## The Document Set

| File | Role | Volatility | Read it when… |
|---|---|---|---|
| **context.md** | Orientation + router (read first) | Stable | You need to understand the project or find the right file |
| **status.md** | Current progress | Volatile | You need to know what's done / what's next |
| **master-plan.md** | Strategy + phase specifications | Semi-stable | You need to understand the sequence of work |
| **harness.json** | Validation/UI-test config the SDLC engines read | Semi-stable | You're adapting the pipeline to this stack |
| **decisions/** | Architectural decisions (atomic, append-only) | Append-only | You want to check a prior architectural choice |
| **index.md** | Navigation index for `planning/` | Stable | You need a map of the planning folder |
| **log.md** (root) | Dated narrative of work completed | Append-only | You want the chronological dev history |

## The Project Sequence at a Glance

<!-- Phase names only, one line each. The sequence is load-bearing; details live in
     master-plan.md. -->

- **Phase 0 — Foundation**
- **Phase 1 — Core**
- **Phase 2 — Depth / Hardening**
- **Phase 3+ — Differentiating Build**

## Governing Principles

<!-- 6–8 numbered rules that govern how this project is built. At minimum keep the first
     three; add project-specific architectural rules. -->

1. **Tests ship with every block.** No block is "done" until its core functionality is
   covered by automated tests.
2. **Just-in-time scope.** Build what the current block needs, not a speculative future.
3. **Sequence, not calendar.** Work is ordered by dependency and competence, not by dates.
4. **The data contract is byte-for-byte, non-negotiable.** Any drift from
   `orchestrator/docs/data-contract.md` v1.0.1 breaks `bastion`; every phase's acceptance criteria
   include a round-trip/parity check against it.
5. **Reuse-not-depend.** Harvest audited-reusable parts from `workflow-engine-rs` /
   `claude-sdk-rs`; do not adopt either repo wholesale (see D41 audit).
6. **Graduate per-workflow, not big-bang.** `orchestrator` stays the production path until a given
   workflow reaches parity in `engine-rs`; SDLC-flow is first.
7. **Brain/RAG stays Python.** Never port `document_qa`/`document_ingest`/pgvector — they touch the
   framework only through `Node.process`/`TaskContext` + Postgres and are cleanly separable.

## Fast Facts

- **Destination:** `engine-rs` embedded in `bastion serve` as Bastion's primary execution substrate,
  reaching data-contract parity with the Python `orchestrator` one workflow at a time (SDLC-flow first).
- **Type:** infrastructure
- **Tech stack:** Rust, Cargo workspace, tokio (async runtime; persistence layer decided in EN.0.A),
  Postgres (durable `events` record), embeds in `bastion serve`.
- **Key constraints:** Solo build; must preserve the orchestrator data-contract byte-for-byte; runs
  in parallel with the Python orchestrator until parity, not a replace-in-place migration.
- **Started:** 2026-07-02

---

*This file orients; it does not track. For state, open status.md. For why choices were made,
open decisions/.*
