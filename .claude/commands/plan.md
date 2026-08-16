# Plan — Author an initiative: its narrative and its block records.

## Variables

$ARGUMENTS — free-text description of the feature, experiment, or body of work to plan.
             Optional flags: `--founding`, `--clarify`.

| Flag | What it does |
|---|---|
| `--founding` | This is the project's founding roadmap — a new repo's first blocks. Adds the Goal / Destination / Architecture framing and writes to `planning/founding/`. Invoked by `/new-project`. |
| `--clarify` | Force the clarify gate on regardless of `planning/harness.json`. |

## Purpose

Author **one initiative**: a coherent body of work in **one repo**, spanning one or more blocks.
Output is two things:

- `planning/<slug>/plan.md` — the authored **narrative**: the goal, the sequencing rationale, the
  architecture framing, the cut list. Everything true of the *set* rather than of any one block.
- `planning/blocks/<BlockID>.json` — one **block record** per block. The definition of each
  member.

`/plan` does **not** write `tasks.json`. Task decomposition is deferred to
`/generate-tasks <BlockID>`, run later, per block — because a later block's tasks depend on an
earlier block's code, and decomposing everything at authoring time burns tokens on work that gets
re-derived anyway (D65).

> **Scope ladder.** Multi-repo program → `/generate-roadmap`. One repo, multiple blocks → here.
> A single block → `/ticket` (behavior change) or `/chore` (maintenance).
> `master-plan.md` is **generated** from the block graph, not authored — never hand-write one.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the work.

2. **Clarify gate.** Read `planning/harness.json` → `planning.clarify`. When it is `true`, when
   `$ARGUMENTS` contains `--clarify`, when `--founding` is set, or when the goal / scope /
   destination is genuinely underspecified, pause and ask the user **2–4 targeted clarifying
   questions** (the destination, the checkpoint that signals "done", the load-bearing
   architectural choices, the rough phase boundaries) before writing. Fold the answers in. When
   the gate is off and the input is already rich, proceed immediately. Strip the flags before
   using `$ARGUMENTS` as prose.

   - **Plan-quality floor — clarify, don't fabricate (holds even when the gate is off).** The plan
     is the highest-leverage artifact; a wrong assumption here multiplies through every downstream
     task. If filling a load-bearing element (a block's files, acceptance criteria, scope boundary,
     phase ordering, dependency, or **why**) would require *inventing* a fact you cannot ground in
     `$ARGUMENTS`, `CLAUDE.md`, `planning/context.md`, the repo, or an existing plan — **stop and
     ask the user a targeted question** instead of writing a plausible-looking guess. The clarify
     gate governs *proactive* question rounds; this floor governs *never fabricating*.

3. Read `CLAUDE.md` and `planning/context.md` — internalize the standing rules and current
   architecture. Read the files the blocks will touch. When revising an existing initiative, read
   its `plan.md` and every block record it already owns; preserve completed work.

4. **Allocate the phase numbers from `state.json`, not from any narrative file.** Read this repo's
   `planning/state.json`, take the highest existing phase for this repo, and number upward.
   Narrative files lag, and that lag is how one repo came to carry two unrelated "Phase 4"s.

5. **THINK HARD about decomposition before writing:**
   - An initiative is typically 1–3 phases and 1–6 blocks. Much larger and it is a multi-repo
     program — use `/generate-roadmap`.
   - A **block** is a coherent, independently reviewable unit that `/generate-tasks` can turn into
     ~one spec. Not so large it hides separable concerns, not so small it fragments one feature.
   - **Sequence by dependency and competence, not calendar.** Foundational, enabling work first;
     the hardest, most-differentiating work last.
   - **`/generate-tasks` reads only the target block's record** — not this narrative, not sibling
     blocks. Every block record must therefore be self-sufficient: concrete **files** (new vs
     modified, by path), **observable acceptance criteria**, an explicit **out of scope**, and any
     shared **interfaces**.
   - **Name files by path.** This is load-bearing, not decoration: it is how `/generate-tasks`
     derives disjoint task ownership without guessing.
   - **Distant blocks may be forward-looking** — author the full record while context is fresh,
     but set `forward_looking: true` and expect to refine files and interfaces when each becomes
     next.
   - Do **not** bake stack, locale, or deployment specifics into blocks — those live in
     `CLAUDE.md` + `planning/harness.json`. Blocks are about *what*, *why*, *which files*, and
     *bounds*.

6. Choose a short descriptive slug (e.g. `keyboard-nav`, `auth-refresh`). With `--founding` the
   slug is `founding`. Create `planning/<slug>/` if absent.

7. **Write each block record and register it.** For every block, read and follow
   `.claude/workflows/block-registration.md` — the canonical procedure for the block ID, the
   operator and cross-repo edge questions, the carryover read, the block record, and `state.json`
   registration. Do not restate it here or invent a variant. Set `kind` to `block` and
   `initiative` to `<slug>`.

8. **Write the narrative** to `planning/<slug>/plan.md` using the Output Format below. The
   narrative holds only what is true of the set — it must not duplicate a block's what/why/files,
   which live in the block records and would immediately drift.

9. **Property self-check.** Re-read what you wrote and **revise in place** until every property
   holds, then re-check:
   - **Every block heading is the full canonical ID** — `### MV.3.A — <name>`, never
     `### Block A`. The ID is self-describing; there is no literal "Block" word.
   - **Every block has a record** at `planning/blocks/<BlockID>.json` that validates against
     `.claude/workflows/block.schema.json`, with `why`, `description`, and `out_of_scope`
     non-empty and files named by path.
   - **Every block is registered in `state.json`** with a matching `id`, `wave`, and `depends_on`.
   - **The Sequence Table lists one row per block** and matches both the headings and the records.
   - **No leftover scaffold sentinels** — no `{{TOKEN}}`, no unfilled `<...>` stubs, no empty
     bullets. Legitimate `<...>` in code or prose is fine.
   - **Frontmatter `related:` carries ≥1 real `doc_id`** (not `[]`), else this plan is an isolated
     graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`). Use genuine doc_ids only; never invent one. On
     a revise, leave an already-populated `related:` intact.
   - **No `master-plan.md` was authored or edited.** It is generated from the block graph.

10. Report the paths and the first runnable block.

## Codebase Structure

- `CLAUDE.md` — standing rules, stack, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — validation commands + UI-test config
- `planning/blocks/` — block records; `planning/<slug>/` — initiative narratives
- `.claude/workflows/block.schema.json` — the block record field contract
- `.claude/workflows/block-registration.md` — the shared registration procedure

Read `CLAUDE.md` for the project's actual stack and conventions — do not assume any framework,
language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules**. CLAUDE.md
is the authority; assume no stack, locale-parity, narrative, or content-layout rule unless written
there. Universal harness rules apply: no fabricated metrics or quotes, no emoji, every block's
acceptance criteria leave the project's gated checks (`planning/harness.json` →
`validation.checks[]`) passing.

## Output Format

~~~md
---
type: Plan
title: <Initiative Name>
description: <One line: what becomes true when this initiative is done.>
doc_id: plan-<slug>
layer: [<inferred layer>]
project: <repo slug>
status: active
keywords: [<3-5 terms>]
related: [<≥1 real doc_id>]   # required — never empty; else this is an isolated graph node
---

# <Initiative Name>

*Created <DATE>. Block definitions live in `planning/blocks/`; this document holds only what is
true of the set.*

## The Goal, Stated Plainly
<1–3 paragraphs: what this is, why it matters now, and what "done" means — the checkpoint that
signals completion. With --founding, this is the project's arc, not one feature's.>

## The Destination
<The named outcome: what is true when this is fully executed. If commercial: the buyer, the
differentiator, the through-line.>

## Architecture / Design Overview
<Key structural decisions; which existing systems this hooks into; new abstractions needed. An
ASCII diagram if it earns its place. Keep deployment specifics out — those live in CLAUDE.md +
harness.json.>

## Sequencing Rationale
<Why the phases fall where they do. What is foundational, what is hardest and therefore last, and
which orderings are forced by dependency rather than preference.>

---

## Phase <N> — <name>

### <Prefix>.<Phase>.<Block> — <name>
<!-- Example: ### MV.3.A — Block record validation -->
<!-- One short paragraph ONLY: this block's role in the initiative. Its what / why / files /
     out-of-scope / acceptance criteria live in planning/blocks/<ID>.json — do not restate them
     here, or the two copies drift within a week. -->

### <Prefix>.<Phase>.<Block> — <name>
<!-- same shape -->

---

## What Is Cut, and Why

| Candidate | Why it is out |
|---|---|
| <thing considered and excluded> | <the reason> |

<An unstated cut reads as an oversight, gets re-proposed next time, and is re-litigated. A stated
cut is a decision with a date on it. Make this list longer than is comfortable.>

---

## Sequence

| Phase | Block | What | SDLC workflow | Model | Role in the destination |
|---|---|---|---|---|---|
| <N> | <ID> | <short> | <short> | <short> | <short> |

---

*Sequenced by dependency and competence, not calendar.*
~~~

## Report

```
planning/<slug>/plan.md            (<N> phases, <M> blocks)
planning/blocks/                   <M> block records written
state.json: <created | already existed>, <M> blocks registered

Blocks ready to decompose:
  - <ID> — <name>
  ...

Next (turn the first block into a runnable spec):
  /generate-tasks <ID>
```
