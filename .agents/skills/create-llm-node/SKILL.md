---
name: create-llm-node
description: >
  How to add a new LLM-calling node or seam in engine-rs without reinventing
  transport/cancellation boilerplate — the TransportSlotted/Cancellable traits every
  model-calling node must implement, resolve_meta_transport/wire for the graph-side
  local-vs-cloud routing decision, and the concrete cost this repo paid twice for
  skipping it. Scoped to engine-rs only. Use BEFORE writing any new node/function that
  constructs an AgentCodeStep (or equivalent) and picks a model, and BEFORE adding a
  hand-rolled `if tier == Local { ... }` conditional to any graph.rs.
allowed-tools: Bash(rg:*) Bash(grep:*) Bash(cat:*)
---

# Adding a new LLM-calling node in engine-rs

This skill is scoped to `core/engine-rs` only — the trait it describes lives there
(`crates/engine-core/src/workflows/llm_node.rs`) and nothing else in the fleet has it.

## The rule (engine-rs `AGENTS.md` standing rule 11)

Every node/seam that makes an LLM call implements `crate::workflows::llm_node::
{TransportSlotted, Cancellable}` — never a hand-rolled transport field or a bespoke
model-tier-to-transport resolution. Read `llm_node.rs`'s own doc comment before writing anything;
it is the authority on the shape, not this skill.

## Why this is a hard rule, not a suggestion

engine-rs paid for skipping this twice:

1. **Before `llm_node.rs` existed** (`EN.ticket.transport-slot-consolidation`, 2026-09-14): 16+ node
   files across `sdlc_flow`, `sdlc_task`, `content_pipeline`, `proposal_generator`,
   `diagnostic_intake`, `orchestration`, `linkedin_post`, `claim_reaffirm` each hand-wrote the
   identical `TransportSlot`-shaped field, the identical `with_meta_transport`/`with_transport`
   builder pair, and 13+ `graph.rs` files each hand-wrote the identical
   `if policy.model_tiers.X == ModelTier::Local { node.with_meta_transport(...) }` conditional per
   stage. None of it was wrong individually — it just meant every new local-eligible stage was a
   copy-paste of two patterns nobody had named.
2. **After it existed** (same day, same session): a genuinely new LLM call site —
   `engine-serve/src/journal.rs`'s D57 verification-ledger composer (`EN.15.L`,
   `compose_ledger_entries_via_agent`) — was built as a plain function that constructed an
   `AgentCodeStep` directly and picked a model string, with no transport override of any kind. It
   shipped, passed review, and sat making one real cloud API call on every passing `ORCHESTRATION`
   chain step — regardless of every other stage's tier being set to `Local` — until a dedicated
   local-model-only audit caught it well after the fact, specifically because nothing forced a
   check against the pattern for a new call site outside the already-migrated node files.

The second incident is the reason this is a standing rule and not just a trait sitting in a module
nobody is told to use: a trait with no enforcement is exactly as effective as no trait, for a call
site nobody thought to audit.

## The shape

Read `crates/engine-core/src/workflows/llm_node.rs`'s doc comment in full first — it names the
migration history and the exact node-side/graph-side split. Short version:

**Node-side** — a node (or any struct that composes an `AgentCodeStep`) exposes its transport
override through one `TransportSlot` field:

```rust
struct MyNewNode {
    transport_slot: TransportSlot,
    cancellation_token: Option<CancellationToken>,
    // ... other fields
}

impl TransportSlotted for MyNewNode {
    fn transport_slot_mut(&mut self) -> &mut TransportSlot {
        &mut self.transport_slot
    }
}

impl Cancellable for MyNewNode {
    fn cancellation_token_mut(&mut self) -> &mut Option<CancellationToken> {
        &mut self.cancellation_token
    }
}
```

The two traits' default methods give the struct `with_meta_transport`/`with_transport`/
`with_cancellation_token` for free — do not hand-write these builder bodies again.

If the call site is a plain function rather than a persistent `Node` struct (as
`compose_ledger_entries_via_agent` is — see `journal.rs`), it does not need the trait impls at all;
it composes the `AgentCodeStep` directly and applies the resolved transport inline via
`step.with_meta_transport(move |config, prompt| (meta)(config, prompt))`. Either shape must still
go through `resolve_meta_transport` below for the actual routing decision — the trait is for
node-side field/builder dedup, not the only valid entry point.

**Graph-side** — wherever a `registry_for_policy` (or equivalent one-shot construction site)
decides local-vs-cloud routing for a stage, resolve through `resolve_meta_transport` and apply
through `wire`:

```rust
registry.register(Box::new(wire(
    MyNewNode::new(),
    resolve_meta_transport(policy.model_tiers.my_stage, AgentBackend::ClaudeCli, &policy.local, &policy.pi),
    token.clone(),
)));
```

Never write a new `if tier == ModelTier::Local { node.with_meta_transport(...) }` conditional by
hand — `resolve_meta_transport` already reproduces every existing backend/tier branch
(`Pi`/`Aider` always route local regardless of tier; `ClaudeCli` + `Local` routes through the
OpenAI-compatible transport; `ClaudeCli` + any other tier is a no-op).

## What "every node/seam that makes an LLM call" actually means

If you are about to construct an `AgentCodeStep` (or the equivalent for whatever the node
ultimately dispatches through) and select a model, you are in scope for this rule — full stop, not
"only nodes that already look like the other 20-odd migrated ones." The composer incident above
happened precisely because the author reasonably thought of it as "a one-off judgment call inside
`integrate.rs`'s seam machinery," not as "a node that makes an LLM call." It is both.

## Before writing a new call site

1. Read `llm_node.rs`'s doc comment.
2. Read one already-migrated example close to what you're building — a node struct
   (`crates/engine-core/src/workflows/sdlc_flow/task_loop.rs`'s `TriageTaskNode`) or a plain
   function (`crates/engine-serve/src/journal.rs`'s `compose_ledger_entries_via_agent`).
3. Check whether the four-layer policy resolution (`event_override > profile > harness_defaults >
   builtin`) applies to your call site, or only a subset — some seams may have no per-run event to
   resolve a profile against. Say so in a doc comment if you're deliberately narrowing it; don't
   silently under-resolve. (As of 2026-09-14 the ledger composer itself was fixed to resolve all
   four layers — check its current state before citing it as an example of a narrowed seam.)
4. Give every new tier/backend-resolving knob a behavior-stable built-in default (adding the knob
   must not change what an existing run does) and, where the workflow has named profiles at all,
   set it explicitly in each one — per this repo's own standing rule 6.
