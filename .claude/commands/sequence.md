# Sequence — Cut a verified seam map into an ordered set of blocks each of which ships something.

Stage 3 of the pre-plan pipeline: `/assess` → `/seams` → `/sequence` → `/plan`.
Method: `docs/how-to-plan-with-agents.md` in the brain repo.

## Variables

$ARGUMENTS — the slug, plus optional flags. Example: `orchestration-extensions`

| Flag | What it does |
|---|---|
| `--single-repo` | The work lands in one repo; skip the ownership split and hand off to `/plan` |
| `--no-redteam` | Skip the adversarial pass. Do not use before authoring a real plan |

## Purpose

Turn `seams.md` into **the cut**: an ordered set of candidate blocks, each with an owning repo,
each shipping something the operator can use on its own, sequenced by dependency and by what
makes later work verifiable.

Output: `planning/<slug>/sequence.md`. It is the **only** input `/plan` (one repo) or
`/generate-roadmap` (multiple repos) needs.

This stage does not author block records or register `state.json`. That is `/plan`.

## Instructions

1. Read `planning/<slug>/seams.md`, `assessment.md`, `verification.md`, `CLAUDE.md`, and the
   in-scope repos' `planning/context.md`. If `seams.md` is missing, stop and point at `/seams` —
   sequencing from an assessment alone produces a cut along architectural layers, which is the
   failure mode this whole pipeline exists to avoid.

2. **Resolve the forks first.** Every fork in `seams.md` must have an answer before cutting —
   the cut depends on them. Present them to the user with your recommendations and get a decision.
   Record each answer and its date in `sequence.md`. **This is a blocking gate**: an unresolved
   fork silently becomes a decision made by whichever agent hits it first.

3. **Cut by deliverable, not by layer.** The governing constraint:

   > Every block ships something the operator can use the day it lands.

   This deliberately rules out the natural engineering cut (all the plumbing, then all the value).
   Test every candidate block against it: *"what can the operator do on the day this merges that
   they could not do the day before?"* If the answer is "nothing yet," the block is mis-cut —
   re-cut it so it lands with a visible surface, however small (a command that prints a plan, one
   gate that actually fires, a report you can read in the morning).

   A block that genuinely cannot ship value alone must be **merged into the one that consumes it**,
   not left as a standalone.

4. **Sequence by dependency and by verifiability, not by calendar.**
   - Foundational and enabling work first; hardest and most differentiating last.
   - **A block that makes later work observable outranks a block that adds capability.** If the
     system cannot stop, report, or close out its own work, every later block's failures are
     invisible — which is the one failure mode that is unaffordable.
   - Prefer the cheapest evidence first. A smoke run of a never-executed path outranks
     implementing a fix for the theory of why it fails.
   - Half-built capabilities from `seams.md` are usually the highest value per line of code in the
     whole cut. Rank them accordingly.
   - Deletions go **before** the extensions that would otherwise inherit them.

5. **Size each block.** A block is one coherent, independently reviewable unit that
   `/generate-tasks` can turn into roughly one spec. Record for each:
   - **owning repo** — and whether it is code, a config/docs change, or an **operator errand**
     (a human action: a credential, a machine visit, a decision). Errands are first-class; they
     block chains and they are invisible if unlisted.
   - **ships** — the one-line answer to "what can the operator do now"
   - **depends_on** — real edges, including cross-repo and operator edges
   - **files it will touch, by path** — provisional is fine, but named. `/plan` and
     `/generate-tasks` derive task ownership from these
   - **the gate that proves it** — and per base-template D68, how that gate is shown capable of
     failing
   - **the risk that would sink it**

6. **Split by repo and name the contract.** For every cross-repo edge, say which repo authors the
   contract and which re-pins it, and whether a data-contract or workspace-contract version bump is
   required. A cross-repo edge with no named author is the most common source of two half-built
   sides that never meet.

7. **State the cut list.** What was considered and excluded, with the reason. Make this longer than
   is comfortable — an unstated cut reads as an oversight, gets re-proposed, and is re-litigated.

8. **Red-team the sequence** (unless `--no-redteam`). 2–3 fresh Opus agents, given only
   `sequence.md`, each with one brief:
   - *"Which block, if it lands as written, delivers nothing usable? Name it and say why."*
   - *"Give me three concrete ways this ordering fails at block 4."*
   - *"What is missing entirely — a concern with no block?"*
   Fold in what survives; record what you rejected and why.

9. **Handoff test.** Take the first block's row and ask: could a fresh agent given only this row
   plus its repo's `CLAUDE.md` start work without asking a question? If not, the row has a hole
   exactly there — fill it. Report the result.

10. **Property self-check.** Revise in place until all hold:
    - Every fork from `seams.md` has a recorded answer.
    - Every block has an owning repo, a `ships` line, real `depends_on`, named files, and a gate.
    - No block's `ships` line reads as "enables a later block."
    - Every cross-repo edge names the contract author.
    - Operator errands are listed as blocks, not as prose asides.
    - The cut list is non-empty.
    - Absolute numbers carried from the assessment are re-derived or marked stale.
    - Frontmatter `related:` carries ≥1 real `doc_id`.

11. Commit with an explicit pathspec. Report the cut and the next command:
    `/plan` for one repo, `/generate-roadmap` for several.

## Output Format

~~~md
---
type: Reference
title: "<Topic> — proposed sequence"
description: <One line: the proposed cut into blocks, what each ships, and the rule that ordered them.>
doc_id: sequence-<slug>
layer: [<layer>]
project: <repo slug or omit if cross-cutting>
status: active
keywords: [sequencing, blocks, waves, <3-5 terms>]
related: [seams-<slug>, assessment-<slug>]
---

# <Topic> — proposed sequence

*Cut <DATE> from `seams.md`. Candidate blocks only — `/plan` authors the records.*

## Decisions taken

| Fork | Answer | Date | Why |
|---|---|---|---|

## The sequencing rule
<In two or three sentences: why this order and not the architectural one. What is foundational,
what is last, which orderings are forced by dependency rather than preference.>

## The cut

### Wave <N> — <what becomes true> · <k> blocks · <repos>

| # | Block | Repo | Ships (what the operator can do the day it lands) | depends_on | Files | Gate | Risk |
|---|---|---|---|---|---|---|---|

*Wave exit: <the observable condition that says this wave is done.>*

## Cross-repo contracts

| Edge | Author | Re-pins | Contract bump needed |
|---|---|---|---|

## Operator errands

| # | Errand | Blocks | Why a human |
|---|---|---|---|

## What is cut, and why

| Candidate | Why it is out |
|---|---|

## Red team — what survived

| Attack | Verdict | What changed |
|---|---|---|

## Handoff test
<Result of running the first block's row past the "could a fresh agent start?" test.>

## Totals

| Repo | Blocks | What it owns |
|---|---|---|
~~~

## Report

```
planning/<slug>/sequence.md

Waves: <n>   Blocks: <m>   Operator errands: <e>
Forks resolved: <k>
Blocks re-cut for failing the ships-alone test: <list>
Red team: <x> landed, <y> rejected
Handoff test on block 1: PASS | FAIL — <what was missing>

Next:  /plan "<the initiative>"        (one repo)
       /generate-roadmap <slug>        (several repos)
```
