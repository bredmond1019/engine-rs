---
type: Index
title: engine-rs Molecule Nodes Index
description: Every proven or near-duplicate Node sequence (a molecule) — the file listing for docs/nodes/molecules/, with the concrete refactor for each near-molecule.
doc_id: nodes-molecules-index
layer: [engine]
project: engine-rs
status: active
keywords: [molecules, index, navigation, duplication, reuse, engine-rs]
related: [nodes-index, nodes-atoms-index, nodes-gaps]
---

# Molecule Nodes — Index

- A **molecule** is 2+ Nodes used together the same way by more than one workflow — proven if the
  code is literally shared, near-molecule if it's the same shape hand-copied
- Found by comparing every registered workflow graph's Node sequence — see
  [`../../workflows/README.md`](../../workflows/README.md) for the full graph shapes
- **4 proven molecules**, **4 near-molecule/duplicate-pair clusters** below

## Proven molecules (code is literally shared today)

| Molecule | Sequence | Shared between | Doc |
|---|---|---|---|
| SDLC intake→implement/test/triage prefix | `SetupWorktreeNode → SpecExistsRouterNode → [GenerateTasksNode] → LoadTaskStateNode → TaskQueueRouterNode → ImplementTaskNode → TestTaskNode → TriageTaskNode` | `sdlc_flow`, `sdlc_task` (identical Rust types) | [sdlc-intake-prefix.md](sdlc-intake-prefix.md) |
| Doc-plus-contacts ingest | `MaterializeDocNode → MergeContactsNode` | `lead_ingest`, `research_agent` (identical types, identical `with_source_nodes` convention) | [doc-plus-contacts-ingest.md](doc-plus-contacts-ingest.md) |

## Near-molecules / duplicate-pair clusters (same shape, hand-copied)

| Cluster | What's duplicated | Where | Doc |
|---|---|---|---|
| Queue-drain loop | Router/save/back-edge shape | `sdlc_flow`/`sdlc_task` (shared) vs. `claim_reaffirm` (hand-copied) | [queue-drain-loop.md](queue-drain-loop.md) |
| Bounded critic/revise loop | Critic → router → (exit \| increment → revise → back-edge) | `proposal_generator` (generic `loop_combinator`) vs. `content_pipeline`/`linkedin_post` (hand-rolled) | [critic-revise-loop.md](critic-revise-loop.md) |
| `PersistToBrainNode` | Whole node, ~700 lines each | `content_pipeline` vs. `proposal_generator` | [persist-to-brain.md](persist-to-brain.md) |
| Fetch content trio | Injectable-fetch-then-normalize shape | `content_pipeline`'s `FetchArticleNode`/`FetchTranscriptNode`/`NormalizeChannelContentNode` | [fetch-content-trio.md](fetch-content-trio.md) |
| Research-dispatch pair | Send-via-`ChannelTransport`-and-record-receipts | `content_pipeline::ActionDispatchNode` vs. `research_agent::ResearchIngressDispatchNode` | [research-dispatch-pair.md](research-dispatch-pair.md) |
| Company-research pair | WebSearch-driven research-brief scaffold | `research_agent::CompanyResearchNode` vs. `proposal_generator::ProposalCompanyResearchNode` | [company-research-pair.md](company-research-pair.md) |

## Irreducibly workflow-specific (checked, not gaps)

These were compared against every other workflow and found to have no reusable sub-sequence — named
here so the next audit doesn't re-check them from scratch:

- `DELIVERABLE_RENDER` (`RenderDeliverableNode → RenderPdfNode`) — the only PDF-rendering workflow
- `TERMINAL_PROBE` (`TerminalSessionNode → TerminalObserveNode`) — a diagnostic, not business work
- `COMMANDER` (`CommanderDrainNode → CommanderTriageNode`) — deliberately fuses drain+emit+commit+log
  into one node for transactional coherence under one lock/timestamp; splitting it produces a
  fragment with no independent meaning
- `ORCHESTRATION` / `DEBRIEF` — each a single node by explicit design; internal complexity is
  function composition, not graph composition, on purpose
- All single-node micro-workflows (`RECALL`, `HARVEST_APPROVE`, `CONSOLIDATE`, `SWEEP`,
  `DIAGNOSTIC_INTAKE`, `OPPORTUNITY_SET_STAGE`/`ADD_ACTION`, `APPROVE_AND_RUN`) — trivially
  non-decomposable, no molecule question applies
- `content_pipeline::TranslateSkipRouterNode` vs. `linkedin_post::TranslateGateNode` — deliberately
  **not** unified; `linkedin_post`'s own module doc explains the divergence was evaluated (different
  upstream key conventions) and rejected on purpose, not missed

## See also

- [`../atoms/index.md`](../atoms/index.md) — single Nodes, not sequences
- [`../gaps.md`](../gaps.md) — trait/config gaps found on the Nodes inside these molecules
