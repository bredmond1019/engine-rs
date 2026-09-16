---
type: Reference
title: Brain / Content Atom Nodes
description: Nodes that write to the Brain corpus, the harvest/approval gate in front of that write, recall from Synapse, and their injectable seams.
doc_id: nodes-atoms-brain-content
layer: [engine]
project: engine-rs
status: active
keywords: [materialize, harvest, recall, brain, docmaterializer, opportunity]
related: [nodes-atoms-index, materialize-doc-node, harvest-gate, nodes-molecules-persist-to-brain, nodes-molecules-doc-plus-contacts-ingest]
---

# Brain / Content Atom Nodes

- Writing an artifact into the Brain corpus, the approval gate in front of that write, and querying
  the Brain back mid-run → table below
- `MaterializeDocNode` is this repo's clearest proven atom — used by 4 workflows through one
  injectable seam
- `PersistToBrainNode` exists **twice**, independently, doing the same job — see the
  [molecule doc](../molecules/persist-to-brain.md) rather than building a third copy

## Nodes

| Node | File | Verdict | Evidence | Own doc |
|---|---|---|---|---|
| `MaterializeDocNode` | [`nodes/materialize_doc.rs`](../../../crates/engine-core/src/nodes/materialize_doc.rs) | **atom** | `model` is a plain `String` ctor arg (not a closed enum); writer goes through injectable `Arc<dyn DocMaterializer>`; used by content_pipeline, claim_reaffirm, research_agent, lead_ingest | [`../../materialize-doc-node.md`](../../materialize-doc-node.md) |
| `doc_materializer` seam | [`nodes/doc_materializer.rs`](../../../crates/engine-core/src/nodes/doc_materializer.rs) | seam | The injectable seam `MaterializeDocNode` calls (mev/okf-core in-process) | same |
| `HarvestMode` / `HarvestGate` | [`nodes/harvest_gate.rs`](../../../crates/engine-core/src/nodes/harvest_gate.rs) | seam | Generic materialize→harvest gate (`off`/`in_process`/`approval`) every pipeline inherits | [`../../harvest-gate.md`](../../harvest-gate.md) |
| `HarvestApproveNode` | [`nodes/harvest_approve.rs`](../../../crates/engine-core/src/nodes/harvest_approve.rs) | **atom** | Generic `{artifact_id,url,payload,doc_paths}` record; POSTs via injectable `Arc<dyn HttpPost>`; auth optional/layered, never hardcoded. Used by two independent workflows (`approve_and_run`, `harvest_approve`) | [`../../harvest-gate.md`](../../harvest-gate.md), [`../../approval-ledger.md`](../../approval-ledger.md) |
| `RecallNode` | [`nodes/brain_client.rs`](../../../crates/engine-core/src/nodes/brain_client.rs) | **atom** | Fully seam-driven (`HttpGet`, `BrainConfig`, `InputBinding` query source); used across ≥3 workflows (claim_reaffirm, recall, orchestration) | [`../../workflows/recall.md`](../../workflows/recall.md) |
| `brain_client` (`HttpGet` seam + `BrainConfig`) | `nodes/brain_client.rs` | seam | Injectable HTTP-GET seam for Synapse's `GET /recall` | same |
| `OpportunityEditNode` | [`nodes/opportunity_edit.rs`](../../../crates/engine-core/src/nodes/opportunity_edit.rs) | **near-atom** | Same seam pattern as `MaterializeDocNode`, but `OpportunityEditOp` is a closed 2-variant enum tied to `SetOpportunityStageEvent`/`AddOpportunityActionEvent` field names; single-workflow use only | [`../../workflows/opportunity-edit.md`](../../workflows/opportunity-edit.md) |
| `MergeContactsNode` | [`nodes/merge_contacts.rs`](../../../crates/engine-core/src/nodes/merge_contacts.rs) | **specific** | Hardcodes the "opportunity" business shape (`company_name`/`vertical`/`contacts`/`prospects` literal fields) mirroring okf-core/mev schema directly in the node body; reused by research_agent/lead_ingest only because both happen to emit that exact shape — see [doc-plus-contacts-ingest molecule](../molecules/doc-plus-contacts-ingest.md) | — |

**`OpportunityEditNode` refactor**: genericize `build_edit` to take a caller-supplied
event→`OpportunityEdit` mapping closure/trait instead of a fixed enum, mirroring
`MaterializeDocNode`'s plain-string `model` field.

## Duplicate flagged for consolidation

`PersistToBrainNode` exists in both `content_pipeline::persist_to_brain` and
`proposal_generator::persist_to_brain` — structurally identical (same fields/builders/
`resolve_target`), one file's doc comment literally names the other. **[will consolidate — see
molecule doc](../molecules/persist-to-brain.md)** rather than building a third instance elsewhere.

## See also

- [`../molecules/persist-to-brain.md`](../molecules/persist-to-brain.md) — the duplicate-pair consolidation plan
- [`../molecules/doc-plus-contacts-ingest.md`](../molecules/doc-plus-contacts-ingest.md) — `MaterializeDocNode -> MergeContactsNode`, already shared between two workflows
- [`../../operator-payload-contract.md`](../../operator-payload-contract.md) — what happens after a harvest gate defers to a human
