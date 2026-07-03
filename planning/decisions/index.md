---
type: Index
title: engine-rs Decisions Registry
description: Index of atomic, append-only architectural decision records for engine-rs.
doc_id: decisions-index
layer: [factory]
status: active
keywords: [decisions, ADR, architecture, append-only, decision registry]
related: [planning-index, context]
---

# Decisions Registry

Architectural decision records (ADRs) for engine-rs. Each decision is **one atomic
file**, append-only — never edit a settled decision; supersede it with a new one and link back.

## Decisions

- [D1: Initial OKF Scaffold](./D1-initial-okf.md) — Project initialized on the standard OKF
  documentation structure.
- [D2: Async Runtime + Persistence Stack](./D2-async-runtime-choice.md) — Standardizes on
  `tokio` as the async runtime and `sqlx` (postgres, runtime-tokio, tls-rustls) as the
  persistence layer for `engine-store`.
- [D3: HTTP Framework for engine-serve](./D3-http-framework-choice.md) — Standardizes
  `engine-serve`'s HTTP surface on `actix-web`, consistent with `rag-engine-rs` and the D2
  tokio runtime.
- [D5: Async Node Trait](./D5-async-node-trait.md) — Makes `Node::process` an `async fn` via
  the `async-trait` crate (async run loop + `join_all` fan-out; `Router::route` and `OnProgress`
  stay sync), unblocking `ClaudeCodeStep` from blocking a thread per session. **D4** (Claude Code
  transport) is reserved for EN.2.A.

<!-- Add a row per decision as they are made. Record new ones with /log-decision-style atomic
     files (D2, D3, …). D4 is reserved for the EN.2.A transport decision (see carryover). -->
