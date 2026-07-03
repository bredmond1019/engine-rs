---
type: Decision
title: "D3: HTTP Framework for engine-serve"
description: Standardizes engine-serve's HTTP surface on actix-web, consistent with the sibling rag-engine-rs service and the D2 tokio runtime.
doc_id: D3-http-framework-choice
layer: [engine, console]
project: engine-rs
status: active
keywords: [actix-web, http, engine-serve, framework, bastion serve, api]
related: [decisions-index, master-plan, D2-async-runtime-choice]
---

# D3 — HTTP Framework for engine-serve

**Decided:** 2026-07-03
**Status:** Accepted

## Decision

`engine-serve` standardizes on **actix-web 4** for its four-endpoint HTTP surface
(`POST /events/`, `GET /health`, `GET /workflows`, `GET /workflows/{type}/graph`), the
transport the local Console (and, eventually, remote observers over `bastion serve`) talks
to.

## Rationale

- **Runs on tokio.** `actix-web` is built on `tokio`, so it shares the single async runtime
  D2 already pins for the whole workspace — no nested-runtime or runtime-bridging problem
  when `engine-serve` is embedded inside the `bastion serve` host process.
- **Consistency with `rag-engine-rs`.** The sibling Rust service (`rag-engine-rs`) already
  uses `actix-web` for its own Actix streaming chat surface. Standardizing on the same
  framework across the two Rust services trades a marginal feature-for-feature comparison
  against alternatives (`axum`, `warp`) for operator familiarity and one less framework to
  carry across the portfolio.
- **In-process test harness.** `actix_web::test` (`test::init_service` + `test::TestRequest`
  + `test::call_service`) lets endpoint tests run in-process against the real route table
  (`configure`), with no bound socket or separate test client — cheap, deterministic
  endpoint tests for the dispatch/live-state/durable-write wiring this block adds.
- **Future streaming path stays open.** The reserved event-stream read API for BastionUI
  (out of scope for this block) is not blocked by this choice — `actix-web`'s SSE/streaming
  support (`HttpResponse::streaming`, `actix-web-lab`) covers that path if/when it lands.

## What This Commits `engine-serve` To

- `crates/engine-serve/src/http.rs` wires its route table through
  `actix_web::web::ServiceConfig` (`configure`), shared `AppState` via `web::Data`, and the
  `actix_web::test` harness for endpoint tests.
- Any future HTTP-facing addition to `engine-serve` (new routes, middleware, streaming
  responses) builds on `actix-web` rather than introducing a second framework.

## Rejected Alternatives

- **`axum`** — Comparable ergonomics and also tokio-based, but adopting it here would mean
  the portfolio carries two different Rust web frameworks (`axum` in `engine-serve`,
  `actix-web` in `rag-engine-rs`) for no functional gain; consistency wins for a
  solo-maintained stack.
- **`warp`** — Filter-based routing is less discoverable for a small, fixed four-endpoint
  surface than `actix-web`'s handler-per-route style; no compelling advantage over the
  already-adopted sibling framework.
