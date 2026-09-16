---
type: Index
title: engine-rs Node Library
description: Entry point for the Node/Molecule Library — every reusable Node grouped by purpose, every proven or near-duplicate Node sequence (a molecule), and the gaps found while cataloguing them.
doc_id: nodes-index
layer: [engine]
project: engine-rs
status: active
keywords: [nodes, molecules, atoms, library, reuse, extensibility, catalogue]
related: [docs-index, architecture, workflows-readme, nodes-atoms-index, nodes-molecules-index, nodes-gaps]
---

# The Node / Molecule Library

- A **Node** is one typed unit of work — implements `Node` (`crate::node`,
  [`architecture.md`](../architecture.md) § Core Types) — this library calls a reusable one an
  **atom** → [`atoms/index.md`](atoms/index.md)
- A **molecule** is a Node sequence used the same way by more than one workflow (or one duplicated
  by hand where it should have been shared) → [`molecules/index.md`](molecules/index.md)
- A **gap** is a Node missing a trait/seam it should have (no transport slot, no `Cancellable`, a
  hardcoded value a future run might want to vary) → [`gaps.md`](gaps.md)
- This library exists so the next workflow reuses or extends something here instead of hand-rolling
  a near-duplicate — check it **before** writing a new Node

## Why this exists

A September 2026 audit (three parallel passes: `crates/engine-core/src/nodes/`, nodes embedded
inside individual `workflows/<name>/` directories, and every workflow graph's Node sequence) found
the same shapes built independently 2-3 times across different workflows — `PersistToBrainNode`
twice, `ReviseNode` three times, a queue-drain router hand-copied once — none of it caught earlier
because there was no single place that listed every Node by purpose. This library is that place.
Keep it current: when you add a Node, add its row; when you find a duplicate, file it in
[`molecules/index.md`](molecules/index.md) rather than leaving it for the next audit to rediscover.

## Quickstart — before writing a new Node

1. **Check [`atoms/index.md`](atoms/index.md)** for a Node that already does what you need, grouped
   by purpose (Transport/LLM, Control flow, Brain/Content, Channel IO, Terminal, or
   workflow-embedded near-atoms).
2. **Check [`molecules/index.md`](molecules/index.md)** for a Node *sequence* — if two or more of
   your steps already appear together elsewhere, reuse that shape instead of composing your own.
3. **Check [`gaps.md`](gaps.md)** if you're about to add an LLM call or a config value — someone
   may have already found the exact seam you need, or the exact trap to avoid.
4. If your Node composes an LLM call, load the **`create-llm-node`** project skill before writing
   one by hand — `TransportSlotted`/`Cancellable` (CLAUDE.md standing rule 11) exist precisely so
   you don't hand-roll a transport field.
5. If nothing fits, build it **config-heavy and single-purpose** (CLAUDE.md standing rule 6/12: no
   hardcoded strings/names a future caller might want to vary), then **add it to this library** —
   a Node not indexed here is a Node the next audit rediscovers as "hidden."

## Layout

| Doc | What it covers |
|---|---|
| [`atoms/index.md`](atoms/index.md) | Every reusable Node, grouped by purpose, one row each |
| [`molecules/index.md`](molecules/index.md) | Every proven or near-duplicate Node sequence, with the concrete refactor for each near-molecule |
| [`gaps.md`](gaps.md) | Nodes missing a trait/seam they should have, and hardcoded values a future run might want to vary — a registry so these are never rediscovered from scratch |

## See also

- [`../architecture.md`](../architecture.md) — crate layout, `AppState`, injectable seams, data flow
- [`../workflows/README.md`](../workflows/README.md) — the workflow-level capability catalogue (a
  workflow is a graph of the Nodes/molecules in this library)
- [`../coming-soon.md`](../coming-soon.md) — planned capabilities that do not exist yet (nothing in
  this library belongs there — everything here is shipped)
