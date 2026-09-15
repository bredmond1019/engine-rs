---
type: Reference
title: "Molecule: Research-Dispatch Pair"
description: ActionDispatchNode (content_pipeline) and ResearchIngressDispatchNode (research_agent) independently implement the same send-via-ChannelTransport-and-record-receipts shape — research_agent's own doc admits it copies content_pipeline's node.
doc_id: nodes-molecules-research-dispatch-pair
layer: [engine]
project: engine-rs
status: active
keywords: [actiondispatch, ingressdispatch, channeltransport, near-molecule, research agent, content pipeline]
related: [nodes-molecules-index, nodes-atoms-channel-io, content-pipeline-workflow, research-agent-workflow]
---

# Molecule: Research-Dispatch Pair

- **Near-molecule — documented copy, not consolidated**
- `research_agent::ResearchIngressDispatchNode`'s own doc comment states it is "modeled directly on
  `content_pipeline::action_dispatch::ActionDispatchNode`"

## The two instances

| Node | File | Notes |
|---|---|---|
| `ActionDispatchNode` | [`content_pipeline/action_dispatch.rs`](../../../crates/engine-core/src/workflows/content_pipeline/action_dispatch.rs) | Send-via-injectable-`ChannelTransport`-and-record-receipts loop is workflow-agnostic; only `build_actions` is content-pipeline-specific |
| `ResearchIngressDispatchNode` | [`research_agent/ingress_dispatch.rs`](../../../crates/engine-core/src/workflows/research_agent/ingress_dispatch.rs) | Not LLM; injectable `ChannelTransport`; policy-gated no-op shape, 4-layer resolved. Hardcodes its upstream node list and a `"RESEARCH_AGENT"` literal |

Both sit on top of the [`ChannelTransport` seam](../atoms/channel-io.md).

## The refactor

Generalize into one shared "ingress-dispatch" Node type parameterized by:

- the source-node list (currently hardcoded per instance)
- the workflow-type-name literal (`"RESEARCH_AGENT"` vs. content_pipeline's equivalent)
- the actions-builder closure (`build_actions` differs by workflow)

## Why it wasn't caught earlier

The doc-comment cross-reference in `research_agent` reads as attribution for a design decision, not
as an open TODO — same pattern as the [`PersistToBrainNode`](persist-to-brain.md) duplication.

## See also

- [`../atoms/channel-io.md`](../atoms/channel-io.md) — the `ChannelTransport` seam both nodes sit on
- [`persist-to-brain.md`](persist-to-brain.md) — the sibling "documented but never consolidated" pattern
