---
type: Reference
title: "Molecule: Bounded Critic/Revise Loop"
description: The critic-router-revise loop shape — proposal_generator already uses a generic combinator for it, content_pipeline and linkedin_post each hand-roll their own instead. Highest-value single consolidation in the audit.
doc_id: nodes-molecules-critic-revise-loop
layer: [engine]
project: engine-rs
status: active
keywords: [critic, revise, loop combinator, near-molecule, content pipeline, linkedin post, proposal generator]
related: [nodes-molecules-index, content-pipeline-workflow, linkedin-post-workflow, proposal-generator-workflow, nodes-atoms-control-flow]
---

# Molecule: Bounded Critic/Revise Loop

- **Near-molecule — highest-value consolidation target found in the whole audit**
- Shape: `{CriticNode} → {Router reading verdict+capped} → {exit | IncrementIterationNode →
  ReviseNode → (back-edge) CriticNode}`
- A **working generic version already exists** ([`loop_combinator::build_loop`](../../../crates/engine-core/src/loop_combinator.rs)/`LoopSpec`/`LoopCluster`)
  and is used by one of the three workflows below — the other two never migrated onto it

## The three instances

| Workflow | Critic | Router | Increment | Revise | Uses generic combinator? |
|---|---|---|---|---|---|
| `content_pipeline` | `SelfCriticNode` ([`content_pipeline/self_critic.rs`](../../../crates/engine-core/src/workflows/content_pipeline/self_critic.rs)) | `CriticRouterNode` ([`content_pipeline/critic_router.rs`](../../../crates/engine-core/src/workflows/content_pipeline/critic_router.rs)) — hand-rolled, hardcoded to `self_critic`/`source_router` identities | `IncrementCriticIterationNode` ([`content_pipeline/increment_critic_iteration.rs`](../../../crates/engine-core/src/workflows/content_pipeline/increment_critic_iteration.rs)) | `ReviseNode` ([`content_pipeline/revise.rs`](../../../crates/engine-core/src/workflows/content_pipeline/revise.rs)) | **No** |
| `linkedin_post` | `BrandCriticNode` ([`linkedin_post/brand_critic.rs`](../../../crates/engine-core/src/workflows/linkedin_post/brand_critic.rs)) | Local `CriticRouterNode` in [`linkedin_post/graph.rs`](../../../crates/engine-core/src/workflows/linkedin_post/graph.rs) — hand-rolled, deliberately distinct because content_pipeline's router is hardcoded to identities `linkedin_post` doesn't have | **Reuses `content_pipeline`'s `IncrementCriticIterationNode` directly** — genuine cross-workflow reuse, confirmed by the `use` statement | `ReviseNode` ([`linkedin_post/revise.rs`](../../../crates/engine-core/src/workflows/linkedin_post/revise.rs)) — independent 3rd implementation | **No** |
| `proposal_generator` | `ProposalReviewNode` ([`proposal_generator/review.rs`](../../../crates/engine-core/src/workflows/proposal_generator/review.rs)) | Generic — via `build_loop`/`LoopSpec` | via `LoopSpec` | `ProposalReviseNode` ([`proposal_generator/revise.rs`](../../../crates/engine-core/src/workflows/proposal_generator/revise.rs)) | **Yes** — the proof of concept |

## What's already shared vs. what's duplicated

- **`IncrementCriticIterationNode` is a real shared molecule component** — `linkedin_post` imports
  it directly from `content_pipeline`. This is the one clean win in this cluster.
- **The router half is duplicated three ways** even though a working generic combinator exists:
  `content_pipeline` and `linkedin_post` each hand-roll their own `CriticRouterNode` instead of
  using `proposal_generator`'s `build_loop`/`LoopSpec`.
- **`ReviseNode` is hand-implemented three separate times** for the identical "apply critic feedback
  to a prior draft" shape (`content_pipeline`, `linkedin_post`, `proposal_generator::ProposalReviseNode`)
  — none share code, only the shape.

## The refactor

1. Generalize `LoopSpec` to read a **named** verdict field + a `capped` bool instead of
   `proposal_generator`'s fixed shape (check the current struct in `loop_combinator.rs` before
   assuming it's already field-agnostic).
2. Migrate `content_pipeline::CriticRouterNode` and `linkedin_post`'s local `CriticRouterNode` onto
   `build_loop`. This collapses three router implementations to one.
3. Separately, consider whether `ReviseNode`'s three copies can share a single generic
   "apply-critic-feedback" Node parameterized by prompt/schema — lower priority than the router
   unification since the prompts are genuinely workflow-specific, but the *shape* (read draft + read
   critic issues + call model + return revised draft) is identical all three times.

**Why `linkedin_post` didn't just reuse `content_pipeline`'s router**: its own module doc explains
content_pipeline's `CriticRouterNode` is hardcoded to `self_critic`/`source_router`'s specific node
identities, so a direct import would have failed at runtime. This is exactly the failure mode the
`build_loop` generic already solves — `linkedin_post` should target that, not
`content_pipeline`'s router.

## See also

- [`../atoms/control-flow.md`](../atoms/control-flow.md) § 2 — `build_loop`/`LoopSpec` atom entry
- [`../gaps.md`](../gaps.md) §2 — every Node in this cluster's hand-rolled transport status
- [`../../workflows/content-pipeline.md`](../../workflows/content-pipeline.md), [`../../workflows/linkedin-post.md`](../../workflows/linkedin-post.md), [`../../workflows/proposal-generator.md`](../../workflows/proposal-generator.md)
