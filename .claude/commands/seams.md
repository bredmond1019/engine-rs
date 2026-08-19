# Seams — Map where new work attaches to the existing system.

Stage 2 of the pre-plan pipeline: `/assess` → `/seams` → `/sequence` → `/plan`.
Method: `docs/how-to-plan-with-agents.md` in the brain repo.

## Variables

$ARGUMENTS — the assessment slug, plus optional flags. Example: `orchestration-extensions`

| Flag | What it does |
|---|---|
| `--spike <n>` | Run up to `n` spikes instead of the default 1. `--spike 0` disables |
| `--from <path>` | Read an assessment that is not at `planning/<slug>/assessment.md` |

## Purpose

The assessment says what is true. Sequencing says what to build. **This stage answers the
question in between: where does the new work attach to the old, and what breaks if that
attachment is wrong.** It is the stage most often skipped and the one whose absence most reliably
produces a plan that is coherent on paper and unbuildable in practice.

Output: `planning/<slug>/seams.md`.

It produces **no blocks, no waves, no estimates.** Those are `/sequence`.

## Instructions

1. Resolve the slug. Read `planning/<slug>/assessment.md`, `verification.md` if present, and
   `CLAUDE.md`. **Where assessment and verification disagree, verification wins.** If no
   assessment exists, stop and point at `/assess`.

2. **Classify every capability the work depends on into exactly one of three states.** This is the
   core of the command; do not soften the categories.

   | State | Definition | The planning consequence |
   |---|---|---|
   | **Built** | Exists, is called in production, and has been observed working | Call it. Do not rebuild it |
   | **Half-built** | Exists in source but has no production caller, is behind a disabled flag, is untested, or was never actually run | **This is where plans die.** Each one is either a small wiring block or a large rewrite, and you cannot tell which without looking |
   | **Absent** | Does not exist | Honest new work |

   For half-built, additionally record: *what specifically is missing* (a caller, a flag, a
   config value, a schema agreement), and *how you know* — with file + symbol. "Shipped and
   unused" and "shipped and broken" are different blocks; say which.

   Verify each classification against source. A capability the assessment called built because a
   struct exists is half-built until a production call site is named.

3. **Draw the seam list.** For each place new work touches existing work, one row:
   - the existing symbol/module/route/schema it attaches to
   - the shape of the attachment (a new caller, a new field, a new node, a new consumer of an
     existing artifact)
   - **who owns the write** on either side — two writers to one artifact is a defect, not a design
   - whether the contract between them is already typed, or is currently prose that must become data

4. **Blast radius, per seam.** What else breaks if this seam is wrong. Name real consumers found
   by grep, not hypothetical ones. A seam with an unknown blast radius is not plannable — say so
   explicitly rather than guessing.

5. **Spike the riskiest assumption.** Default: exactly one, the assumption whose falsity would
   invalidate the most downstream work.
   - Prefer a smoke run of an existing path over new code. If a subsystem has never been executed,
     running it once *is* the spike and is always the cheapest item on the board.
   - Otherwise write ≤30 lines, throwaway, on a scratch branch or in the scratchpad. It answers
     one question: does the attachment work at all.
   - Record the result — including a negative — in `seams.md`. **A spike that refutes the
     assumption is the most valuable output this command can produce**; do not bury it.
   - Never commit spike code to the plan branch.

6. **Name the forks.** 2–4 genuine decisions that are the operator's, not an agent's. For each:
   the options, what each forecloses, and **your recommendation with a reason**. A fork left
   unstated gets decided silently by whichever agent hits it first.

   Carry forward the assessment's open questions and any `OPEN` decision it recorded — this
   document is where they get shaped into an answerable choice.

7. **Cross-cutting checklist.** Walk it explicitly and write a line for each, including "not
   applicable, because —". These are the items plans omit by default:
   migrations · feature flags · observability (can you tell what it did?) · error and failure paths ·
   auth boundary · performance under real data · concurrency and single-writer discipline ·
   the install/deploy boundary (a merged block delivers nothing until the artifact is deployed) ·
   rollback.

8. **Red-team pass.** Spawn 2–3 **fresh** agents (Opus), each with a different lens — correctness,
   operability, "what does this foreclose in six months" — given `seams.md` and told to attack it.
   Their brief: *"Give me three concrete ways the attachment described here fails."* Fold what
   survives into the document; record what you rejected and why. Do not let an agent that helped
   write the seam map review it.

9. **Property self-check.** Revise in place until all hold:
   - Every capability is classified, and every half-built one names its missing piece with file + symbol.
   - Every seam names both sides and a single writer.
   - Every seam has a blast radius or an explicit "unknown — must be spiked".
   - At least one spike was run, or `--spike 0` was passed deliberately.
   - Every cross-cutting item has a line.
   - Every fork has a recommendation.
   - Frontmatter `related:` carries ≥1 real `doc_id`.

10. Commit with an explicit pathspec. Report and point at `/sequence <slug>`.

## Session boundary — usually continue

**`/sequence` may run in this session, and normally should.** It is the same act of judgement
continued — where the work attaches, therefore in what order — and it needs the seam map held in
context, not re-read cold. Its adversarial pass uses fresh subagents, which is where the
independence has to live anyway.

Two conditions send it to a fresh session instead:

- **The forks are not answered yet.** If the operator will take more than a working session to
  decide, stop here. A `/sequence` run that opens with stale assumptions about unresolved forks is
  worse than one that starts cold.
- **This session is already long.** Spikes, red-team returns and a full seam map add up. If the
  next command would be starved of room to read source, hand off.

Close by telling the operator:

```
Seam map complete: planning/<slug>/seams.md

<N> forks need your answer before sequencing — they change the cut, so none can be
deferred into planning:
  1. <fork> — recommend <option>, because <reason>
  ...

Once answered, run in THIS session:
  /sequence <slug>

If you would rather answer these later, I will /handoff now and a fresh Opus
session can pick up /sequence from seams.md.
```

## Output Format

~~~md
---
type: Reference
title: "<Topic> — seam map"
description: <One line: where the new work attaches to the existing system, and what breaks if it is wrong.>
doc_id: seams-<slug>
layer: [<layer>]
project: <repo slug>
status: active
keywords: [seams, integration, <3-5 terms>]
related: [assessment-<slug>, <...>]
---

# <Topic> — seam map

*Derived <DATE> from `assessment.md` as corrected by `verification.md`.*

## Built / half-built / absent

| Capability | State | What is missing | Evidence |
|---|---|---|---|
| <name> | built \| half-built \| absent | <a caller / a flag / a schema agreement / —> | `path` → `symbol` |

## The seams

| # | New work | Attaches to | Shape | Writer | Contract | Blast radius |
|---|---|---|---|---|---|---|
| 1 | <what> | `module::symbol` | <new caller \| new field \| new consumer> | <who writes> | typed \| prose-today | <what else breaks> |

## Spikes run

### <assumption>
**Question:** <the single thing it answers>
**What was done:** <the run or the ≤30 lines>
**Result:** CONFIRMED | REFUTED | INCONCLUSIVE — <what this changes>

## What to delete first
<Dead or superseded surface that should go before anything is extended, and why removing it
shrinks the work.>

## Cross-cutting

| Concern | Applies? | What it means here |
|---|---|---|
| Migrations · flags · observability · error paths · auth · perf · concurrency · deploy boundary · rollback | yes/no | <one line each> |

## Forks for the operator

### <Fork 1 — the question>
- **Option A** — <what it means, what it forecloses>
- **Option B** — <same>
- **Recommendation:** <one>, because <reason>.

## Red team — what survived

| Attack | Verdict | What changed |
|---|---|---|
| <failure mode> | real \| rejected | <the change, or why rejected> |
~~~

## Report

```
planning/<slug>/seams.md

Capabilities:  <b> built · <h> half-built · <a> absent
Seams:         <n> (<u> with unknown blast radius)
Spikes:        <k> run — <result summary>
Forks needing the operator: <n>
Red team: <x> attacks landed, <y> rejected

Next:  /sequence <slug>
```
