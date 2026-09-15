---
type: Reference
title: Transport / LLM Atom Nodes
description: Every Node that spawns or calls a model, and the shared transport-selection machinery every one of them must implement — TransportSlotted/Cancellable, resolve_meta_transport, and the per-backend transports.
doc_id: nodes-atoms-transport-llm
layer: [engine]
project: engine-rs
status: active
keywords: [transport, llm, agentcodestep, transportslotted, cancellable, resolve_meta_transport, judgment]
related: [nodes-atoms-index, nodes-gaps, workflows-readme]
---

# Transport / LLM Atom Nodes

- Everything that spawns or calls a model, plus the shared seam that replaced 16+ hand-rolled
  per-node transport copies → §1 (traits), §2 (the one true atom Node), §3 (per-backend transports)
- **Load the `create-llm-node` project skill before adding a new LLM-calling Node** — this page
  names the shape, that skill has the recipe
- Nodes that *should* use this shape but don't yet are tracked in [`../gaps.md`](../gaps.md) §1–2,
  not repeated here

## 1 — The seam itself

| What | File | Notes |
|---|---|---|
| `TransportSlotted` / `Cancellable` | [`workflows/llm_node.rs`](../../../crates/engine-core/src/workflows/llm_node.rs) | The shared node-side traits every model-calling Node must implement (CLAUDE.md standing rule 11). Its own doc comment is the authority on the shape and migration history |
| `TransportSlot` | [`workflows/transport_slot.rs`](../../../crates/engine-core/src/workflows/transport_slot.rs) | The one `Option<ModelTransport>` field + builder pair every local-eligible node composes instead of duplicating it |
| `resolve_meta_transport` / `wire` | `workflows/llm_node.rs` | The graph-side call that decides local-vs-cloud routing per stage — never a hand-written `if tier == Local { ... }` |

## 2 — The atom

| Node | File | Verdict | Evidence |
|---|---|---|---|
| `AgentCodeStep` | [`nodes/agent_code_step.rs`](../../../crates/engine-core/src/nodes/agent_code_step.rs) | **atom** | Spawns a Claude Code session via `claude_code_rs::execute`, maps `Outcome` into `NodeRun`/`TaskContext` (D4). Identity/prompt fully injected (`PromptSource::Fixed`/`Builder`) — *is* the seam other nodes wrap, not a hand-rolled field. Constructed by 24+ workflow files |

`JudgmentNode<T>` ([`nodes/judgment.rs`](../../../crates/engine-core/src/nodes/judgment.rs)) is not
a `Node` itself — it composes `AgentCodeStep` and exposes `judge()` — but is a generic,
`TransportSlotted`/`Cancellable`-correct seam: a reusable, bounded, schema-constrained `claude`
call (byte-capped input, model tier, turn ceiling, typed `JudgmentError`). Used by two orchestration
runners. Easy to miss because it's generic (`T`) rather than proven by a same-named second caller.

## 3 — Per-backend transports (config-resolved, no hardcoded model)

| Node / module | File | One-line purpose |
|---|---|---|
| `AgentOutcome` / `CostEstimate` | [`nodes/agent_outcome.rs`](../../../crates/engine-core/src/nodes/agent_outcome.rs) | Backend-agnostic result shape a non-`claude_cli` transport reports in, so it slots into `AgentCodeStep::MetaTransport` unchanged |
| `PiTransport` | [`nodes/pi_transport.rs`](../../../crates/engine-core/src/nodes/pi_transport.rs) | `AgentBackend::Pi` implementation of `MetaTransport` |
| `AiderTransport` | [`nodes/aider_transport.rs`](../../../crates/engine-core/src/nodes/aider_transport.rs) | `AgentBackend::Aider` implementation of `MetaTransport`, mirrors `PiTransport` |
| `openai_compat_transport` | [`nodes/openai_compat_transport.rs`](../../../crates/engine-core/src/nodes/openai_compat_transport.rs) | The `local` model tier's transport — POSTs to an OpenAI-compatible `/v1/chat/completions` (e.g. local Ollama) instead of spawning `claude` |

Full local-vs-cloud routing story, per-backend knobs, and every measured local-model pitfall:
[`../../workflows/policy-and-profiles.md`](../../workflows/policy-and-profiles.md) and
[`../../local-model-bench.md`](../../local-model-bench.md).

## Every LLM-calling Node in the repo, and its transport status

Most LLM-calling Nodes live inside one workflow (`content_pipeline::SummarizeNode`,
`claim_reaffirm::JudgeClaimNode`, etc.) and are correctly `specific` — the prompt and schema are
workflow-bound even where the transport shape should be shared. Their transport compliance
(atom-shape vs. hand-rolled vs. missing entirely) is tracked centrally in
[`../gaps.md`](../gaps.md) §1–2 rather than duplicated here, so there is exactly one place to check
whether a given Node already follows rule 11.

## See also

- [`../gaps.md`](../gaps.md) — every LLM-calling Node still missing this shape, and why
- [`../../workflows/policy-and-profiles.md`](../../workflows/policy-and-profiles.md) — the four-layer Policy resolution `AgentCodeStep`'s model tier reads
