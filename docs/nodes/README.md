---
type: Reference
title: The Node Library
description: Every reusable Node implementation in engine-core — what it does, its file, and whether a dedicated doc or config knob already exists — grouped by purpose so a new workflow can find and reuse one instead of writing a duplicate.
doc_id: nodes-readme
layer: [engine]
project: engine-rs
status: active
keywords: [nodes, node library, catalogue, reuse, extensibility, transport, terminal, operator]
related: [docs-index, architecture, workflows-readme, terminal-nodes, operator-payload-contract, materialize-doc-node, harvest-gate, suspend-resume]
---

# The Node Library

- A **node** is one typed unit of work in a workflow graph — implements `Node` (`crate::node`,
  [`architecture.md`](../architecture.md) § Core Types), transforms a `TaskContext`, never touches
  its own run/timing envelope → §1
- This page groups every reusable node by purpose so a new workflow reuses one instead of
  hand-rolling a near-duplicate → §2–6
- File listing only (no descriptions): [index.md](index.md)
- Nodes with their own full doc are **linked, not repeated** here — this page is the map, not the
  detail
- **Extensibility flags** (§7) — nodes found during this sweep that hardcode a value CLAUDE.md
  standing rule 6/11 says should be a config knob or an extensible enum

## Quickstart — reusing a node

1. Find the category below closest to what you need.
2. Check the "own doc" column — if one exists, read it; it has the full contract, seams, and knobs.
3. If no doc exists, read the file's own module-level `//!` doc comment first — every node in this
   repo documents its block ID, its injectable seam (if any), and why it lives where it does.
4. If the node composes an LLM call, load the **`create-llm-node`** project skill before writing
   a new one — `TransportSlotted`/`Cancellable` (§4) exist precisely so you don't hand-roll a
   transport field.

## 1 — The `Node` interface itself

- Every node in every table below implements this one trait.

| What | File | Notes |
|---|---|---|
| `Node` trait + `NodeRegistry` | [`crates/engine-core/src/node.rs`](../../crates/engine-core/src/node.rs) | `process(&self, ctx) -> Result<TaskContext, NodeError>` — a node only transforms context, never its own status/timing |
| `NodeExt::with_identity` | same file | Wrap any node with an instance identity so the same node *type* can appear more than once in one graph |
| `NodeExt::with_input_from` / `InputBinding` | same file | Declarative upstream-input wiring between nodes, instead of a node reading a hardcoded upstream key |
| `build_loop` / `LoopSpec` | [`loop_combinator.rs`](../../crates/engine-core/src/loop_combinator.rs) | Reusable bounded-loop builder (`{guard router, increment node, back-edge}`) — generalizes the hand-written retry idiom `sdlc_flow` used to hand-roll per workflow |

## 2 — Agent / LLM transport nodes

- Everything that spawns or calls a model, and the shared transport-selection machinery that
  replaced 16+ hand-rolled per-node copies.
- **Load the `create-llm-node` project skill before adding a new one.**

| Node / module | File | One-line purpose | Own doc |
|---|---|---|---|
| `AgentCodeStep` | [`nodes/agent_code_step.rs`](../../crates/engine-core/src/nodes/agent_code_step.rs) | Spawns a Claude Code session via `claude_code_rs::execute`, maps `Outcome` into `NodeRun`/`TaskContext` (D4) | — |
| `AgentOutcome`/`CostEstimate` | [`nodes/agent_outcome.rs`](../../crates/engine-core/src/nodes/agent_outcome.rs) | Backend-agnostic result shape a non-`claude_cli` transport reports in, so it slots into `AgentCodeStep::MetaTransport` unchanged | — |
| `PiTransport` | [`nodes/pi_transport.rs`](../../crates/engine-core/src/nodes/pi_transport.rs) | `AgentBackend::Pi` implementation of `MetaTransport` | — |
| `AiderTransport` | [`nodes/aider_transport.rs`](../../crates/engine-core/src/nodes/aider_transport.rs) | `AgentBackend::Aider` implementation of `MetaTransport`, mirrors `PiTransport` | — |
| `openai_compat_transport` | [`nodes/openai_compat_transport.rs`](../../crates/engine-core/src/nodes/openai_compat_transport.rs) | The `local` model tier's transport — POSTs to an OpenAI-compatible `/v1/chat/completions` (e.g. local Ollama) instead of spawning `claude` | — |
| `JudgmentNode` | [`nodes/judgment.rs`](../../crates/engine-core/src/nodes/judgment.rs) | Reusable, bounded, schema-constrained `claude` call: byte-capped input, model tier, turn ceiling, typed `JudgmentError` | — |
| `LlmNode` / `TransportSlotted` / `Cancellable` | [`workflows/llm_node.rs`](../../crates/engine-core/src/workflows/llm_node.rs) | The shared node-side shape + `resolve_meta_transport`/`wire` graph-side resolution every model-calling node must implement (standing rule 11) | — |
| `TransportSlot` | [`workflows/transport_slot.rs`](../../crates/engine-core/src/workflows/transport_slot.rs) | The one `Option<ModelTransport>` field + builder every local-eligible node composes instead of duplicating it | — |

## 3 — Control flow / composition nodes

- Nodes that shape the graph itself rather than doing external work.

| Node | File | One-line purpose | Own doc |
|---|---|---|---|
| `FanOutNode` | [`nodes/fan_out.rs`](../../crates/engine-core/src/nodes/fan_out.rs) | Constructs N identity-distinguished instances of the same node type, runs them concurrently via `ParallelNode` | — |
| `AggregateNode` | [`nodes/aggregate.rs`](../../crates/engine-core/src/nodes/aggregate.rs) | Joins N `FanOutNode`-produced `ctx.nodes` entries into one deterministically-ordered `Vec` | — |
| `SuspendNode` | [`nodes/suspend.rs`](../../crates/engine-core/src/nodes/suspend.rs) | Requests the walk suspend after this node (finalization belongs to `Workflow::walk`) | [suspend-resume.md](../suspend-resume.md) |

## 4 — Brain / content materialization nodes

- Writing an artifact into the Brain corpus, and the approval gate in front of that write.

| Node | File | One-line purpose | Own doc |
|---|---|---|---|
| `MaterializeDocNode` | [`nodes/materialize_doc.rs`](../../crates/engine-core/src/nodes/materialize_doc.rs) | Generic node every pipeline appends to write a `BrainDocModel` artifact into the Brain corpus | [materialize-doc-node.md](../materialize-doc-node.md) |
| `doc_materializer` seam | [`nodes/doc_materializer.rs`](../../crates/engine-core/src/nodes/doc_materializer.rs) | The injectable seam `MaterializeDocNode` calls (mev/okf-core in-process) | [materialize-doc-node.md](../materialize-doc-node.md) |
| `HarvestMode`/`HarvestGate` | [`nodes/harvest_gate.rs`](../../crates/engine-core/src/nodes/harvest_gate.rs) | Generic materialize→harvest gate (`off`/`in_process`/`approval`) every pipeline inherits | [harvest-gate.md](../harvest-gate.md) |
| `HarvestApproveNode` | [`nodes/harvest_approve.rs`](../../crates/engine-core/src/nodes/harvest_approve.rs) | Completes a deferred harvest: reads the pending record, POSTs its payload | [harvest-gate.md](../harvest-gate.md), [approval-ledger.md](../approval-ledger.md) |
| `OpportunityEditNode` | [`nodes/opportunity_edit.rs`](../../crates/engine-core/src/nodes/opportunity_edit.rs) | Drives one `set-stage`/`add-action` edit against the `DocMaterializer` seam | [workflows/opportunity-edit.md](../workflows/opportunity-edit.md) |
| `MergeContactsNode` | [`nodes/merge_contacts.rs`](../../crates/engine-core/src/nodes/merge_contacts.rs) | Collects contacts a `RESEARCH_AGENT` run surfaced, merges into the opportunity doc just written | — |
| `brain_client` | [`nodes/brain_client.rs`](../../crates/engine-core/src/nodes/brain_client.rs) | Injectable HTTP-GET seam for Synapse's `GET /recall`, plus shared `BrainConfig` | — |

## 5 — Channel / outbound IO nodes

- Generic HTTP and channel-delivery primitives other nodes compose rather than hand-roll their own
  client.

| Node | File | One-line purpose | Own doc |
|---|---|---|---|
| `http_post` (`HttpPost` seam) | [`nodes/http_post.rs`](../../crates/engine-core/src/nodes/http_post.rs) | Injectable HTTP-POST seam `PersistToBrainNode` calls to push a finished artifact to an ingest endpoint | — |
| `HttpRequestNode` | [`nodes/http_request.rs`](../../crates/engine-core/src/nodes/http_request.rs) | General-purpose HTTP node: configure URL/body/headers, get `{status, body}` back — reuses the `HttpPost` seam rather than a new client | — |
| `channel_transport` | [`nodes/channel_transport.rs`](../../crates/engine-core/src/nodes/channel_transport.rs) | Injectable egress seam `ActionDispatchNode` calls to deliver a `CONTENT_PIPELINE` run's outbound action to its originating channel | — |
| Email adapter (`email/`) | [`nodes/email/`](../../crates/engine-core/src/nodes/email/) — `transport.rs`, `inbound.rs`, `webhook_events.rs` | Resend-backed outbound send + both inbound webhook paths | [email-adapter.md](../email-adapter.md) |

## 6 — Terminal / tmux and operator-approval nodes

- Both already have thorough dedicated docs — **do not duplicate here, follow the links.**

| Area | Own doc |
|---|---|
| Session identity, observe, guarded send, bounded await, admission control, held sessions, `LiveClaudeSessionNode` | [terminal-nodes.md](../terminal-nodes.md) |
| `TerminalProbe` diagnostic graph | [workflows/terminal-probe.md](../workflows/terminal-probe.md) |
| Operator payload / queue / transport / channel primitives | [operator-payload-contract.md](../operator-payload-contract.md) |
| Approval ledger (`record_decision`, digest-mismatch requeue) | [approval-ledger.md](../approval-ledger.md) |
| `APPROVE_AND_RUN` micro-workflow (harvest-specific composition of the above) | [workflows/approve-and-run.md](../workflows/approve-and-run.md) |

## 7 — Extensibility flags found during this sweep

Per CLAUDE.md standing rule 6 (config over hardcoding) and rule 11 (no hand-rolled transport
selection). These are **observations for future work, not defects fixed by this doc pass.**

- **`workflows/approve_and_run/render.rs`** hardcodes exactly three response options as
  `pub const OPTION_APPROVE`/`OPTION_SKIP`/`OPTION_OPEN_SESSION`, and
  **`workflows/approve_and_run/verdict.rs`** pattern-matches those three literal keys in `decide`.
  This is a real channel constraint in part (`operator::limits::WHATSAPP_MAX_REPLY_BUTTONS = 3`),
  but the option *set itself* (which three, and what each means) is not config — a second gate
  wanting a different three options (e.g. approve/reject/discuss) cannot reuse this module and must
  fork it.
- **`operator::ledger::LedgerDecision`** (`operator/ledger/record.rs`) is a closed, hand-written
  enum (`Approved`, `Skipped`, `RoutedToSession`, `Requeued`) with no extension point — a workflow
  needing a `Rejected` or `Discuss` outcome cannot add one without editing this shared type, which
  every existing ledger row's `decision` field also depends on.
- **`operator::channel::OperatorChannel`** is the one instance in this area that *does* follow the
  principle well: a two-variant enum (`Notification` / `Session{slug}`) that generalizes cleanly and
  is worth modeling any new "how does this decision reach a human" surface on, rather than on
  `approve_and_run`'s fixed-option pattern above.
- No further hardcoded literals were found in the nodes tabulated above beyond the two flagged —
  most (transport backends, HTTP/channel seams, materialize/harvest nodes) already resolve
  model tier, URL, and mode through `Config`/`Policy`/seam injection rather than a baked-in value.

### Workflow-graph sweep (rule 6 + rule 11, `workflows/*/graph.rs`)

- **Rule 11 is only partially followed.** `orchestration`, `sdlc_flow`, and `sdlc_task` route
  local-vs-cloud entirely through `llm_node::resolve_meta_transport`/`wire`, but
  **`content_pipeline/graph.rs`** (`build_registry`, 4 call sites: `summarize`/`critic`/`revise`/
  `translate`), **`proposal_generator/graph.rs`** (3 call sites: `opportunity`/`review`/`revise`),
  and **`diagnostic_intake/graph.rs`** (`extract`) still hand-roll
  `if policy.model_tiers.<stage> == ModelTier::Local { node.with_meta_transport(...) }` per stage,
  the exact shape rule 11 exists to retire. This is *documented* deferred debt, not an undiscovered
  gap — `workflows/llm_node.rs`'s own module doc comment names these five workflows (plus
  `claim_reaffirm::judge::JudgeClaimNode` and `linkedin_post`'s node) as "Phase 3 narrowed... per
  operator direction," migration "later-phase work, deliberately deferred by the operator for now."
  Flagging here because the task asked whether the shape still exists in workflow graphs: it does,
  in three of them.
- **`commander/triage.rs::build_triage_step`** (called from `commander/mod.rs`'s
  `CommanderTriageNode::process`) is a gap the `llm_node.rs` doc comment does *not* list: its one
  `AgentCodeStep` takes a raw `model: Option<String>` read straight off `ctx.event.get("model")`
  with no `ModelTier`, no `Policy`/profile resolution, and no `TransportSlotted`/`Cancellable`
  impl at all — not even the hand-rolled pattern above. `commander` has no `policy.rs`/`profiles.rs`
  the way `content_pipeline`/`proposal_generator`/etc. do, so this one LLM call site has no
  local-vs-cloud routing capability and no baseline/cheap-fast/thorough profile surface — the model
  string is whatever the caller happens to pass, full stop.
- **`terminal_probe/graph.rs::DEFAULT_DRIVER_TIMEOUT`** (`Duration::from_secs(10)`) is a literal
  baked into `registry()`'s default `TmuxDriver` construction rather than resolved through any
  config surface — there is no `terminal_probe` policy/profile module at all (the module doc
  correctly notes there's no `ModelTier` to resolve here, since it has no LLM call, but the driver
  timeout is a separate cost/reliability knob that trades the same way rule 12 describes). Mitigated
  in part by `registry_with(driver)`, which lets a caller inject a pre-built `TmuxDriver` with its
  own timeout — but that requires writing Rust at the call site, not a runtime/event override the
  way every other workflow's tunables work.

## See also

- [`../architecture.md`](../architecture.md) — crate layout, `AppState`, injectable seams, data flow
- [`../workflows/README.md`](../workflows/README.md) — the workflow-level capability catalogue (a
  workflow is a graph of the nodes on this page)
- [`../coming-soon.md`](../coming-soon.md) — planned capabilities, including any planned node, that
  do not exist yet
