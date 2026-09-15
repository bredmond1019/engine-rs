---
type: Reference
title: "Molecule: Doc-Plus-Contacts Ingest"
description: The MaterializeDocNode -> MergeContactsNode sequence shared literally between LEAD_INGEST and RESEARCH_AGENT.
doc_id: nodes-molecules-doc-plus-contacts-ingest
layer: [engine]
project: engine-rs
status: active
keywords: [materializedoc, mergecontacts, molecule, lead ingest, research agent]
related: [nodes-molecules-index, lead-ingest, research-agent-workflow, nodes-atoms-brain-content]
---

# Molecule: Doc-Plus-Contacts Ingest

- **Proven, not a candidate** — identical Node types, identical `with_source_nodes` wiring
  convention, identical ordering in both workflows
- Sequence: `MaterializeDocNode → MergeContactsNode`
- Used by `LEAD_INGEST` and `RESEARCH_AGENT` — [`lead-ingest.md`](../../workflows/lead-ingest.md),
  [`research-agent.md`](../../workflows/research-agent.md)

## Node-by-node

| Node | File | Verdict |
|---|---|---|
| `MaterializeDocNode` | [`nodes/materialize_doc.rs`](../../../crates/engine-core/src/nodes/materialize_doc.rs) | atom — see [`brain-content.md`](../atoms/brain-content.md) |
| `MergeContactsNode` | [`nodes/merge_contacts.rs`](../../../crates/engine-core/src/nodes/merge_contacts.rs) | specific — hardcodes the "opportunity" schema (`company_name`/`vertical`/`contacts`/`prospects`), so it only reuses because both workflows happen to emit that exact shape |

## Why this is a molecule and not two coincidences

Both `crates/engine-core/src/workflows/lead_ingest/mod.rs` and
`crates/engine-core/src/workflows/research_agent/graph.rs` wire the pair with the same
`with_source_nodes` convention and the same node ordering — a third workflow producing an
opportunity-shaped document should reuse this exact two-node sequence rather than re-deriving it.

## The one real risk in this molecule

`MergeContactsNode`'s hardcoded field names mean a **third** workflow can only reuse this sequence
if it also happens to emit the opportunity schema verbatim. Generalizing it would mean either a
schema-detection seam or a caller-supplied field-mapping closure (same shape as the
`OpportunityEditNode` refactor in [`brain-content.md`](../atoms/brain-content.md)) — not yet needed
because only these two workflows exist today, but worth doing *before* a third caller shows up with
a slightly different shape and forks it instead.

## See also

- [`../atoms/brain-content.md`](../atoms/brain-content.md) — `MaterializeDocNode`'s full atom entry
- [`persist-to-brain.md`](persist-to-brain.md) — the other Brain-write duplication cluster in this repo
