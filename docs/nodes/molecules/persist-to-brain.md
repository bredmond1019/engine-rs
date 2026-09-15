---
type: Reference
title: "Molecule: PersistToBrainNode Duplication"
description: content_pipeline and proposal_generator each independently built a ~700-line PersistToBrainNode, structurally identical, one file's doc comment naming the other. The clearest should-have-been-one-node case in the audit.
doc_id: nodes-molecules-persist-to-brain
layer: [engine]
project: engine-rs
status: active
keywords: [persisttobrain, duplication, near-molecule, content pipeline, proposal generator]
related: [nodes-molecules-index, nodes-atoms-brain-content, content-pipeline-workflow, proposal-generator-workflow]
---

# Molecule: `PersistToBrainNode` Duplication

- **Near-molecule — highest-value single consolidation target found in the audit**
- Two ~700-line independent implementations, structurally identical (same fields, same builders,
  same `resolve_target` logic)
- One file's own doc comment names the other by path — the duplication is documented, just never
  consolidated

## The two instances

| Instance | File | Differs by |
|---|---|---|
| `content_pipeline::PersistToBrainNode` | [`content_pipeline/persist_to_brain.rs`](../../../crates/engine-core/src/workflows/content_pipeline/persist_to_brain.rs) | Payload shape, has `HarvestGate` present |
| `proposal_generator::PersistToBrainNode` | [`proposal_generator/persist_to_brain.rs`](../../../crates/engine-core/src/workflows/proposal_generator/persist_to_brain.rs) | Payload shape, own doc comment literally cites content_pipeline's `resolve_target` as the pattern it mirrors |

## Why this sat undetected

Nothing in-repo flagged the duplication as an issue — the doc comment cross-reference reads as
attribution ("this mirrors X"), not as a TODO. No prior sweep compared the two files side by side
until this audit did.

## The refactor

Extract shared `BrainConfig` + `HttpPost` + builders + `resolve_target` into one
`crate::nodes::brain_client`-style type, parameterized by:

- a payload-builder closure (each workflow's document shape differs)
- the ingest path
- an optional `HarvestGate` (present in `content_pipeline`, absent in `proposal_generator`)

This is the same "config-heavy, single-purpose, extendible" test CLAUDE.md standing rule 12 asks
for — the two existing files already show exactly which parts vary (payload/gate) and which don't
(config, HTTP, target resolution), so the generic type's parameter list is already known.

## See also

- [`../atoms/brain-content.md`](../atoms/brain-content.md) — the atom-level Brain-write catalogue this consolidation would join
- [`doc-plus-contacts-ingest.md`](doc-plus-contacts-ingest.md) — the other proven Brain-ingest molecule
