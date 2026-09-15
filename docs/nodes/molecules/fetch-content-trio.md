---
type: Reference
title: "Molecule: Fetch Content Trio"
description: FetchArticleNode/FetchTranscriptNode/NormalizeChannelContentNode in content_pipeline share the injectable-fetch-then-normalize shape but aren't unified into one generic FetchContentNode<F>.
doc_id: nodes-molecules-fetch-content-trio
layer: [engine]
project: engine-rs
status: active
keywords: [fetch, content pipeline, near-molecule, generic, trait]
related: [nodes-molecules-index, content-pipeline-workflow]
---

# Molecule: Fetch Content Trio

- **Near-molecule, single workflow** — three Nodes in `content_pipeline` share the same shape but
  aren't unified
- All three sit at the entry of `CONTENT_PIPELINE`'s `SourceRouterNode` fan-out

## The three Nodes

| Node | File | Notes |
|---|---|---|
| `FetchArticleNode` | [`content_pipeline/fetch_article.rs`](../../../crates/engine-core/src/workflows/content_pipeline/fetch_article.rs) | Injectable `ArticleFetch` seam; near-identical to `FetchTranscriptNode` |
| `FetchTranscriptNode` | [`content_pipeline/fetch_transcript.rs`](../../../crates/engine-core/src/workflows/content_pipeline/fetch_transcript.rs) | Own doc: "same shape as fetch_article.rs"; hardcodes upstream node-name read instead of `InputBinding` |
| `NormalizeChannelContentNode` | [`content_pipeline/normalize_channel_content.rs`](../../../crates/engine-core/src/workflows/content_pipeline/normalize_channel_content.rs) | Deterministic passthrough converging `SourcePayload` variants; also hardcodes upstream read |

## The refactor

Collapse into one generic `FetchContentNode<F: Fetch>`, parameterized by the fetch implementation
(`ArticleFetch` vs. transcript vs. passthrough-normalize), and bind the upstream source via
`InputBinding` instead of a hardcoded node-name string in the two fetch nodes.

## Why this is worth doing even though only one workflow uses it today

`CONTENT_PIPELINE` is explicitly multi-source (`SourceRouterNode` already branches on source type),
so this is the workflow most likely to gain a **fourth** source next — a generic `FetchContentNode<F>`
means that addition is a new `Fetch` impl, not a fourth hand-copied Node file.

## See also

- [`../../workflows/content-pipeline.md`](../../workflows/content-pipeline.md)
- [`critic-revise-loop.md`](critic-revise-loop.md) — the other content_pipeline consolidation opportunity
