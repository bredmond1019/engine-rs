# Generate Master Plan — Author the project roadmap as canonical block definitions.

## Variables

$ARGUMENTS — free-text description of the project, its goal, and any planning notes from a
             planning session (paste the conversation's conclusions here). Optional `--clarify`.
             If omitted, the command reads `planning/context.md` + `CLAUDE.md` and asks the user to
             describe the goal before writing.

## Purpose

Turn a free-form planning session into a `planning/master-plan.md` whose phase/block structure
`/generate-tasks` can consume **directly**. The master plan is the roadmap source of truth: a
sequence of **block definitions** (What / Why / Build notes / Acceptance criteria), each addressable
by a parseable `<Prefix>.<PhaseNumber>.<BlockLetter>` identifier (e.g. `BA.11.B`). `/generate-tasks <Prefix>.<PhaseNumber>.<BlockLetter>` later explodes one
block into a runnable `tasks.md`.

> This is the **roadmap** producer. For a single ad-hoc / experimental feature you don't yet want in
> the roadmap, use `/plan` (one standalone block) → `/generate-tasks --from`. See
> `planning/decisions/D34-adhoc-planning-seam.md`.

## Instructions

1. Run `/prime` to orient (standing rules, architecture, current state).
2. Read `planning/context.md` (why the project exists), `CLAUDE.md` (the project's actual stack +
   standing rules), and any existing `planning/master-plan.md` (you may be *revising/extending* it,
   not always writing from scratch — preserve completed phases).
3. **Clarify gate (only when enabled, OR when `$ARGUMENTS` is thin).** Read `planning/harness.json` →
   `planning.clarify`. When it is `true`, when `$ARGUMENTS` contains `--clarify`, or when the goal /
   scope / destination is genuinely underspecified, pause and ask the user **2–4 targeted clarifying
   questions** (the destination, the competence/delivery checkpoint that signals "done", the
   load-bearing architectural choices, the rough phase boundaries) before writing. Fold the answers
   in. When the gate is off and the input is already rich, proceed immediately. Strip a `--clarify`
   token before using `$ARGUMENTS` as prose.
   - **Plan-quality floor — clarify, don't fabricate (holds even when the gate is off).** The plan is
     the highest-leverage artifact; a wrong assumption here multiplies through every downstream task.
     If filling a load-bearing element (a block's Files, Acceptance criteria, scope boundary, a phase
     ordering, or a dependency) would require *inventing* a fact you cannot ground in `$ARGUMENTS`,
     `CLAUDE.md`, `planning/context.md`, the repo, or an existing `master-plan.md` — **stop and ask the
     user a targeted question** instead of writing a plausible-looking guess. The clarify gate governs
     *proactive* question rounds; this floor governs *never fabricating*. Prefer an honest "I need X to
     define block N" over a confident invention.
4. **Determine the Block ID Prefix:** Find this repo's `prefix` in `brain.toml` at the brain root (e.g., `BA`). Use this for all block IDs. If none exists, derive a strict two-letter uppercase prefix (e.g. `BA`). **This prefix must be unique across all projects.**
5. **THINK HARD about phase/block decomposition before writing:**
   - **Sequence by dependency and competence, not calendar.** Foundational, enabling work is Phase 0;
     the hardest, most-differentiating work is the last phase. `/sdlc-block` runs **phases
     sequentially and the blocks within a phase in parallel** by default; add an optional
     `- **Depends on:** <id> (e.g., `BA.0.A`)` line to a block only to override that default (e.g. to serialize
     two same-phase blocks that edit the same file).
   - A **block** is a coherent unit of work that `/generate-tasks` can turn into ~one spec (roughly a
     21-hour spread across a few sessions). Don't make blocks so large they hide separable concerns,
     nor so small they fragment one feature across many.
   - **`/generate-tasks` reads ONLY the target block's section** — not this overview, not sibling
     blocks. So every block must be **self-sufficient**: it must hand the generator the four things it
     is required to produce — concrete **files per task** (for disjoint, merge-safe decomposition),
     **observable acceptance criteria**, correct **scope boundaries**, and (if any) the **shared
     interface surface** the block leans on. Use the per-block skeleton in the Output Format (What /
     Why / Files / Interfaces / Out of scope / Acceptance criteria).
   - **Name each block's files (New vs Modified), by path.** This is load-bearing, not decoration:
     it is how `/generate-tasks` derives disjoint task ownership without guessing. Tasks that would
     share a file must be serialized (`dependsOn`) or restricted to append-only; tasks owning distinct
     files run in parallel. A block that doesn't name its files forces the generator to guess.
   - **Declare each block's Out of scope** — the explicit boundary of what belongs to a *later* block.
     If a block depends on a sibling repo or a not-yet-built project, note that prerequisite here.
   - **Distant (later-phase) blocks may be forward-looking** — author them with the full skeleton
     while the context is fresh, but say so, and expect their Files / interface lines to need
     refinement when each becomes next.
   - Do **not** bake stack/locale/deployment specifics into blocks — those live in `CLAUDE.md` +
     `planning/harness.json`. Keep block definitions about *what*, *why*, *which files*, and *bounds*.
6. Write (or revise) `planning/master-plan.md` using the Output Format below. Preserve the OKF
   frontmatter and any already-completed phases when revising.
7. **Property self-check (before reporting).** Re-read what you wrote and **revise in place** until
   every property holds, then re-check:
   - **Every block is a `### <Prefix>.<PhaseNumber>.<BlockLetter> — <name>` heading under a
     `## Phase N — <name>` heading** — the heading is the bare ID (e.g. `### BA.0.A — <name>`), no
     literal "Block" word — so `/generate-tasks <Prefix>.<PhaseNumber>.<BlockLetter>` can parse and
     locate it. No flat lists for blocks.
   - **Every block names its Files** (New vs Modified, by path), so `/generate-tasks` can derive
     ownership without guessing. A block with no named files is too thin (a forward-looking distant
     block may name them provisionally — but it must say it is provisional).
   - **Every block declares Out of scope** — at least one explicit boundary.
   - **Every block has a non-empty What, Why, and observable Acceptance criteria** that **end with the
     project's gating checks passing**. A block whose Acceptance criteria can't be judged true/false
     against a diff is too vague for `/generate-tasks`.
   - **The Quick Reference Sequence Table lists one row per block** and matches the block headings.
   - **No leftover scaffold sentinels** — no `{{TOKEN}}`, no unfilled `<...>` HTML-comment stubs, no
     empty bullets. (Legitimate `<...>` in code/prose is fine.)
   - **Frontmatter `related:` carries ≥1 real `doc_id`** (not `[]`) so the roadmap is not an isolated
     graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`) — a governing decision, the project `context` doc,
     or a source `/capture` notes doc_id. Use genuine doc_ids only; never invent one. On a *revise*,
     leave an already-populated `related:` intact.
8. Report the path written and the first runnable block (see Report).

## Codebase Structure

- `CLAUDE.md` — the project's actual stack, standing rules, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — the project's validation commands + UI-test config
- `planning/master-plan.md` — the file this command writes (the roadmap)

Read `CLAUDE.md` for the project's real stack and conventions — do not assume any framework,
language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules**. CLAUDE.md is
the authority; assume no stack, locale-parity, narrative, or content-layout rule unless written
there. Universal harness rules apply: no fabricated metrics/quotes, no emoji, every block's
Acceptance criteria leave the project's gated checks (`planning/harness.json` →
`validation.checks[]`) passing. Maintain OKF frontmatter on `master-plan.md`.

## Output Format

```md
---
type: Plan
title: <Project Name> Master Plan
description: Strategic roadmap and phase specifications for <Project Name>.
doc_id: <project>-master-plan
related: [<≥1 real doc_id>]   # required — never leave empty; else this roadmap is an isolated graph node (mev W_GRAPH_ISOLATED_NODE)
---

# <Project Name> — Master Plan

*Living document. Created <DATE>.*

## The Goal, Stated Plainly
<2–3 paragraphs: what the project is, why it matters, and what "ready" means — the competence or
delivery checkpoint that signals project completion.>

## The Destination
<The named product or outcome. If commercial: the buyer, the differentiator, the through-line.>

## Architecture / Design Overview
<The key structural design: layers, an ASCII diagram with per-file/module annotations if useful, and
the load-bearing design decisions (with rationale, pointing to a decisions/ file for the full case).
Keep deployment specifics out — those live in CLAUDE.md + harness.json.>

---

## The Block Contract

`/generate-tasks` reads **only the target block's section** below — not this overview, not sibling
blocks. So every block section must be self-sufficient and hand the generator the four things it is
required to produce: concrete **files per task** (for disjoint, merge-safe decomposition),
**observable acceptance criteria**, correct **scope boundaries**, and the **shared interface surface**
it leans on. Every block uses the same skeleton:

- **What** — the scope, in implementation terms.
- **Why** — the motivation (keeps the generator from over- or under-scoping).
- **SDLC workflow?** — `Yes (patch/task/run/flow)` or `No — <reason>`.
- **Model** — `Sonnet` | `Gemini Pro` | `Gemini Flash` | `Either`. Rule of thumb: Opus = reasoning/breakdown only; Sonnet = high-risk/complex; Gemini Pro = intermediate; Gemini Flash = simple.
- **Workflow & Model Rationale** — prose explaining the choices.
- **Files** — *new* vs *modified*, named by path. Load-bearing: tasks sharing a file must be
  serialized (`dependsOn`) or append-only; tasks owning distinct files run in parallel. A block that
  doesn't name its files forces the generator to guess ownership.
- **Interfaces / shared surface** *(optional — when the project has a shared API/contract layer)* —
  which shared exports/APIs/contracts the block consumes, and any *new* one it must add. Omit for
  projects with no shared layer.
- **Out of scope** — explicit boundaries; what belongs to a later block. Note any cross-repo /
  not-yet-built prerequisite here.
- **Depends on** *(optional)* — `- **Depends on:** <id> (e.g., `BA.0.A`)` (a bare letter `<BlockLetter>`
  means `<Prefix>.<Phase>.<BlockLetter>` of the *same* phase; a fully-qualified `BA.0.A` is also
  accepted). Names sibling blocks that must merge first. Omit it and the default order applies (see
  below); add it only to override that default — e.g. two blocks in the same phase that edit the same
  file must be serialized.
- **Acceptance criteria** — each a true/false condition a reviewer can check against the diff, ending
  with the project's gating checks passing.

**Default ordering — phases sequential, blocks within a phase parallel.** `/sdlc-block` runs each
phase as a wave: all blocks of Phase N run in parallel (each in its own worktree → PR), and Phase N+1
starts only after Phase N's blocks merge. A `Depends on` line refines this by adding an explicit edge,
so a dependent block waves after the block it names (use it to serialize same-phase blocks that share a
file).

Later-phase blocks may be **forward-looking** — authored with the full skeleton while the context is
fresh, but expect to refine their Files / interface lines when each becomes next (say so explicitly in
those blocks).

---

## Phase 0 — <name>

### <Prefix>.<PhaseNumber>.<BlockLetter> — <name>
<!-- Example: ### BA.0.A — Foundation setup (no "Block" word in the heading — the ID is self-describing) -->
- **What:** <scope in implementation terms — concrete enough to scope tasks>
- **Why:** <why this block, why now in the sequence>
- **SDLC workflow?:** <Yes (patch/task/run/flow) or No — reason>
- **Model:** <Sonnet | Gemini Pro | Gemini Flash | Either>
- **Workflow & Model Rationale:** <why this model and workflow were chosen>
- **Files:**
  - *New* <path> (what it holds), …
  - *Modified* <path> (what changes), …
- **Interfaces / shared surface:** <optional — shared exports/APIs this block consumes or must add>
- **Out of scope:** <explicit boundaries; what is a later block's job; any cross-repo prerequisite>
- **Depends on:** <id> (e.g., `BA.0.A`)   *(include only when a sibling in the same phase must merge first; omit this line entirely when the default phase-sequential / block-parallel order suffices)*
- **Acceptance criteria:** <observable, true/false conditions checkable against the diff; end with the
  project's gating checks passing>

### <Prefix>.<PhaseNumber>.B — <name>
<!-- same skeleton -->

---

## Phase 1 — <name>

### <Prefix>.<PhaseNumber>.<BlockLetter> — <name>
<!-- Example: ### BA.1.C — Foundation setup -->
<!-- same skeleton; one sub-section per block -->

---

<!-- ...continue for each phase; the last phase is the hardest, most-differentiating work.
     Distant phases' blocks carry the full skeleton but may be flagged forward-looking. -->

---

## Quick Reference Sequence Table

| Phase | Block | What | Why | SDLC workflow? | Model | Role in destination |
|---|---|---|---|---|---|---|
| 0 | A | <short> | <short> | <short> | <short> | <short> |

---

*Sequenced by dependency and competence, not calendar. When life gets in the way, pick up where you
left off.*
```

### State Registration — register blocks in state.json

After writing/revising `master-plan.md`, register every block (new or changed) in this repo's
`planning/state.json` — the authoritative dependency graph that `master-plan.md`'s prose is a view over
(see `state-schema.md`'s "Authored vs derived" table):

1. Open `planning/state.json`. Find or create a `tracks[]` entry whose `title` matches the phase name
   (one track per phase; reuse an existing track if the phase already has one).
2. For each block in that phase, add an entry to that track's `blocks[]` if it doesn't already exist
   (match by `id`):
   - `id`: the block's canonical ID (e.g. `BA.0.A`)
   - `title`: the block's name
   - `status`: `"open"` — never hand-set `"blocked"` (that is a derived value, see the schema)
   - `sdlc_workflow`: the block's chosen workflow (`none` | `patch` | `task` | `run` | `flow`). (Map 'No' to `none`, and 'Yes (task)' to `task`, etc.)
   - `model`: the block's chosen model (`sonnet` | `gemini-pro` | `gemini-flash` | `either`).
   - `wave`: an integer execution-order rank. Default to `10 * <phase number>` (Phase 0 → `10`, Phase 1
     → `20`, …) so every block in a phase shares a wave and later phases sort after.
   - `depends_on`: one `{ "type": "block", "repo": "<this-repo-slug>", "id": "<ID>" }` entry per explicit
     **Depends on:** line on the block (resolve a bare letter `X` to `<Prefix>.<PhaseNumber>.X`). Omit or
     use `[]` when the block has no explicit "Depends on" line — do **not** encode the implicit
     phase-sequential default as a `depends_on` edge; `wave` already expresses that ordering.
   - **Cross-repo-edge prompt.** A "Depends on" line only ever names a same-repo sibling block, so a
     dependency on *another repo's* block never surfaces on its own — ask explicitly: "Does this block
     depend on work landing in another repo first?" If yes, resolve that repo's `slug` from `brain.toml`
     and add `{ "type": "block", "repo": "<other-repo-slug>", "id": "<their-ID>" }` to this block's
     `depends_on` (in addition to any same-repo edges); if the dependency is non-block (hardware, a
     paid-API budget, a manual step), use `{ "type": "external", "what": "<gloss>" }` instead. Skip the
     question only when the plan explicitly says the phase is fully self-contained.
   - If the block was promoted from an HQ backlog item, add `"origin": { "type": "backlog", "slug": "<slug>" }`.
   - **Do not overwrite** an existing block's `status` or `tasks` (the derived pointer + status
     summary — see `docs/state/state-schema.md`) if it is already `in_progress` / `closed` /
     already has a `tasks.json` — only add missing blocks, or update `title` / `wave` / `depends_on`
     on ones still `open`. Never hand-author a `tasks` value yourself; it's regenerated by
     `mev emit-state --write` from each block's `tasks.json`, if one exists.
3. Save `planning/state.json` and validate it is still valid JSON:
   `python3 -c "import json;json.load(open('planning/state.json'))"`.

### State Refresh

Run `mev emit-state --write` to update the brain's focus derivation and state based on the new planning files.

## Report

Output the path written and the next step:
```
planning/master-plan.md  (<N> phases, <M> blocks)

Blocks ready to generate:
  - BA.0.A — <name>
  - BA.0.B — <name>
  ...

Next (turn the first block into a runnable spec):
  /generate-tasks BA.0.A
```
