---
type: Decision
title: "D2: Async Runtime + Persistence Stack"
description: Standardizes engine-rs on tokio for async execution and sqlx (postgres, runtime-tokio, tls-rustls) for the Postgres durable-record layer.
doc_id: D2-async-runtime-choice
layer: [engine]
project: engine-rs
status: active
keywords: [tokio, sqlx, postgres, async runtime, persistence, workspace dependencies]
related: [decisions-index, context, architecture]
---

# D2 — Async Runtime + Persistence Stack

**Decided:** 2026-07-02
**Status:** Accepted

## Decision

`engine-rs` standardizes on:

- **Async runtime:** [`tokio`](https://crates.io/crates/tokio) (multi-threaded, full feature set)
  as the workspace-wide async runtime, declared once under `[workspace.dependencies]` in the
  root `Cargo.toml` so every member crate (`engine-core`, `engine-contract`, `engine-store`,
  `engine-serve`) inherits the same version.
- **Persistence:** [`sqlx`](https://crates.io/crates/sqlx) with the `postgres`, `runtime-tokio`,
  and `tls-rustls` features for the Postgres durable-record layer that `engine-store` builds on
  in EN.0.B.

## Rationale

- **tokio** is the de facto standard async runtime for Rust server/embedding workloads and is
  already the runtime `bastion serve` (the embedding host, per D42) will run on — a single
  runtime across the embedding process avoids a nested-runtime or runtime-bridging problem.
- **sqlx over deadpool + tokio-postgres:**
  - `sqlx` gives compile-time query checking (`query!`/`query_as!` macros) against a real schema,
    which the data-contract's `events` row (per `orchestrator/docs/data-contract.md`) benefits
    from — the JSON-column shape (`task_context`, `node_runs`) is easy to typo without it.
  - `sqlx`'s native `sqlx::types::Json<T>` support is direct ergonomics for the serde-typed
    payloads `engine-contract` defines, versus hand-rolling `tokio-postgres` row-to-struct
    mapping plus a separate pool manager (`deadpool`).
  - `sqlx` bundles its own async connection pooling, so no separate pool crate is needed —
    fewer moving parts for a solo-maintained workspace.
  - Trade-off accepted: `sqlx`'s compile-time checked macros require a reachable database (or a
    committed `.sqlx` query cache) to build, which is marginally more CI setup than
    `tokio-postgres` — deferred to EN.0.B when `engine-store` actually issues queries; this
    block only pins the dependency choice, not a live Postgres CI job.
- `tls-rustls` (over `native-tls`) keeps the dependency tree pure-Rust, avoiding an OpenSSL
  system-library requirement on the deploy target (Mac Mini, per `docs/infrastructure.md`).

## What This Commits `engine-store` To

- `engine-store` (EN.0.B) will use `sqlx::PgPool` for its connection handle and `sqlx::query!`/
  `query_as!` (or the untyped `query`/`query_as` fallback where compile-time checking isn't
  practical) for reads/writes of the `events` row.
- Any future migration tooling should use `sqlx-cli` / `sqlx::migrate!` rather than an
  independent migration framework, to stay inside the one persistence stack.

## Rejected Alternatives

- **`tokio-postgres` + `deadpool-postgres`:** rejected — more manual row-mapping and a separate
  pool crate to wire up, with no compile-time query checking; better raw-protocol control than
  `sqlx` but not needed at this scale (solo-maintained, single Postgres target).
- **`async-std` as the runtime:** rejected — smaller ecosystem, and `bastion serve`'s host
  process is expected to standardize on `tokio` regardless (per D42's embedding model), so a
  second runtime would only add a bridging cost.

## Provenance

Recorded as part of `EN.0.A — Cargo workspace + CI`.
