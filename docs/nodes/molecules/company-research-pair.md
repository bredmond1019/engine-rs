---
type: Reference
title: "Molecule: Company-Research Pair"
description: research_agent::CompanyResearchNode and proposal_generator::ProposalCompanyResearchNode independently implement the same WebSearch-driven research-brief scaffold — proposal_generator's own doc admits it adapts research_agent's node.
doc_id: nodes-molecules-company-research-pair
layer: [engine]
project: engine-rs
status: active
keywords: [companyresearch, websearch, near-molecule, research agent, proposal generator]
related: [nodes-molecules-index, research-agent-workflow, proposal-generator-workflow, nodes-gaps]
---

# Molecule: Company-Research Pair

- **Near-molecule — documented copy, not consolidated**
- `proposal_generator::ProposalCompanyResearchNode`'s own doc comment states it adapts
  `research_agent::CompanyResearchNode`
- Both are also individually flagged in [`../gaps.md`](../gaps.md) §2 for hand-rolled transport
  (no `Cancellable`)

## The two instances

| Node | File | Notes |
|---|---|---|
| `CompanyResearchNode` | [`research_agent/company_research.rs`](../../../crates/engine-core/src/workflows/research_agent/company_research.rs) | LLM node, hand-rolled transport, WebSearch-driven |
| `ProposalCompanyResearchNode` | [`proposal_generator/company_research.rs`](../../../crates/engine-core/src/workflows/proposal_generator/company_research.rs) | Same prompt/schema/WebSearch shape, same hand-rolled transport. Properly reuses the shared `CompanyBrief` schema type — only the node body itself is duplicated |

## The refactor

Extract a shared WebSearch-research Node shape parameterized by prompt-builder and scope (company
vs. prospecting), then migrate both onto `TransportSlotted`/`Cancellable` in the same pass — doing
the transport migration separately from the de-duplication would mean touching this code twice.

## See also

- [`../gaps.md`](../gaps.md) §2 — both nodes' transport gap
- [`../../workflows/research-agent.md`](../../workflows/research-agent.md), [`../../workflows/proposal-generator.md`](../../workflows/proposal-generator.md)
