---
name: create-node
description: Guardrail before writing any new engine-rs Node (atom) — check whether it already exists, whether it should compose an existing injectable seam instead of a hardcoded value, and whether it's actually an LLM-calling node (route to create-llm-node instead/also). Use BEFORE writing a new struct implementing `crate::node::Node`, and before adding a new field to an existing node that hardcodes a string/name/path a future caller might want to vary.
allowed-tools: Bash(rg:*) Bash(grep:*) Bash(cat:*)
---

# Before creating a new node in engine-rs

Scoped to `core/engine-rs`. This is the general node-design checklist; if the node makes an LLM
call, this skill hands off to **`create-llm-node`** for the transport/cancellation shape — read
both, this one first.

> **The governing principle (AGENTS.md standing rule 12, engine-rs CLAUDE.md standing rule 6):**
> lean config-heavy, avoid hardcoding strings/data/names, and build extendible, single-purpose
> interfaces. Every value that trades cost, safety, quality, or identity is a config knob or an
> injected type — never a literal baked into the node body. This is not a style preference to
> apply after the fact; it is the design question to ask *before* the first line of a new node,
> and it governs every step below.

## Why this exists

A 2026-09-15 fleet-wide audit of `crates/engine-core/src/nodes/` and every `workflows/*/` module
found the same failure mode repeated a dozen times: a node gets hand-written from scratch that is
90% identical to one that already exists, because nothing prompted a search first. Concrete
casualties, found in one pass: `PersistToBrainNode` implemented twice byte-for-byte
(`content_pipeline` and `proposal_generator`, one's own doc comment naming the other by path);
`ReviseNode` hand-implemented three separate times (`content_pipeline`, `linkedin_post`,
`proposal_generator`'s `ProposalReviseNode`); `CompanyResearchNode`/`ProposalCompanyResearchNode`
as an admitted copy; a fetch-node trio (`FetchArticleNode`/`FetchTranscriptNode`/
`NormalizeChannelContentNode`) that could be one generic node. None of these were malicious or
even careless — each author reasonably didn't know the other one existed. The fix is a five-minute
search before writing, every time.

## Step 1 — does this already exist?

Read `docs/nodes/atoms/index.md` (grouped by category: Transport/LLM, Control flow, Brain/Content,
Channel IO, Terminal, etc.) and skim the group file closest to what you're building. Then grep for
the shape directly — a doc lags the code:

```bash
rg -L 'impl Node for' crates/engine-core/src/nodes crates/engine-core/src/workflows --glob '!target'
```

If you find a node that does 80%+ of what you need, the default move is **parameterize it further
and reuse it**, not fork it. A node that hardcodes one workflow's identity but is otherwise generic
(e.g. `OpportunityEditNode`'s closed 2-variant edit-op enum) is a **near-atom** — the fix is
widening its constructor/config, not writing a sibling.

Only write a new node when the search turns up nothing close, or when what's close is genuinely
architecturally different (not just "different field values").

## Step 2 — is it actually generic, or does it just look like it?

Before the first line of code, name:

- **What varies per call site?** Every one of those things is a constructor param or builder
  method, never a literal in the body. Test: *if a future caller might reasonably want a different
  value here, it's a knob.* This applies to strings (node names, target identities, target-language
  codes, filenames, path templates), not just numbers and enums — `linkedin_post::TranslateGateNode`'s
  hardcoded `TRANSLATE_TARGET_LANG = "pt-BR"` and `sdlc_flow::PullRequestNode`'s hardcoded
  `--base main`/PR title template are real, already-flagged instances of this exact gap. Read the
  field list back out loud: any field that is a fixed value instead of a parameter is a candidate
  to fix now, while the node is new, not later once three callers depend on the fixed behavior.
- **Should this field be a generic type parameter or a trait object instead of a concrete type?**
  Don't wait for a second caller to force the question — if you can already see that a future node
  might reasonably want to substitute its own behavior for one piece of this node (a different
  fetch source, a different render target, a different verdict shape, a different persistence
  backend), design that piece behind a small trait or a generic (`struct Foo<F: Fetch>`,
  `Arc<dyn SomeSeam>`) from the start. This is exactly what turns a one-off into an atom: contrast
  `MaterializeDocNode`'s injectable `Arc<dyn DocMaterializer>` (reused by 4 workflows because the
  seam was there from day one) against the fetch trio (`FetchArticleNode`/`FetchTranscriptNode`/
  `NormalizeChannelContentNode`), three concrete nodes that could have been one
  `FetchContentNode<F: Fetch>` if the trait had been named up front instead of after the third copy
  existed. When in doubt, name the trait even for a single caller — an unused generality costs a
  few lines; a missed one costs a rewrite plus every caller that copied the concrete version in the
  meantime.
- **What's the injectable seam, if any?** This repo already has named seams for the common cases —
  reuse them rather than inventing a parallel one:
  - `HttpPost` / `HttpGet` — outbound HTTP, mockable in tests (`content_pipeline`,
    `proposal_generator`, `harvest_approve`, `approve_and_run`, `RecallNode` all use these).
  - `ChannelTransport` — operator-facing notification delivery (Slack/Telegram/WhatsApp pending
    adapters named, don't invent a parallel channel enum).
  - `DocMaterializer` — corpus doc writes (`MaterializeDocNode`).
  - `CommandRunner` / `TerminalDriver` — process/tmux execution seams.
  - `InputBinding` — reading an upstream node's output by name. **A hardcoded upstream node-name
    string (`ctx.nodes.get("some_literal_name")`) is a real, flagged gap** — several audited nodes
    do this instead of `InputBinding`; don't add another one.
  - `Policy` four-layer resolution (event override > profile > `harness.json` > builtin default) —
    any cost/latency/quality-trading value goes here, not a `const`.
  - `crates/engine-core/src/operator/{payload,channel,queue,ledger,transport}.rs` — the generic
    human-decision subsystem (`OperatorPayload`, `OperatorChannel::{Notification,Session}`,
    `OperatorQueue`, `operator::ledger`, `OperatorTransport`). If you are about to build "notify a
    human and wait for a tap," this is the seam — see `workflows::approve_and_run` as the worked
    example. Do not shell a notification CLI directly from a node; depend on the trait.
- **Does it make an LLM call?** If yes, stop here and go read **`create-llm-node`** — every node
  that constructs an `AgentCodeStep` (or equivalent) and picks a model implements
  `TransportSlotted`/`Cancellable` and resolves through `resolve_meta_transport`. This is a hard
  rule (standing rule 11), not optional for a node that "just does one small model call."

## Step 3 — is this actually two responsibilities in one node?

If the node does research-then-write, or fetch-then-normalize, or decide-then-notify, check
whether it should be two atoms composed into a **molecule** instead of one monolith. Smaller atoms
route independently to different model tiers and are individually reusable; a fused node forces
every future caller to take the whole bundle. See **`create-molecule`** if you're now looking at a
multi-node sequence rather than one node.

## Step 4 — document it

Every new atom gets an entry in `docs/nodes/atoms/<group>.md` (create the group file if none fits)
with: what it's for and when to reach for it; the workflow doc(s) that use it; the trait/seam it
implements (or should implement); if it's a known duplicate slated for consolidation, a
`[will consolidate with <Node>]` note linking the molecule doc; file path + key struct/method
names. Update `docs/nodes/atoms/index.md`'s table and the directory's `index.md` (AGENTS.md
standing rule 7).

If you deliberately chose NOT to reuse something close (like `linkedin_post::CriticRouterNode`
correctly not reusing `content_pipeline`'s same-named router because the identities are hardcoded
to a different workflow), say so in the node's own doc comment — a documented divergence is
evidence the search happened; a silent one reads as another accident later.

**If you had to hardcode something you couldn't config-ify** (an external tool's binary name, a
wire-format-fixed header, a value pinned by a contract another repo owns), say so explicitly in the
node's doc comment and in its `docs/nodes/atoms/<group>.md` entry — a named, justified hardcode is
fine (AGENTS.md rule 12's "where feasible" qualifier); an *unnamed* one is what gets silently
copied into the next three call sites. This is also where you flag a gap you noticed but didn't
fix — e.g. `commander::CommanderTriageNode` making an LLM call with no transport/cancellation seam
at all, or `TerminalObserveNode` hardcoding a `static CLAUDE_MANIFEST` instead of taking the
`ManifestSource` seam that already exists two files away. Naming the gap in the doc is what lets
the next person find it in five minutes instead of rediscovering it in an audit.
