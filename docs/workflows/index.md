---
type: Index
title: engine-rs Workflow Docs
description: Navigation index for the per-workflow reference docs — one row per file in docs/workflows/.
doc_id: workflows-index
layer: [meta]
project: engine-rs
status: active
keywords: [documentation, index, navigation, workflows, engine-rs]
related: [docs-index, workflows-readme]
---

# engine-rs — Workflow Docs Index

**Start at [README.md](README.md)** — the capability catalogue: what every workflow does in plain
English, and how to trigger one. This page is just the file listing.

| Doc | What it covers |
|---|---|
| [README.md](README.md) | **The capability catalogue.** Every registered workflow, what it does, how to trigger it, and the prerequisites |
| [policy-and-profiles.md](policy-and-profiles.md) | How to change cost/speed/quality without editing Rust — the four-layer precedence, the per-workflow profile table, and running a stage on a local model |
| [sdlc-flow.md](sdlc-flow.md) | The `SDLC_FLOW` graph — node roles, commit topology, triggering, stopping a run, reading and resuming state |
| [sdlc-flow-policy.md](sdlc-flow-policy.md) | `SDLC_FLOW`'s own knobs — model tiers, review mode, verbosity, its five named profiles, and `RunOutcomes` telemetry |
| [sdlc-flow-smoke.md](sdlc-flow-smoke.md) | Proving `SDLC_FLOW` end to end from bastion-web — the six operational prerequisites, the trigger/watch/verify/cleanup procedure, and the resume hazard |
| [sdlc-task.md](sdlc-task.md) | The `SDLC_TASK` graph — the lean implement/test/triage loop, its three terminal statuses, the policy surface, and dispatch over HTTP |
| [orchestration.md](orchestration.md) | The `ORCHESTRATION` workflow — chaining `SDLC_FLOW`/`SDLC_TASK` runs across repos, the dependency and admission gates, and the `lane-log.jsonl` contract |
| [debrief.md](debrief.md) | The `DEBRIEF` workflow — rendering a morning brief from one campaign's journal, the bail-naming guarantee, and the dispatch/write-back split |
| [recall.md](recall.md) | The `RECALL` workflow — querying Synapse's `GET /recall` mid-run over the injectable `HttpGet` seam, the dispatch-step shape, and the skip-next branch it drives |
| [research-agent.md](research-agent.md) | The `RESEARCH_AGENT` graph — company brief vs. prospecting, the anti-fabrication contract, and the self-feeding trigger into `CONTENT_PIPELINE` |
| [diagnostic-intake.md](diagnostic-intake.md) | The `DIAGNOSTIC_INTAKE` graph — single-node structured extraction from call notes, with a Local-tier rewire |
| [proposal-generator.md](proposal-generator.md) | The `PROPOSAL_GENERATOR` graph — the seven-node research-to-persist pipeline and the locale-firewalled rate card |
| [deliverable-render.md](deliverable-render.md) | The `DELIVERABLE_RENDER` graph — roadmap to locale-correct markdown plus a `typst` PDF, and the `authored_locale` mismatch refusal |
| [content-pipeline.md](content-pipeline.md) | The `CONTENT_PIPELINE` graph — the sixteen-node envelope-based content workflow and its egress boundary |
| [linkedin-post.md](linkedin-post.md) | The `LINKEDIN_POST` graph — drafting from real fleet work, the traceability invariant, and the brand-rubric critic loop |
| [lead-ingest.md](lead-ingest.md) | The `LEAD_INGEST` graph — the two-node inbound-lead write, its idempotency, and the untrusted-input boundary |
| [opportunity-edit.md](opportunity-edit.md) | The `OPPORTUNITY_SET_STAGE` / `OPPORTUNITY_ADD_ACTION` micro-workflows — payloads, the seam operation, and the error surface |
| [approve-and-run.md](approve-and-run.md) | The `APPROVE_AND_RUN` micro-workflow — draining pending records, resolving a verdict into a ledger row plus an authorized execution |
| [terminal-probe.md](terminal-probe.md) | The `TERMINAL_PROBE` graph — the read-only session/observe diagnostic for the terminal stack |

Docs for capabilities that are **not** workflows — the crate architecture, the CLI, the data
contract, terminal internals, suspend/resume, the harvest gate — are one level up in
[`../index.md`](../index.md).
