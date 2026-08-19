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

   **Write each wave's exit as an observation with a command, never as a set of closed blocks.**
   A block closes when its spec is satisfied, which is not the same as the capability working —
   a previous roadmap in this fleet closed 30 of 53 blocks and still shipped a demo nobody had
   loaded in a browser. These lines become the roadmap's Definition of Done verbatim, so they must
   be checkable by running something:

   ```
   OK   `mev lane-frontier --repo engine-rs` lists EN.9.B as ready
   OK   `curl -s localhost:8080/api/runs/<id> | jq .status` returns "aborted" within 5s
   BAD  EN.9.A and EN.9.B closed
   BAD  the engine can stop
   ```

5. **Size each block.** A block is one coherent, independently reviewable unit that
   `/generate-tasks` can turn into roughly one spec. Record for each:
   - **a canonical block ID** — `<PFX>.<phase>.<block>`, e.g. `EN.12.A`, using the repo's `prefix`
     from `brain.toml`. **This is the block's identity everywhere downstream** — the lane files, the
     roadmap tables, `state.json`, and `/orchestrate`, which resolves IDs from the graph and cannot
     do anything sensible with a name it does not find there.

     Allocate it the way `.claude/workflows/block-registration.md` step 1 does: read the **owning
     repo's** `planning/state.json`, take the highest existing phase **for that repo**, and number
     upward. Never read a phase number from `master-plan.md` or `status.md` — narrative files lag,
     and that lag is how one repo came to carry two unrelated "Phase 4"s. A cut spanning six repos
     means six separate allocations, one per repo, each from that repo's own graph.

     A block already registered keeps the ID it has — do not reallocate. Allocating an ID for a
     candidate is cheap and reversible; a block cut later simply leaves its ID unused.

   - **a stable row ref** — `SQ-01`, `SQ-02`, … assigned once and never renumbered. This is a
     **document-local reference for this file's rows only**, so `/generate-roadmap`'s coverage
     crosswalk can prove no row fell out during lane assignment.

     > **`SQ-nn` is not a block ID and must never be used as one.** It may appear in a table
     > column, in prose, and in a lane-file `#` comment for traceability. It may **not** appear as
     > a bare line in a lane file, as a block's `id` in `state.json`, or anywhere `/orchestrate`
     > will try to resolve it. A lane file whose executable lines are `SQ-nn` is unrunnable: every
     > ID misses the graph, and the lane either stops on the first block or improvises a spec for
     > something that was never specced. This has already happened once — the first roadmap built
     > from a `sequence.md` shipped five lane files of `SQ-nn` lines that passed both crosswalks,
     > because a crosswalk checks that refs *appear*, not that they are resolvable.
   - **registration state** — `registered` if the block already exists in its repo's `state.json`
     (give the real ID), or `candidate` if it does not. **`/orchestrate` resolves block IDs from
     `state.json`, and a lane file naming an ID that is not in the graph does not degrade
     gracefully — the lane stops, or improvises a spec.** Every `candidate` becomes a Wave 0
     registration item in the roadmap, so getting this column wrong is what stalls a run on day one.
     Check it, per repo, with `rg -L '"id": "<ID>"' <repo>/planning/state.json` — never from memory.
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

   And once per **repo**, not per block: whether that repo's gates are **heavy** and in which
   category. Determine it mechanically, never by memory:
   `python3 <base-template>/scripts/fleet_concurrency_check.py is-heavy --repo-path <repo>`
   (exit 0 = heavy; the JSON `category` is `browser-automation` or `native-build`). Lane assignment
   downstream is capped per category, so a repo mislabelled light is a run that gets refused at
   registration or thrashes the machine.

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
    - **Every block carries a canonical `<PFX>.<phase>.<block>` ID**, allocated from the owning
      repo's own `state.json`, and every `depends_on` edge names block IDs rather than `SQ-nn`
      refs. Grep your own output: a `depends_on` cell or a Wave 0 row containing `SQ-` is a defect.
    - **No two blocks share an ID**, and no allocated ID collides with one already in that repo's
      graph — check each against the repo's `state.json`, not against this document.
    - Every block carries a stable `SQ-nn` ref and a registration state checked against the repo's
      real `state.json`, and every `candidate` appears in the Wave 0 table.
    - Every repo has a gate weight determined by running `is-heavy`, not asserted.
    - Every wave exit is a command with an expected output, not a list of closed blocks.
    - Frontmatter `related:` carries ≥1 real `doc_id`.

11. Commit with an explicit pathspec. Report the cut and the next command:
    `/plan` for one repo, `/generate-roadmap` for several.

## Session boundary — end here, always

**This command ends its session. The next command runs fresh. This is the strongest boundary in the
chain and it is not optional.**

`sequence.md` exists to be executable by an agent that was not in the room. If the session that
authored the cut also authors the plan or the roadmap, it fills every gap from memory — the reason
behind a wave order, why one repo owns a block, what a `ships` line really meant — and the document
ships with those holes intact. Nobody finds them until an implementer hits one. Running `/plan` or
`/generate-roadmap` fresh **is** the handoff test, performed for real instead of imagined, and it
costs nothing but a new session.

Close with `/handoff` and tell the operator:

```
Sequence complete: planning/<slug>/sequence.md
<n> waves · <m> blocks (<r> registered, <c> candidates) · <e> operator errands

Start a FRESH session — Opus — and run ONE of:
  /plan "<initiative>"                                       — one repo
  /generate-roadmap <slug> --from planning/<slug>/sequence.md — several repos

<Say which, and why: repo count and block count.>

Fresh is required, not preferred. A fresh session reading only sequence.md is the
handoff test actually being run. If it has to ask you something, that is a hole in
this document — send the question back here rather than answering it inline.

Handoff written to planning/handoff.md.
```

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

| Ref | Block ID | Name | Repo | Reg | Ships (what the operator can do the day it lands) | depends_on | Files | Gate | Risk |
|---|---|---|---|---|---|---|---|---|---|
| SQ-01 | `EN.12.A` | <name> | <repo> | `registered` \| `candidate` | <what becomes possible> | `EN.12.B`, `MV.4.A` | <paths> | <check + how it is shown failing> | <what sinks it> |

*`Block ID` is the identity. `Ref` is a row label for this document's crosswalk and appears in no
lane file's executable lines. `depends_on` names block IDs, never refs.*

*Wave exit (an observation with a command — becomes the roadmap's Definition of Done verbatim):*
```
OK  <command> → <expected output>
```

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

## Repos and gate weight

| Repo | Blocks | Gate weight | Isolation | What it owns |
|---|---|---|---|---|
| <repo> | <n> | light \| browser-automation \| native-build | `--worktree` \| `--no-worktree` | <one line> |

*Gate weight from `fleet_concurrency_check.py is-heavy`, not from memory. Isolation is policy for
`base-template` (always `--worktree`) and the brain root (always `--no-worktree`).*

## Wave 0 — what must be registered before any lane launches

| Ref | Block ID | Repo | Why it is not yet in `state.json` |
|---|---|---|---|

*Every `candidate` row above appears here. `/orchestrate` resolves IDs from `state.json`; a lane
naming an unregistered ID stops or improvises.*

## Totals

| Repo | Blocks | What it owns |
|---|---|---|
~~~

## Report

```
planning/<slug>/sequence.md

Waves: <n>   Blocks: <m>  (<r> registered, <c> candidates -> Wave 0)   Operator errands: <e>
Block IDs allocated: <per repo, e.g. EN.12.A-EN.13.F (28) · MV.4.A-MV.4.C (3)>
Repos: <list with gate weight>
Forks resolved: <k>
Blocks re-cut for failing the ships-alone test: <list>
Red team: <x> landed, <y> rejected
Handoff test on block 1: PASS | FAIL — <what was missing>

Next:  /plan "<the initiative>"                    (one repo)
       /generate-roadmap <slug> --from planning/<slug>/sequence.md   (several repos)
```
