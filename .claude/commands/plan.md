# Plan — Author a mini-roadmap for an ad-hoc or experimental feature.

## Variables

$ARGUMENTS — free-text description of the feature, experiment, or bounded task to plan.

## Purpose

Turn a free-form description into a `planning/plan-<slug>/plan.md` whose phase/block structure
`/generate-tasks` can consume directly. This is the **mini-roadmap** producer — for ad-hoc or
experimental work you want to plan and run **without** putting it in the main `master-plan.md`
first. The output uses the same master-plan format (phases/blocks/Quick Reference table), so
`/sdlc-block` can orchestrate it as a branch train, or `/generate-tasks <block>` → `/sdlc-flow`
can run a single block.

> For the main project roadmap, use `/generate-master-plan` → `planning/master-plan.md`.
> See `planning/decisions/D34-adhoc-planning-seam.md`.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the feature or task.
2. **Clarify gate (only when enabled).** Read `planning/harness.json` → `planning.clarify`. When it is
   `true` **or** `$ARGUMENTS` contains `--clarify`, and the prompt is genuinely ambiguous (its scope,
   target files, or success criteria could be read more than one way), pause and ask the user **2–4
   targeted clarifying questions** before writing anything; fold the answers into the plan. If the
   prompt is already unambiguous, skip the questions and proceed even when the gate is on. When
   `planning.clarify` is absent/`false` and no `--clarify` flag is present, skip this step entirely.
   Strip a `--clarify` token from the prompt before using it as the title.
   - **Plan-quality floor — clarify, don't fabricate (holds even when the gate is off).** If filling a
     load-bearing element (a block's Files, Acceptance criteria, scope boundary, or a dependency) would
     require *inventing* a fact you cannot ground in `$ARGUMENTS`, `CLAUDE.md`, `planning/context.md`,
     or the repo — **stop and ask the user a targeted question** rather than write a plausible-looking
     guess. An honest "I need X to define block N" beats a confident invention.
3. Read `CLAUDE.md` and `planning/context.md` — internalize the standing rules and current architecture.
4. **Determine the Block ID Prefix:** find this repo's `prefix` in `brain.toml` at the brain root
   (e.g. `BA`). Record it per-block as a `**Block ID:** <Prefix>.<PhaseNumber>.<BlockLetter>`
   bullet in the block body (Output Format below) — the `### Block <Letter>` **heading itself stays
   a single uppercase letter, unprefixed.** `/sdlc-block` parses `### Block X` expecting `X` to be
   exactly one letter (`.claude/workflows/sdlc-block.js`); embedding the full prefixed ID in the
   heading text breaks that parse.
5. Read any files directly relevant to the task (the files the blocks will touch).
6. **THINK HARD about scope and decomposition before writing:**
   - A mini-roadmap is typically 1–3 phases and 1–5 blocks. If it grows larger than that, it belongs
     in `master-plan.md` via `/generate-master-plan`, not here.
   - A **block** is a coherent, independently reviewable unit of work that `/generate-tasks` can turn
     into ~one spec. Don't make blocks so large they hide separable concerns, nor so small they fragment
     one feature across many.
   - `/generate-tasks` reads **only the target block's section** — so every block must be
     **self-sufficient**: concrete **Files** (New vs Modified, by path), **observable Acceptance
     criteria**, explicit **Out of scope** boundaries, and any shared **Interfaces**.
   - **Name files by path.** Tasks sharing a file must be serialized (`dependsOn`) or append-only;
     tasks owning distinct files may run in parallel.
   - Sequence blocks by dependency and competence — foundational / enabling work first.
7. Choose a short descriptive slug (e.g. `keyboard-nav`, `auth-refresh`, `rust-streaming`).
8. Create `planning/plan-<slug>/` if it does not exist, then write the plan to
   `planning/plan-<slug>/plan.md` using the Output Format below.
9. **Property self-check.** Before reporting, re-read the plan and **revise in place** until every
   property holds, then re-check:
   - Every block is a `### Block X — <name>` heading (single uppercase letter, no prefix) under a
     `## Phase N — <name>` heading, so `/generate-tasks phaseN-blockX` and `/sdlc-block` can both
     parse and locate it. The full `<Prefix>.<PhaseNumber>.<BlockLetter>` id lives in a
     `**Block ID:**` bullet in the block body, not the heading.
   - Every block names its **Files** (New vs Modified, by path).
   - Every block declares **Out of scope** — at least one explicit boundary.
   - Every block has non-empty **What**, **Why**, and observable **Acceptance criteria** — each a
     true/false condition checkable against the diff, ending with the project's gating checks passing.
   - The **Quick Reference Sequence Table** has one row per block and matches the block headings.
   - No leftover scaffold sentinels (`{{TOKEN}}`, unfilled `<placeholder>`-style angle stubs, empty
     bullets). Legitimate `<...>` in code/prose is fine.
   - **Frontmatter `related:` carries ≥1 real `doc_id`** (not `[]`) so this plan is not an isolated
     graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`) — the source `/capture` notes doc_id if this plan
     was promoted from one, else the project `master-plan` or a governing decision. Use genuine
     doc_ids only; never invent one.
10. Report the path and next steps.

## Codebase Structure

- `CLAUDE.md` — standing rules, stack, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — validation commands + UI-test config
- `planning/master-plan.md` — the main project roadmap (this command does NOT modify it)

Read `CLAUDE.md` for the project's actual stack and conventions — do not assume any framework,
language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules**. CLAUDE.md
is the authority; assume no stack, locale-parity, narrative, or content-layout rule unless written
there. Universal harness rules apply: no fabricated metrics/quotes, no emoji, every block's
Acceptance criteria leave the project's gating checks (`planning/harness.json` →
`validation.checks[]`) passing.

## Output Format

The plan uses the same block-definition skeleton as `master-plan.md` (so `/sdlc-block` and
`/generate-tasks` can consume it directly):

~~~md
---
type: Plan
title: <Feature Name> Plan
description: Mini-roadmap for <feature name> — ad-hoc, not in master-plan.md.
doc_id: plan-<slug>
related: [<≥1 real doc_id>]   # required — never leave empty; else this plan is an isolated graph node (mev W_GRAPH_ISOLATED_NODE)
---

# <Feature Name> — Plan

*Mini-roadmap. Created <DATE>. Ad-hoc: not in `planning/master-plan.md`.
See `planning/decisions/D34-adhoc-planning-seam.md`.*

## The Goal, Stated Plainly
<1–2 paragraphs: what this feature is, why it matters now, and what "done" means.>

## The Destination
<The named outcome: what is true when this plan is fully executed.>

## Architecture / Design Overview
<Key structural decisions; which existing systems it hooks into; any new abstractions needed.>

---

## The Block Contract

`/generate-tasks` reads **only the target block's section** below. Every block is self-sufficient
and uses the same skeleton:

- **What** — the scope, in implementation terms.
- **Why** — the motivation (keeps the generator from over- or under-scoping).
- **Files** — *new* vs *modified*, named by path. Load-bearing: tasks sharing a file must be
  serialized (`dependsOn`) or append-only; tasks owning distinct files may run in parallel.
- **Interfaces / shared surface** *(optional)* — shared exports/APIs consumed or added. Omit when
  there is no shared layer.
- **Out of scope** — explicit boundaries; what belongs to a later block or a different effort.
- **Acceptance criteria** — true/false conditions checkable against the diff, ending with the
  project's gating checks passing.

---

## Phase 0 — <name>

### Block <Letter> — <name>
- **Block ID:** <Prefix>.<PhaseNumber>.<BlockLetter> — the canonical id used in `state.json` /
  `depends_on` / `/generate-tasks --from`. The heading above stays a bare letter; `/sdlc-block`
  parses it as one.
- **What:** <scope in implementation terms>
- **Why:** <why this block, why now in the sequence>
- **Files:**
  - *New* `<path>` — <what it holds>
  - *Modified* `<path>` — <what changes>
- **Interfaces / shared surface:** <optional>
- **Out of scope:** <explicit boundaries>
- **Acceptance criteria:**
  - <observable true/false condition>
  - Project's gating checks pass (see `planning/harness.json`).

---

## Quick Reference Sequence Table

| Phase | Block | What | Why | Role in destination |
|---|---|---|---|---|
| 0 | A | <short> | <short> | <short> |

---

*Ad-hoc mini-roadmap — run one block or the full train (see Report below).*
~~~


### Step X — Register the block(s) in state.json
After writing the `plan.md` file, you MUST also register its blocks in `planning/state.json` — this
plan is ad-hoc (not in `master-plan.md`), so its blocks don't exist there yet. **No `tasks.json`
exists for any of these blocks yet** — `/plan` only defines blocks; `/generate-tasks --from` (run
separately, per block, later) is what produces a block's `tasks.json`. Do not add a `tasks` field
to any block registered here.
1. Open `planning/state.json`. Find or create a `tracks[]` entry titled `"plan-<slug>"` (or reuse an
   existing ad-hoc-plans track if the convention already exists in this repo).
2. For each block in `plan-<slug>/plan.md`, add an entry to that track's `blocks[]` if it doesn't
   already exist (match by `id`):
   - `id`: the block's canonical ID (the `**Block ID:**` bullet value, e.g. `BA.0.A` — full
     prefixed form; the plan.md *heading* stays the bare letter)
   - `title`: the block's name
   - `status`: `"open"`
   - `wave`: an integer rank placing it after this repo's current highest wave (so ad-hoc plans queue
     behind committed roadmap work) — default to `10 * (floor(highest existing wave / 10) + 1)`, keeping
     blocks in the same phase on the same wave.
   - `depends_on`: one `{ "type": "block", "repo": "<this-repo-slug>", "id": "<ID>" }` entry per explicit
     "Depends on" line in the block; `[]` if none.
   - **Cross-repo-edge prompt.** A "Depends on" line only ever names a same-repo sibling block, so a
     dependency on *another repo's* block never surfaces on its own — ask explicitly: "Does this block
     depend on work landing in another repo first?" If yes, resolve that repo's `slug` from `brain.toml`
     and add `{ "type": "block", "repo": "<other-repo-slug>", "id": "<their-ID>" }` to this block's
     `depends_on` (in addition to any same-repo edges); if the dependency is non-block (hardware, a
     paid-API budget, a manual step), use `{ "type": "external", "what": "<gloss>" }` instead.
   - `origin`: omit unless this plan was promoted from an HQ backlog item, in which case
     `{ "type": "backlog", "slug": "<backlog-slug>" }`.
3. Save `planning/state.json` and validate it is still valid JSON:
   `python3 -c "import json;json.load(open('planning/state.json'))"`.

### State Refresh

Run `mev emit-state --write` to update the brain's focus derivation and state based on the new planning files.

## Report

Output the path written and the next steps:

```
planning/plan-<slug>/plan.md  (<N> phases, <M> blocks)

Blocks ready to generate:
  - BA.0.A — <name>   (block heading: ### Block A)
  ...

Next (single block — decompose and run):
  /generate-tasks --from planning/plan-<slug>/plan.md phase0-blockA
  /sdlc-flow plan-<slug>

Next (all blocks as a branch train):
  /sdlc-block planning/plan-<slug>/plan.md
```
