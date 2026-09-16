---
name: create-molecule
description: Guardrail before wiring a new multi-node sequence (a "molecule") into a workflow graph — check whether the shape already exists elsewhere (queue-drain loop, bounded critic/revise loop, doc-plus-ingest, SDLC intake prefix), and use the existing generic combinator instead of hand-rolling a cyclic edge or router. Use BEFORE adding a new loop/cycle to any workflow graph.rs, and before writing a second router that reads a verdict field and picks a target.
allowed-tools: Bash(rg:*) Bash(grep:*) Bash(cat:*)
---

# Before composing a new molecule (multi-node sequence) in engine-rs

Scoped to `core/engine-rs`. A **molecule** is a small, named sequence of atom nodes that recurs
across workflows — the level between a single `Node` (an atom) and a full workflow graph. Read
`create-node` first if you're not yet sure the pieces you're composing are the right atoms.

> **The governing principle (AGENTS.md standing rule 12, engine-rs CLAUDE.md standing rule 6):**
> lean config-heavy, avoid hardcoding, build extendible single-purpose interfaces. At the molecule
> level this means: a recurring shape (a loop, a router, a fan-out/gather) should be a generic
> combinator parameterized over the varying piece (a verdict type, an item type, a target
> identity), not a concrete graph wiring copied per workflow. Ask this *before* wiring the edges,
> not after a second workflow needs the same shape.

## Why this exists

The 2026-09-15 molecule audit compared every workflow graph's node sequence and found the same
shapes hand-copied instead of shared, every time because nobody checked first:

- **Queue-drain loop** (`{QueueRouter} -> body -> {SaveNode} -> back-edge to QueueRouter`, exit
  when drained): shared correctly between `sdlc_flow`/`sdlc_task` (same Rust types), then
  **independently hand-copied** in `claim_reaffirm` (`ClaimQueueRouterNode`/`SaveVerdictNode`) —
  that module's own doc comment admits it's a copy of `sdlc_flow`'s idiom.
- **Bounded critic/revise loop** (`{Critic} -> {Router: verdict+capped} -> exit | increment+revise
  +loop`): `proposal_generator` already built the fully generic version
  (`crate::loop_combinator::build_loop`/`LoopSpec`/`LoopCluster`) — and `content_pipeline` and
  `linkedin_post` each hand-rolled their **own** `CriticRouterNode` instead of using it, for no
  reason discovered in either module's history.
- **Doc-plus-contacts ingest** (`MaterializeDocNode -> MergeContactsNode`) and the **SDLC
  intake/implement/test/triage prefix** are both already correctly shared — proof this works when
  someone checks first.

## Step 1 — does this shape already exist?

Read `docs/nodes/molecules/index.md` and the closest molecule doc. Ask specifically:

- **Is this a bounded loop** (a node produces output, a router grades it against a verdict field,
  loop-with-increment on fail up to a cap, exit on pass or cap)? → Use
  `crate::loop_combinator::{build_loop, LoopSpec, LoopCluster}`. Do not hand-wire a cyclic graph
  edge with your own counter/cap check — that is the exact duplication this skill exists to stop.
- **Is this a queue-drain loop** (a router pops the next item, processes it, saves state, loops
  until the queue is empty)? → Check `sdlc_flow::task_loop`'s `TaskQueueRouterNode`/`SaveStateNode`
  shape first. If your queue's item/status types differ, that's a real generic-combinator gap
  (there is no `QueueDrainRouter<Item,Status>` yet — `claim_reaffirm`'s hand-copy is exactly this
  gap materializing twice) — consider whether extracting one now costs less than a third hand-copy.
- **Is this "gather doc + persist to brain"?** → Check `PersistToBrainNode`
  (`content_pipeline::persist_to_brain` and `proposal_generator::persist_to_brain` are currently
  two structurally-identical hand copies — do not add a third; if you need this, that's the
  trigger to finally consolidate them into one `crate::nodes`-level type).
- **Is this "fetch content from a source, normalize, then continue"?** → Check the fetch trio
  (`content_pipeline::{fetch_article, fetch_transcript, normalize_channel_content}`) before writing
  a fourth fetch-shaped node; consider whether a `FetchContentNode<F: Fetch>` generic now serves
  your case too.
- **Is this "notify an operator and branch on approve/reject/discuss"?** → This is
  `crates/engine-core/src/operator/` composed the way `workflows::approve_and_run` does it — see
  `create-node`'s operator/ section. Not a molecule question, a seam-reuse question.

## Step 1a — even if nothing fits yet, should this be generic from day one?

Don't wait for a second workflow to need this shape before making it a combinator. If, while
wiring this molecule, you can already name what would vary for a plausible second caller (a
different verdict field, a different item/status pair, a different router target set), design that
in now as a type parameter or a config struct rather than hand-wiring concrete identities into the
graph edges. `loop_combinator::build_loop`/`LoopSpec` exists precisely because `proposal_generator`
generalized its critic/revise loop instead of leaving it concrete — and `content_pipeline` and
`linkedin_post` both hand-rolled their own copy later anyway, either because they didn't know the
generic existed or because reaching for "just wire the edges" felt faster in the moment. Reaching
for the generic costs you a few extra minutes now; reaching for a fresh hand-copy costs the next
three readers a "wait, which router is this" moment and eventually a consolidation task.

## Step 2 — if nothing fits, is the new shape actually novel?

Some workflows are irreducibly workflow-specific (`COMMANDER`'s drain-triage fusion,
`ORCHESTRATION`/`DEBRIEF` as deliberate single-node function composition, `DELIVERABLE_RENDER`'s
PDF-only render step) — not every multi-node graph is a hidden molecule. The test: would a
*different* workflow's author, given only the shape (not the field names), recognize this as "the
thing I need too"? If yes, it's a molecule even if this is its first instance — document it as one
so the second workflow that needs it finds it instead of re-deriving it.

## Step 3 — document it

New or newly-named molecule: add `docs/nodes/molecules/<slug>.md` — the node sequence, which
workflows use or could use it, why it's genuinely reusable (or only a near-molecule, with the
concrete refactor named), file paths for every instance. Update
`docs/nodes/molecules/index.md`'s table and cross-link from each consuming workflow's doc in
`docs/workflows/`. If you found and fixed a hand-copy while doing this, name the collapsed copy in
the doc so a future reader sees the before/after, not just the after.

**Name what you deliberately left concrete, and why.** If you chose a hand-wired edge over a
generic combinator for a real reason (the shape only has one caller and is unlikely to gain a
second, or genericizing now would obscure more than it saves), say so in the molecule doc rather
than leaving it silent — a documented "not generic, and here's why" is a fine outcome; an
undocumented one is what the next audit rediscovers as a gap.
