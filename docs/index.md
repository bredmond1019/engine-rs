---
type: Index
title: engine-rs Docs
description: Navigation index for all engine-rs reference documentation
doc_id: docs-index
layer: [meta]
project: engine-rs
status: active
keywords: [documentation, index, navigation, engine-rs, reference]
related: [core:engine-rs]
---

# engine-rs — Documentation Index

| Doc | What it covers |
|---|---|
| [architecture.md](architecture.md) | Overview, module map, core types, data flow |
| [cli.md](cli.md) | Synopsis, subcommands, global flags, exit codes, examples |
| [data-contract.md](data-contract.md) | Pinned orchestrator data-contract version, field mappings to `engine_contract` Rust types, HTTP surface parity, re-pin checklist |
| [sdlc-flow-workflow.md](sdlc-flow-workflow.md) | The `SDLC_FLOW` graph — node roles (model vs. deterministic), triggering from `engine-rs`/bastion (event `policy`/`profile` fields), stopping a run, reading outputs, inspecting/resuming state |
| [sdlc-flow-policy.md](sdlc-flow-policy.md) | SDLC Flow's tunable `SdlcPolicy` (model tiers, review mode, verbosity, local-model tier) — 4-layer resolution, the four named policy profiles (`baseline`, `cheap-fast`, `pragmatist`, `batch-reviewer`), structured-output adoption, and `RunOutcomes` telemetry/aggregation |
| [research-agent-workflow.md](research-agent-workflow.md) | The `RESEARCH_AGENT` graph — dual-mode router (company brief vs. prospecting), event schema, tunable `ResearchAgentPolicy` + named profiles, triggering, and reading `research-agent-state.json` outputs |
| [diagnostic-intake-workflow.md](diagnostic-intake-workflow.md) | The `DIAGNOSTIC_INTAKE` graph — single-node structured extraction from raw diagnostic-call notes/transcript, event schema, tunable `DiagnosticIntakePolicy` (with a Local-tier rewire) + named profiles, triggering, and reading `diagnostic-intake-state.json` outputs |
| [proposal-generator-workflow.md](proposal-generator-workflow.md) | The `PROPOSAL_GENERATOR` graph — seven-node research-to-persist pipeline scoring/ranking/drafting/reviewing an `AutomationRoadmap`, event schema, tunable `ProposalGeneratorPolicy` (with Local-tier rewires) + named profiles, the engine↔brain `PersistToBrainNode` boundary, triggering, and reading outputs |
| [content-pipeline-workflow.md](content-pipeline-workflow.md) | The `CONTENT_PIPELINE` graph — fourteen-node envelope-based, channel-agnostic content workflow (fetch/normalize -> summarize -> bounded self-critic loop -> optional translate -> digest render -> persist -> dispatch), the `IngressEnvelope` contract, event schema, tunable `ContentPipelinePolicy` (with Local-tier rewires) + named profiles, the engine↔brain `PersistToBrainNode` boundary, the `ActionDispatchNode` egress boundary, triggering, and reading outputs |

For project strategy and current focus, see [`planning/`](../planning/index.md).
