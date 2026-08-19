# Assess — Fan out recon agents over an existing codebase and produce a verified assessment.

Stage 1 of the pre-plan pipeline: `/assess` → `/seams` → `/sequence` → `/plan`.
Method: `docs/how-to-plan-with-agents.md` in the brain repo.

## Variables

$ARGUMENTS — the assessment topic, plus optional flags.
             Example: "autonomous multi-repo orchestration --slug orchestration-extensions"

| Flag | What it does |
|---|---|
| `--slug <name>` | Output directory `planning/<name>/`. Default: kebab-case of the topic |
| `--areas "a; b; c"` | Name the recon areas explicitly instead of deriving them |
| `--depth quick\|standard\|deep` | 3 / 5–6 / 8+ scouts. Default `standard` |
| `--no-verify` | Skip the verification pass. Only for a throwaway look — never before `/plan` |
| `--resume` | Re-read an existing `assessment.md` and run only the stages that are missing |

## Purpose

Turn "I want to understand this system well enough to plan work on it" into **one dated,
cited, independently re-checked artifact** — `planning/<slug>/assessment.md` — that a later
session can plan from without re-deriving anything.

This command produces **evidence, not a plan.** It may not propose a sequence, a wave, a block,
or an estimate. Those are `/seams` and `/sequence`.

> **Scope.** One coherent question about one system (possibly spanning repos). If you cannot
> state the question in a sentence, you have two assessments.

## Instructions

1. If `$ARGUMENTS` is empty, stop and ask what to assess.

2. **Frame the question.** Restate the topic as **one sentence** and 3–8 **numbered questions**
   the assessment must answer. Show them to the user and proceed — these become the scout briefs
   and the section headings, so a vague question here produces a vague dossier.

   - **Plan-quality floor.** If framing the question requires inventing a load-bearing fact about
     intent (what "done" means, which repos are in scope, what the operator actually wants), STOP
     and ask 2–4 targeted questions. Do not guess the objective.

3. **Establish ground truth before reading anything.** Do not skip this and do not delegate it.
   - Read `CLAUDE.md` and `planning/context.md`.
   - Run the repo's gated checks (`planning/harness.json` → `validation.checks[]`). Record which
     pass, which fail, and how long they take.
   - If the subsystem under assessment has a way to be *run*, run it once. **A smoke run beats an
     inference.** A subsystem that has never been executed cannot be diagnosed by reading.
   - Record `git rev-parse --short HEAD` per repo in scope, and today's date. Everything below is
     pinned to those.

4. **Sweep what the fleet already knows — before the scouts, not after.** A finding already filed
   is not a finding, and a constraint already ratified is not an open question. Read, with
   `rg -L` (every `planning/` is a symlink):
   - every in-scope repo's `planning/carryover` entries in `state.json`, `planning/knowledge.md`,
     `planning/memory.md`, `planning/backlog.md`
   - `planning/decisions/` in each repo **and** the brain's `docs/decisions/`
   - **Decision numbers collide across repos** (HQ D62 ≠ base-template D62). Cite every decision as
     `<repo> D<n> — <title>` and confirm the title matches before relying on it.
   Write what this sweep changes into a `## What the corpus already knew` section. Nothing found
   here may be re-discovered as novel later in the document.

5. **Fan out the scouts.** One Agent per area, all in **one message** so they run concurrently.

   - **Model:** Sonnet by default. Escalate at most one or two areas to Opus when the area
     genuinely requires holding several subsystems at once. Use Fable only for a purely mechanical
     inventory (counts, file lists, dependency maps).
   - **Fresh context each, narrow brief each.** Never give a scout the framing of another scout's
     findings — independent readings are the only thing fan-out buys.
   - **Always include a reuse scout**, whatever the topic: *"What already exists — in this repo or
     a sibling — that this work would duplicate or could call instead of building?"* This is
     reliably the highest-value area and the one most often omitted.
   - **Always include a deletion scout:** *"What exists here that is dead, superseded, or should be
     removed before anything is extended?"*
   - Scouts are **read-only**. They may not write files, edit state, or commit.

   Each scout brief must contain, verbatim:

   ```
   Report FINDINGS, not recommendations. You have seen one part of this system; a
   recommendation from your position is a guess.

   Output shape — at most 40 lines. For each finding:
     CLAIM      one sentence, falsifiable
     EVIDENCE   file path + symbol (function/struct/type). Line numbers move —
                name the symbol so the next reader can grep it.
     CONFIDENCE verified-in-source | inferred | unverified
     LIMITS     what this does NOT establish

   If you cannot find something, write NOT FOUND with where you looked. Never
   fill a gap with a plausible-sounding answer.
   ```

   Write each scout's raw return verbatim to `planning/<slug>/evidence/<area>.md`. **You are the
   only writer** — scouts return text, you write files.

6. **Synthesize.** In this session, holding all returns at once. Write
   `planning/<slug>/assessment.md` in the Output Format below. While synthesizing:
   - Reconcile contradictions between scouts explicitly — name both and say which you believe and why.
   - Mark every claim's confidence. An `inferred` claim may not be stated as fact.
   - Keep the code map: every file, symbol, struct, function and interface a finding touches.

7. **Verification pass (unless `--no-verify`).** Fan out a *second*, fresh set of agents.
   - **Give them claims, not conclusions.** Extract the load-bearing claims from
     `assessment.md` — especially any claim that shapes what gets built first — and hand each
     verifier a subset with no surrounding argument.
   - Each returns `VERIFIED | REFUTED | PARTIAL | NOT FOUND` plus file/symbol proof.
   - Sonnet is correct here; this is lookup with a verdict.
   - Re-derive **every absolute number** (counts, percentages, totals). Numbers rot; the ratios a
     conclusion rests on usually survive. Say which is which.
   - Write `planning/<slug>/verification.md`, and add a banner at the top of `assessment.md`
     pointing at it. **Where the two disagree, verification wins** — it is later and was checked
     against source.
   - Any claim that comes back REFUTED must be corrected **in `assessment.md` itself**, not only
     noted in the verification file. A stale claim left in the main document is the version a
     skimming planner will act on.

8. **Spot-check by hand.** Pick three citations at random — one from a scout, one from the
   synthesis, one that verification marked VERIFIED — and check them yourself. Report the result.
   This is the only defense against a verifier that agreed with a fabrication.

9. **Stop.** Before writing another scout, apply the stopping rules:
   - a new pass returns things you already know → stop
   - a finding is true but changes nothing downstream → drop it, do not chase it
   - the next unknown is cheaper to answer by **writing 30 lines of code** than by reading → stop
     and say so; that is a spike for `/seams`, not more assessment
   Budget: two rounds of discovery, one targeted follow-up, one verification pass. Exceeding that
   requires the user's say-so — ask.

10. **Write the index.** If `planning/<slug>/` holds more than three documents, write an
    `index.md` with a reading order and an "if you only need one thing" router. Update the parent
    `planning/index.md` per the project's standing rules.

11. **Commit** with an explicit pathspec (never `git add -A`):
    `git commit -o planning/<slug> -m "docs(<repo>): assessment — <topic>"`

12. Report the paths, the three spot-checks, the open questions, and the next command
    (`/seams <slug>`).

## Output Format

~~~md
---
type: Reference
title: "<Topic> — assessment"
description: <One line: what this assessment establishes and what it deliberately does not.>
doc_id: assessment-<slug>
layer: [<layer>]
project: <repo slug>
status: active
keywords: [assessment, <3-5 terms>]
related: [<≥1 real doc_id>]
---

# <Topic> — assessment

*Assessed <DATE> against `<repo>@<sha>`<, `<repo2>@<sha2>`>. This folder is **evidence, not a
plan**. Line numbers move — grep the symbol, not the number.*

> **Read `verification.md` before acting on anything here.** <N> agents re-checked these claims
> on <DATE>. Where the two disagree, that document is later and was verified against source.

## The question
<One sentence, then the numbered questions this document answers.>

## Ground truth
<What was built/run/tested, what passed, what failed, how long it took. The commit SHAs.>

## What the corpus already knew
<From the carryover / decisions / knowledge / memory / backlog sweep: what was already filed, what
constraints are already ratified, what this changes about the questions above.>

## Findings
### <Area 1>
<Prose. Every load-bearing claim carries file + symbol and a confidence marker.>

## Contradictions and how they were resolved
<Where two scouts disagreed, and which reading survived.>

## What is dead or superseded
<From the deletion scout. What should be removed before anything is extended.>

## What already exists that this work would duplicate
<From the reuse scout. The single highest-value section — be specific about the call to make
instead of the code to write.>

## Confidence ledger

| Claim | Confidence | Evidence | Verified? |
|---|---|---|---|
| <claim> | verified-in-source \| inferred \| unverified | `path` → `symbol` | VERIFIED \| REFUTED \| PARTIAL |

## Open questions
<What this assessment could not answer, and for each: is it cheaper to read or to spike?>

## Code map
<Every file, struct, function and interface these findings touch, grouped by module.>
~~~

## Report

```
planning/<slug>/assessment.md      (<N> areas, <M> findings)
planning/<slug>/verification.md    (<V> claims re-checked: <x> verified, <y> refuted, <z> partial)
planning/<slug>/evidence/          <N> scout reports

Hand spot-checks: <3 citations, result each>
Refuted and corrected in place: <list, or none>
Open questions for the operator: <count>

Next:  /seams <slug>
```
