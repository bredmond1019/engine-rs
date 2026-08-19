# Plan — Author an initiative: its narrative and its block records.

## Variables

$ARGUMENTS — free-text description of the feature, experiment, or body of work to plan.
             Optional flags: `--founding`, `--clarify`.

| Flag | What it does |
|---|---|
| `--founding` | This is the project's founding roadmap — a new repo's first blocks. Adds the Goal / Destination / Architecture framing and writes to `planning/founding/`. Invoked by `/new-project`. |
| `--clarify` | Force the clarify gate on regardless of `planning/harness.json`. |
| `--no-redteam` | Skip the adversarial pass (step 10). For a small, low-risk initiative only. |

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
>
> **Upstream.** For work on an existing system large enough that the cut is not obvious, the
> pre-plan pipeline runs first: `/assess` → `/seams` → `/sequence`. Its output,
> `planning/<slug>/sequence.md`, is this command's authored input (step 3a). Planning a
> substantial change to an existing subsystem without a seam map produces a cut along
> architectural layers, which is the failure mode where nothing is usable until the end.

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

3a. **Read the pre-plan artifacts if they exist.** Check `planning/<slug>/` for `sequence.md`,
   `seams.md`, `assessment.md` (and `verification.md`).

   - **`sequence.md` is this command's authored input.** Its cut, its wave boundaries, its
     `ships` lines, its `depends_on` edges, its named files and its recorded fork answers are
     carried through — not re-derived. Re-deriving them silently discards a pass that already
     resolved the operator's decisions.
   - **Where `assessment.md` and `verification.md` disagree, verification wins** — it is later and
     was checked against source. Never carry a claim the verification pass marked REFUTED into a
     block record.
   - Line numbers in those documents move. **Grep the symbol, not the number**, and re-check any
     absolute count before it goes in a block.
   - If you depart from `sequence.md`'s cut, say so explicitly in the Sequencing Rationale and
     give the reason. A silent departure means the seam analysis was done and then ignored.

3b. **When there is no pre-plan folder — the floor.** Most initiatives do not need `/assess` →
   `/seams` → `/sequence`, and this command must stay usable for a twenty-minute planning session.
   But four questions cause almost all unbuildable plans, and they are cheap to answer inline.
   Answer them **in proportion to the work** — a sentence each on a two-block initiative, a
   paragraph each on a six-block one — and record the answers in the plan:

   1. **Ground truth.** Have the repo's gated checks been run, and does the thing this builds on
      actually run today? Do not plan around a theory of why something fails when running it once
      settles it.
   2. **Built, half-built, or absent?** For every capability this initiative *calls* rather than
      builds: does it have a production call site, or does it merely exist in source — behind a
      disabled flag, with only test callers, or never executed? Half-built is where plans die,
      because it reads as reuse and costs a rewrite. Name a call site, or say there isn't one.
   3. **What breaks if this attaches wrong?** One line of blast radius per new attachment point,
      grounded in real consumers found by grep, not hypothetical ones.
   4. **What already exists that this would duplicate, and what should be deleted first?**

   **Escalation trigger — this is the point of the floor.** If you cannot answer question 2 from
   what you have read, or question 3 comes back "unknown" for a load-bearing attachment, **stop and
   recommend `/assess <topic>`** rather than planning on top of the gap. Say which question you
   could not answer. A plan authored over an unanswered half-built question is not detectable
   later: it surfaces as a block that was sized as wiring and turns out to be a rewrite.

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
   - **Every block must ship something usable on its own.** Test each candidate: *what can the
     operator do the day this merges that they could not do the day before?* If the answer is
     "nothing yet," the block is mis-cut — merge it into the block that consumes it, or re-cut it
     so it lands with a visible surface however small. Six blocks each delivering something beats
     six blocks delivering only at the end, and this outranks the tidier architectural cut.
   - **A block that makes later work observable outranks a block that adds capability.** If the
     system cannot stop, report on, or verify its own work, every later block's failures are
     invisible.
   - **Deletions come before the extensions that would inherit them.** If dead or superseded
     surface is in the way, cut a block that removes it first — it usually shrinks everything
     after it.
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
   - **The pre-plan floor was answered** — carried from `sequence.md` (3a) or answered inline (3b).
     In particular no capability this initiative *calls* is left unclassified as built / half-built
     / absent.
   - **Frontmatter `related:` carries ≥1 real `doc_id`** (not `[]`), else this plan is an isolated
     graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`). Use genuine doc_ids only; never invent one. On
     a revise, leave an already-populated `related:` intact.
   - **No `master-plan.md` was authored or edited.** It is generated from the block graph.

   Three further properties, all **can-fail** — a plan passing the structural checks above can
   still be unbuildable:

   - **Ships alone.** Every block record's `why` answers *what the operator can do the day this
     lands*. A `why` that reads "enables <later block>" fails: merge it into the block that
     consumes it, or re-cut it so it lands with a visible surface.
   - **Every block names the gate that proves it**, drawn from `planning/harness.json` →
     `validation.checks[]`, and — per base-template D68 — how that gate is shown capable of
     **failing** on this deliverable. A gate that was never observed going red is not evidence.
     Where a criterion's evidence lives outside the repo, the language, or the source tree (an
     installed binary, a sibling repo, a generated artifact), declare it and name the fixture
     that stands in for the missing gate.
   - **Handoff test on the first runnable block.** Could a fresh agent, given only that block
     record plus `CLAUDE.md`, start work without asking a question? If not, the record has a hole
     exactly where the question would be — fill it and re-check. Report the result.

10. **Adversarial pass on the sequencing (skip only with `--no-redteam`).** Step 9 is self-review
   by the context that wrote the plan, which is the weakest verification available. Spawn 2–3
   **fresh** agents (Opus), given only `plan.md` and the first wave's block records, each with one
   brief:
   - *"Which block, if it lands exactly as written, delivers nothing the operator can use? Name it
     and say why."*
   - *"Give me three concrete ways this ordering fails at the third block."*
   - *"What concern has no block at all?"* — migrations, flags, observability, error paths, auth
     boundary, performance under real data, concurrency, the install/deploy boundary, rollback.

   Fold in what survives and revise the records. Record what you rejected and why in the Cut list
   — a rejected attack is a decision with a date on it, and otherwise it gets re-raised.

11. Report the paths and the first runnable block.

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

Pre-plan input:   sequence.md <used | absent> <, departures: ...>
Handoff test on <first block ID>: PASS | FAIL — <what was missing>
Red team: <x> attacks landed, <y> rejected

Next (turn the first block into a runnable spec):
  /generate-tasks <ID>
```
