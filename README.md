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

Bastion's native Rust execution engine — a graph-validated workflow runtime that embeds in `bastion serve`, holds live run state in-memory, and writes the orchestrator data contract to Postgres as a durable record.

## Prerequisites

<!-- What must be installed (runtime, package manager, services). -->

## Setup

```bash
# Numbered steps from zero to running.
```

## Running locally

```bash
# The exact commands from CLAUDE.md.
```

## Tests

```bash
# One-liner to run the test suite.
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
| [planning/context.md](planning/context.md) | Orientation + governing principles |
| [planning/master-plan.md](planning/master-plan.md) | Strategy + phase specifications |
| [planning/status.md](planning/status.md) | Current progress |
| [planning/harness.json](planning/harness.json) | SDLC validation/UI-test config (see `harness.examples.md`) |

---

*Initialized 2026-07-02 from `base-template` (commit `7f2cbada68bdb0433133cf213777994030f7b7d6`).*
