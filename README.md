---
type: Index
title: engine-rs
description: Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the orchestrator data contract to Postgres as a durable record.
doc_id: readme
layer: [factory]
status: active
keywords: [project readme, prerequisites, setup, getting started]
related: [context, master-plan, planning-index]
---

# engine-rs

> Part of the **Bastion** ecosystem — see the [bastion-os](https://github.com/bredmond1019/bastion-os) front door for the full architecture.

Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the orchestrator data contract to Postgres as a durable record.

## Prerequisites

- Rust 1.78+ (via rustup)

## Setup

```bash
# 1. Clone the repository
git clone https://github.com/bredmond1019/engine-rs
# 2. Build the project
cargo build
```

## Running locally

```bash
cargo run --release
```

## Tests

```bash
cargo test
```

## Directory map

```
engine-rs/
├── .claude/        ← Claude Code commands + SDLC workflow engines
├── planning/       ← context, status, master-plan, harness.json, decisions/, <concept>/
└── <source dirs>
```

## Documentation

| Doc | Contents |
|---|---|
| `planning/context.md` | Orientation + governing principles |
| `planning/master-plan.md` | Strategy + phase specifications |
| `planning/status.md` | Current progress |
| `planning/harness.json` | SDLC validation/UI-test config (see `harness.examples.md`) |

## Roadmap / Known limitations

- **Cancellation:** The `Node` trait currently lacks a cancellation hook (no `CancellationToken` integration yet).
- **Merge Semantics:** Parallel merge is literal last-write-wins on whole `Value`s; evolving to deep JSON merge for concurrent branches is planned.

---

*Initialized 2026-07-02 from `base-template` (commit `7f2cbada68bdb0433133cf213777994030f7b7d6`).*

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE) · <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](./LICENSE-MIT) · <http://opensource.org/licenses/MIT>)

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.

Built for one operator and released because it may be useful to others — there is no support
obligation, no issue-response SLA, and no stability promise. See HQ decisions D40 and D75.
