---
type: Reference
title: engine-rs Node Gaps Registry
description: Nodes missing a trait/seam CLAUDE.md standing rule 6/11/12 says they should have, and hardcoded values a future run might want to vary — recorded once so they are never rediscovered from scratch by the next audit.
doc_id: nodes-gaps
layer: [engine]
project: engine-rs
status: active
keywords: [gaps, transport, cancellable, hardcoded, extensibility, rule 11, rule 12]
related: [nodes-index, nodes-atoms-transport-llm, nodes-atoms-index]
---

# Node Gaps Registry

- Found by the September 2026 node/molecule audit — this page is the **standing record**, not a
  one-time finding; add to it, don't let it go stale
- Grouped by kind of gap: no transport seam at all → §1, hand-rolled transport (known, deferred) →
  §2, hardcoded value that should be config → §3
- Every row names the file so the fix is a known quantity, not a re-discovery

## 1 — No transport/cancellation seam at all (worse than hand-rolled)

These make an LLM call with **zero** transport abstraction — not even a hand-rolled
`Option<ModelTransport>` field. Per CLAUDE.md standing rule 11, every LLM-calling Node must
implement `TransportSlotted`/`Cancellable` from
[`workflows/llm_node.rs`](../../crates/engine-core/src/workflows/llm_node.rs).

| Node | File | What's missing | Notes |
|---|---|---|---|
| `CommanderTriageNode` | [`commander/mod.rs`](../../crates/engine-core/src/workflows/commander/mod.rs) via `commander/triage.rs::build_triage_step` | No `TransportSlot`, no `Cancellable`, no `ModelTier`/`Policy` resolution at all — `model: Option<String>` is read straight off `ctx.event.get("model")` | `commander` has no `policy.rs`/`profiles.rs` module the way other workflows do — no local-vs-cloud routing, no baseline/cheap-fast/thorough profile. Not on `llm_node.rs`'s own list of deferred nodes (§2) — a genuinely new find, not documented debt |

## 2 — Hand-rolled transport (known, deferred by the operator)

These implement a transport field by hand (`Option<ModelTransport>` or a bare `TransportSlot`
struct) instead of `impl TransportSlotted` + `impl Cancellable`. `llm_node.rs`'s own module doc
names this as **Phase 3 narrowed / deliberately deferred by the operator** — migration is
mechanical (see `create-llm-node` skill) but has not been prioritized.

| Node | File | Cancellation support? |
|---|---|---|
| `JudgeClaimNode` | [`claim_reaffirm/judge.rs`](../../crates/engine-core/src/workflows/claim_reaffirm/judge.rs) | No |
| `SummarizeNode` | [`content_pipeline/summarize.rs`](../../crates/engine-core/src/workflows/content_pipeline/summarize.rs) | No |
| `SelfCriticNode` | [`content_pipeline/self_critic.rs`](../../crates/engine-core/src/workflows/content_pipeline/self_critic.rs) | No |
| `ReviseNode` (content_pipeline) | [`content_pipeline/revise.rs`](../../crates/engine-core/src/workflows/content_pipeline/revise.rs) | No |
| `TranslateNode` | [`content_pipeline/translate.rs`](../../crates/engine-core/src/workflows/content_pipeline/translate.rs) | No |
| `IntakeExtractNode` | [`diagnostic_intake/extract.rs`](../../crates/engine-core/src/workflows/diagnostic_intake/extract.rs) | No — uses `resolved_policy_strict` correctly (single resolution point) otherwise |
| `PostDraftNode` | [`linkedin_post/draft.rs`](../../crates/engine-core/src/workflows/linkedin_post/draft.rs) | No |
| `ReviseNode` (linkedin_post) | [`linkedin_post/revise.rs`](../../crates/engine-core/src/workflows/linkedin_post/revise.rs) | No |
| `TranslateGateNode` | [`linkedin_post/graph.rs`](../../crates/engine-core/src/workflows/linkedin_post/graph.rs) | No |
| `BrandCriticNode` | [`linkedin_post/brand_critic.rs`](../../crates/engine-core/src/workflows/linkedin_post/brand_critic.rs) | Has a `TransportSlot` struct at least — smaller migration step than the raw-`Option` cases |
| `ProposalReviseNode` | [`proposal_generator/revise.rs`](../../crates/engine-core/src/workflows/proposal_generator/revise.rs) | No |
| `ProposalReviewNode` | [`proposal_generator/review.rs`](../../crates/engine-core/src/workflows/proposal_generator/review.rs) | No |
| `OpportunityIdentifierNode` | [`proposal_generator/opportunity_identifier.rs`](../../crates/engine-core/src/workflows/proposal_generator/opportunity_identifier.rs) | No |
| `ProposalWriterNode` | [`proposal_generator/writer.rs`](../../crates/engine-core/src/workflows/proposal_generator/writer.rs) | No |
| `ProspectingResearchNode` | [`research_agent/prospecting.rs`](../../crates/engine-core/src/workflows/research_agent/prospecting.rs) | No |
| `CompanyResearchNode` | [`research_agent/company_research.rs`](../../crates/engine-core/src/workflows/research_agent/company_research.rs) | No — see [duplicate pair](molecules/company-research-pair.md) |
| `ProposalCompanyResearchNode` | [`proposal_generator/company_research.rs`](../../crates/engine-core/src/workflows/proposal_generator/company_research.rs) | No — see [duplicate pair](molecules/company-research-pair.md) |

**Also part of this bucket, at the workflow-graph level**: `content_pipeline/graph.rs`,
`proposal_generator/graph.rs`, and `diagnostic_intake/graph.rs` still hand-roll
`if policy.model_tiers.<stage> == ModelTier::Local { node.with_meta_transport(...) }` per call site
in `build_registry`, instead of routing through `resolve_meta_transport`/`wire` the way
`orchestration`, `sdlc_flow`, and `sdlc_task` already do.

## 3 — Hardcoded values that should be config (rule 12)

| Node / module | File | The hardcoded value | Why it's a knob |
|---|---|---|---|
| `TranslateGateNode` | `linkedin_post/graph.rs` — `TRANSLATE_TARGET_LANG` const | `"pt-BR"`, explicitly commented "not a policy knob" | A future run may want a different target locale without a code change |
| `TranslateNode` | `content_pipeline/translate.rs` — `target_lang()` fallback | `"pt-BR"` default | Same shape, borderline — defensible as a *default*, but worth resolving through Policy like every other tunable |
| `PullRequestNode` | [`sdlc_flow/pr.rs`](../../crates/engine-core/src/workflows/sdlc_flow/pr.rs) | `--base main`, PR title template, PR body text — no builder override | `harness.json`'s `flow.prBase` exists but is unread; a repo whose default branch isn't `main` has no way to override |
| `EndReviewNode` | [`sdlc_flow/end_review.rs`](../../crates/engine-core/src/workflows/sdlc_flow/end_review.rs) | Fallback diff base `"main"` | Same acknowledged gap as `PullRequestNode` — `flow.prBase` unread here too |
| `terminal_probe::registry()` | [`workflows/terminal_probe/graph.rs`](../../crates/engine-core/src/workflows/terminal_probe/graph.rs) | `DEFAULT_DRIVER_TIMEOUT = Duration::from_secs(10)` baked into the default `TmuxDriver` construction | No `policy.rs`/`profiles.rs` module exists for this workflow at all; `registry_with(driver)` lets a caller inject a pre-built driver with its own timeout, but that requires writing Rust, not a runtime/event override the way every other workflow's tunables work |
| `content_pipeline::CriticRouterNode` | `content_pipeline/critic_router.rs` | Downstream target node name as a string literal, with a stale comment ("module doesn't exist yet" — it now does) | Replace with `translate::SKIP_ROUTER_NODE_NAME` import |
| `content_pipeline::TranslateSkipRouterNode` | `content_pipeline/translate.rs` | Same pattern for its own downstream target | Replace with `digest_render::NODE_NAME` import |
| `operator::ledger::LedgerDecision` | [`operator/ledger/record.rs`](../../crates/engine-core/src/operator/ledger/record.rs) | Closed 4-variant enum (`Approved`/`Skipped`/`RoutedToSession`/`Requeued`) with no extension point | A workflow needing a `Rejected` or `Discuss` outcome cannot add one without editing this shared type — every existing ledger row's `decision` field also depends on it. **Contrast**: `operator::channel::OperatorChannel` (`Notification`/`Session{slug}`) is the model to follow — see `operator-payload-contract.md` |
| `approve_and_run::render.rs` | [`workflows/approve_and_run/render.rs`](../../crates/engine-core/src/workflows/approve_and_run/render.rs) | Exactly three response options as `pub const OPTION_APPROVE`/`OPTION_SKIP`/`OPTION_OPEN_SESSION`, pattern-matched in `verdict.rs::decide` | Partly a real channel constraint (`operator::limits::WHATSAPP_MAX_REPLY_BUTTONS = 3`), but the *option set itself* is not config — a second gate wanting approve/reject/discuss cannot reuse this module and must fork it |

## See also

- [`atoms/transport-llm.md`](atoms/transport-llm.md) — the `TransportSlotted`/`Cancellable` seam these nodes should adopt
- [`molecules/index.md`](molecules/index.md) — duplicate-pair clusters (some rows above double as molecule entries)
- `create-llm-node` project skill — the migration recipe for §1/§2
