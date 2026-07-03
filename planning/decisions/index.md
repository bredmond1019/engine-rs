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

<!-- Add a row per decision as they are made. Record new ones with /log-decision-style atomic
     files (D2, D3, …). -->
