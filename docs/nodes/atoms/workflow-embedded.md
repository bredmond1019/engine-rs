---
type: Reference
title: Workflow-Embedded Near-Atom Nodes
description: Atom-quality Nodes currently living inside a single workflows/<name>/ directory — reusable in shape, not yet promoted or reused a second time.
doc_id: nodes-atoms-workflow-embedded
layer: [engine]
project: engine-rs
status: active
keywords: [workflow-embedded, near-atom, sdlc, reuse, promote]
related: [nodes-atoms-index, sdlc-flow-workflow, sdlc-task-workflow, orchestration-workflow, deliverable-render-workflow]
---

# Workflow-Embedded Near-Atoms

- Nodes that live inside one workflow's own directory (`workflows/<name>/`) but are generic enough
  in shape to reuse elsewhere — some already proven, most not yet promoted to `nodes/`
- The **sdlc_flow → sdlc_task reuse is this repo's one existing example of the target pattern
  working as designed** — 13 Nodes pre-built with builder overrides specifically so a second, leaner
  workflow could reuse them unmodified → §1
- Everything else here is a single-workflow atom-quality Node worth knowing about before you
  hand-roll a near-duplicate elsewhere → §2

## 1 — sdlc_flow Nodes already reused verbatim by sdlc_task

| Node | File | Reused by |
|---|---|---|
| `SetupWorktreeNode`, `SpecExistsRouterNode`, `LoadTaskStateNode`, `GenerateTasksNode` | [`sdlc_flow/setup.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/setup.rs) | `sdlc_task/graph.rs` (all 4, imported directly) |
| `ImplementTaskNode`, `TestTaskNode`, `TriageTaskNode`, `TaskQueueRouterNode`, `IncrementAttemptNode`, `UpdateTaskStatusNode`, `SaveStateNode` | [`sdlc_flow/task_loop.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/task_loop.rs) | `sdlc_task/graph.rs` (all 7). `UpdateTaskStatusNode`'s own doc/error text literally says "every route in both SDLC_FLOW and SDLC_TASK" |
| `CloseBlockNode` | [`sdlc_flow/close_block.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/close_block.rs) | `sdlc_task/graph.rs` — parameterized (`with_state_source`, `with_agent`) specifically to enable reuse |
| `FinalValidationNode` | [`sdlc_flow/final_validation.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/final_validation.rs) | `sdlc_task/graph.rs` — `ValidationScope` enum added specifically so `sdlc_task` could reuse it (D56 "Reconcile" gate) |

This is the sequence documented as the [`sdlc-intake-prefix` molecule](../molecules/sdlc-intake-prefix.md);
this table is the per-node view, that page is the sequence view. Full graphs:
[`../../workflows/sdlc-flow.md`](../../workflows/sdlc-flow.md), [`../../workflows/sdlc-task.md`](../../workflows/sdlc-task.md).

## 2 — Other workflow-embedded near-atoms

| Node | Home workflow | File | Verdict | Notes |
|---|---|---|---|---|
| `WorkSourceNode` | linkedin_post | [`linkedin_post/work_source.rs`](../../../crates/engine-core/src/workflows/linkedin_post/work_source.rs) | **atom** | Fully generic: injectable `CommandRunner`/`FileReader`/`DirReader`/`fleet_root`, no LLM, no hardcoded workflow name in logic. Reusable as-is for any git/log-history need |
| `RenderPdfNode` | deliverable_render | [`deliverable_render/render_pdf.rs`](../../../crates/engine-core/src/workflows/deliverable_render/render_pdf.rs) | **atom** | Injectable `CommandRunner`; mirrors `sdlc_flow::end_review::EndReviewNode`'s `with_runner` pattern per its own doc. Hardcodes typst binary/filename template inline — an external-tool choice, not really a cost knob | [`../../workflows/deliverable-render.md`](../../workflows/deliverable-render.md) |
| `DebriefNode` | orchestration | [`orchestration/debrief.rs`](../../../crates/engine-core/src/workflows/orchestration/debrief.rs) | **near-atom** | No LLM call; injectable `JournalReader`/`ChannelTransport` seams; `dispatch_workflow_type` and `draft_language` already overridable via builders. Only workflow-specific bits: `render_brief`'s digest format and the `DebriefRendered` journal-kind coupling. Refactor: parameterize the renderer + journal-decision-kind to generalize into any "campaign digest" workflow | [`../../workflows/debrief.md`](../../workflows/debrief.md) |
| `PullRequestNode` | sdlc_flow | [`sdlc_flow/pr.rs`](../../../crates/engine-core/src/workflows/sdlc_flow/pr.rs) | **near-atom** | Deterministic git/gh; hardcodes `--base main`, PR title template, PR body text with no builder override — [flagged gap](../gaps.md). Refactor: add `with_base_branch`/`with_title_template`/`with_body` builders sourced from `harness.json`'s `flow.prBase` |
| `ClaimRecallNode` | claim_reaffirm | [`claim_reaffirm/judge.rs`](../../../crates/engine-core/src/workflows/claim_reaffirm/judge.rs) | **near-atom** | Thin wrapper over the generic `nodes::RecallNode`; only workflow-specific bit is query formatting. Refactor: extract query-format/limit into a param |
| `RenderReportNode` | claim_reaffirm | [`claim_reaffirm/render_report.rs`](../../../crates/engine-core/src/workflows/claim_reaffirm/render_report.rs) | **near-atom** | Generic `ReportFs` + fixed-path-write shape (mirrors `nodes::materialize_doc`); only claim-specific bits are the renderer fn and a hardcoded default path. Refactor: split into a generic `WriteMarkdownReportNode<Renderer>` |
| `ActionDispatchNode` | content_pipeline | [`content_pipeline/action_dispatch.rs`](../../../crates/engine-core/src/workflows/content_pipeline/action_dispatch.rs) | **near-atom** | The send-via-injectable-`ChannelTransport`-and-record-receipts loop is workflow-agnostic; only `build_actions` is content-pipeline-specific — see [research-dispatch-pair molecule](../molecules/research-dispatch-pair.md) |

## See also

- [`../molecules/sdlc-intake-prefix.md`](../molecules/sdlc-intake-prefix.md) — the §1 sequence, documented as a molecule
- [`../gaps.md`](../gaps.md) — `PullRequestNode`'s hardcoded base branch
