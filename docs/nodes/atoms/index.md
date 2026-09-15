---
type: Index
title: engine-rs Atom Nodes Index
description: Every reusable Node (an atom), grouped by purpose — the file listing for docs/nodes/atoms/.
doc_id: nodes-atoms-index
layer: [engine]
project: engine-rs
status: active
keywords: [nodes, atoms, index, navigation, reuse, engine-rs]
related: [nodes-index, nodes-molecules-index, nodes-gaps, nodes-atoms-transport-llm, nodes-atoms-control-flow, nodes-atoms-brain-content, nodes-atoms-channel-io, nodes-atoms-terminal, nodes-atoms-workflow-embedded]
---

# Atom Nodes — Index

- An **atom** is a Node reusable as-is (or with an already-provided builder) across more than one
  workflow, or generic enough to be — verdicts below are `atom` (proven or ready), `near-atom`
  (needs a small named refactor), or `specific` (correctly single-purpose, not a gap)
- Grouped by what the Node does, not by which workflow wrote it → tables below
- **Half of the proven atoms below have zero production callers yet** — designed right,
  unadopted, not "specific." Check here before assuming a shape doesn't exist.

## Groups

| Group | Covers | Doc |
|---|---|---|
| Transport / LLM | Everything that spawns or calls a model, plus the shared transport-selection machinery | [transport-llm.md](transport-llm.md) |
| Control flow | Nodes that shape the graph itself — fan-out, aggregate, suspend, generic routers | [control-flow.md](control-flow.md) |
| Brain / Content | Writing to the Brain corpus, the harvest/approval gate in front of it, recall | [brain-content.md](brain-content.md) |
| Channel IO | Generic HTTP and outbound-channel delivery primitives | [channel-io.md](channel-io.md) |
| Terminal | The tmux session stack — open, observe, send, await, hold | [terminal.md](terminal.md) |
| Workflow-embedded near-atoms | Atom-quality Nodes currently living inside a single `workflows/<name>/` directory | [workflow-embedded.md](workflow-embedded.md) |

## Quickstart

1. Pick the group closest to what you need.
2. Check the node's own file for a module-level `//!` doc comment — every Node in this repo
   documents its block ID, its injectable seam (if any), and why it lives where it does.
3. If it composes an LLM call, load the **`create-llm-node`** project skill before writing a new
   one.
4. If a Node is flagged `near-atom`, the refactor it needs is named in its row — do that instead of
   copying it.

## See also

- [`../gaps.md`](../gaps.md) — Nodes missing a trait/seam they should have, or hardcoding a value that should be config
- [`../molecules/index.md`](../molecules/index.md) — Node *sequences*, not single Nodes
- [`../../architecture.md`](../../architecture.md) § Core Types — the `Node` trait itself
