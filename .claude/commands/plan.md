# Plan — Author an initiative: its narrative and its block records.

## Variables

$ARGUMENTS — free-text description of the feature, experiment, or body of work to plan.
             Optional flags: `--founding`, `--clarify`, `--lane`.

| Flag | What it does |
|---|---|
| `--founding` | This is the project's founding roadmap — a new repo's first blocks. Adds the Goal / Destination / Architecture framing and writes to `planning/founding/`. Invoked by `/new-project`. |
| `--clarify` | Force the clarify gate on regardless of `planning/harness.json`. |
| `--no-redteam` | Skip the adversarial pass (step 10). For a small, low-risk initiative only. |
| `--lane` | Also emit **`$BRAIN_ROOT/planning/<slug>/lane-<slug>.json`** (D71) — note `$BRAIN_ROOT`, and note it is **not** under `open-work/` — authored against `.claude/workflows/lane.schema.json`, so `/begin-orchestration` can drive this initiative's blocks in dependency order, the same mechanism `/generate-roadmap` gives a multi-repo program. **RECOMMEND THIS unless the operator says otherwise** — see "Recommending `--lane`" below; step 7c has the exact path, the two traps, and the run command. |

### Recommending `--lane`

**Default to recommending it.** In this fleet's current workflow, a single-repo initiative is
normally worked by `/begin-orchestration` driving the chain, not block-by-block by hand — so the
lane record is usually wanted, and an initiative without one has to be re-planned or hand-authored
later to get it. (This reverses earlier guidance here that called `--lane` a rare opt-in and warned
that emitting it would surprise the caller. It does not; the common case is the reverse.)

So:

- **`--lane` passed** — emit it (step 7c), no question needed.
- **`--lane` NOT passed** — still author the plan and the block records in full. Then, in the
  report, **recommend it in one line and give the exact command to add it**, so the operator can
  take it without re-reading this file. Do not silently emit it: the flag is the operator's, and a
  lane record is a live orchestration input, not a harmless extra file.
- **Skip the recommendation** only when the initiative is genuinely hand-work — a single block, or
  blocks the operator has said they want to drive one at a time.

The recommendation costs one line. Omitting the record costs a re-plan.

> **Where this writes — resolve `BRAIN_ROOT` first.** Walk **up** from the current working
> directory until you find a `brain.toml` (its first line begins `# brain.toml`); that directory is
> `BRAIN_ROOT`. Pre-plan output **always** goes to `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/`,
> never the current repo's own `planning/` — one place to look, whichever repo you invoked this
> from. Scope the work *inside* the folder instead: set `project: <repo-slug>` in the frontmatter
> and prefix the slug when it is repo-specific (`mev-error-handling`).
>
> **Why centralized** (HQ D87): `/sequence` decides the successor by counting repos, and the
> multi-repo successor `/generate-roadmap` writes `planning/roadmaps/<slug>/`, which exists only in
> HQ — so a leaf-repo pre-plan would have to migrate cross-repo on every multi-repo cut. That
> migration is `/generate-roadmap` Step 7b, measured running 3 times in 34 roadmaps. Starting in HQ
> removes the move. Note also that every repo's `planning/` is a symlink into `core/_planning/`
> tracked by the one HQ git repo, so "local" was never a separate repository anyway.
>
> **If no `brain.toml` is found**, this is a standalone repo: write to `planning/open-work/pre-plan/<slug>/`
> relative to the repo root, and say so in the report.

## Purpose

Author **one initiative**: a coherent body of work in **one repo**, spanning one or more blocks.
**This command writes IN PLACE, into `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/`** — `plan.md` lands beside the
`sequence.md` it was authored from, and any pre-plan artifacts (`assessment.md`, `seams.md`,
`evidence/`) stay exactly where `/assess` left them. That is deliberate and is one half of an
invariant: `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/` and `planning/roadmaps/<slug>/` are never both populated. This command
satisfies it by staying put; `/generate-roadmap` satisfies it by relocating the pre-plan into
`planning/roadmaps/<slug>/pre-plan/` (its Step 7b). Do not move anything here.

Output is two things, plus a third when `--lane` is passed:

- `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/plan.md` — the authored **narrative**: the goal, the sequencing rationale, the
  architecture framing, the cut list. Everything true of the *set* rather than of any one block.
- `planning/blocks/<BlockID>.json` — one **block record** per block. The definition of each
  member.
- `$BRAIN_ROOT/planning/<slug>/lane-<slug>.json` (only with `--lane`) — the **lane record**
  `/begin-orchestration` drives: the same blocks, in dependency order, in the one shape
  `.claude/workflows/lane.schema.json` defines. **`$BRAIN_ROOT`, and never under `open-work/` or
  `roadmaps/`** — step 7c has the three ways to get this path wrong and why each fails silently.

`/plan` does **not** write `tasks.json`. Task decomposition is deferred to
`/generate-tasks <BlockID>`, run later, per block — because a later block's tasks depend on an
earlier block's code, and decomposing everything at authoring time burns tokens on work that gets
re-derived anyway (D65).

> **Scope ladder.** Multi-repo program → `/generate-roadmap`. One repo, multiple blocks → here.
> A single block → `/ticket` (behavior change) or `/chore` (maintenance).
> `master-plan.md` is **generated** from the block graph, not authored — never hand-write one.
>
> **Orchestrating the result.** A `/plan` initiative can be driven the same way a `/generate-roadmap`
> program is — `/begin-orchestration` knocking out its blocks in dependency order, with the same
> notes, run records and documentation discipline. Pass `--lane` to also emit a lane record
> (step 7c). **This is the common case in this fleet and you should recommend it** — see
> "Recommending `--lane`" above. Without it, `/plan` produces only the narrative and block records
> and the initiative has to be worked block-by-block by hand.
>
> **Upstream.** For work on an existing system large enough that the cut is not obvious, the
> pre-plan pipeline runs first: `/assess` → `/seams` → `/sequence`. Its output,
> `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/sequence.md`, is this command's authored input (step 3a). Planning a
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

3a. **Read the pre-plan artifacts if they exist.** Check `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/` for `sequence.md`,
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
   slug is `founding`. Create `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/` if absent.

7. **Write each block record and register it.** For every block, read and follow
   `.claude/workflows/block-registration.md` — the canonical procedure for the block ID, the
   operator and cross-repo edge questions, the carryover read, the block record, and `state.json`
   registration. Do not restate it here or invent a variant. Set `kind` to `block` and
   `initiative` to `<slug>`.

7b. **Run the initiative-wide consistency pass before registration closes** — Step 7 of
   `.claude/workflows/block-registration.md`, once for the whole batch, never per block. It is the
   only step that reads every block of the initiative at once, and every defect it catches is one a
   correctly-authored single block cannot show: a second repo with no block record (and `external`
   edges standing in for unfiled fleet work), a block whose `files[]` leave its own repo's tree, a
   split row whose halves both inherited the original's `depends_on`, a sizing flag carried forward
   instead of decided, an operator `exit` naming an artifact nobody can point at, and actionable
   work left in prose with no row in `state.json`. Report what it
   found — including "nothing", which on a multi-block initiative is a claim.

7c. **When `--lane` is set, emit the lane record.** Authored against
   `.claude/workflows/lane.schema.json` (D71). One repo, one lane: this command scopes to a single
   repo, so the lane needs no cross-repo assignment, just the blocks in dependency order.

   **The path — get this exactly right, it is the one thing here that fails silently:**

   ```
   $BRAIN_ROOT/planning/<slug>/lane-<slug>.json
   ```

   Three ways to get it wrong, all of which produce a schema-valid file that nothing can find:

   - **NOT the current repo's `planning/`.** Even when you invoked `/plan` from a leaf repo, this
     goes to the **brain root**. mev's `discover_lane_files` (`core/mev/src/brain/lane_segments.rs`)
     reads `<brain-root>/planning` and nothing else, so a record under a repo's own vaulted
     `planning/` is never discovered. **Measured 2026-09-10:** the fleet's one leaf-repo lane record
     (`side/_planning/price-scout/home-ready/lane-home-ready.json`) returns **zero** hits from
     `mev lanes`, against **eight** for a brain-root control lane — schema-valid, on disk, and
     completely unreachable for months. `check_lane_records.py` passes it, because that script
     validates whatever `--planning` path you hand it; **schema-valid and reachable are different
     claims** and only the second one matters here.
   - **NOT under `open-work/`** (HQ D87). `open-work` is in `NON_ROADMAP_DIR_NAMES`, so a record
     left there is skipped outright. An earlier version of this command's own flag table named that
     path; it was wrong and is fixed.
   - **NOT under `planning/roadmaps/<slug>/`.** That is `/generate-roadmap`'s output directory. A
     `/plan` initiative uses the legacy sibling `planning/<slug>/`, which is the fallback
     `/begin-orchestration` Step 1C rule 2 resolves to.

   The narrative and the machine record deliberately live apart, per D65: `plan.md` goes to
   `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/`, the lane record to
   `$BRAIN_ROOT/planning/<slug>/`.

   - `lane` = `<slug>`. `roadmap` = `<slug>` — this is what makes the initiative resolve the same
     way a roadmap does: `/begin-orchestration`'s bare-slug resolution checks
     `planning/roadmaps/<slug>/` first, then falls back to legacy `planning/<slug>/`. No change to
     `/begin-orchestration` is needed or permitted; this only works because that fallback exists.

   - **The lane log lands in the resolved `roadmap_dir`, which for a `/plan` initiative is
     `planning/<slug>/`** — so the plain run command below is correct and needs no `--log`
     override:

     ```
     /begin-orchestration --roadmap <slug> --lane <slug>
     ```

     This is worth a sentence because it was briefly broken and the failure was silent.
     `/begin-orchestration`'s `--log` default used to be hardcoded to
     `planning/roadmaps/<slug>/lane-log.jsonl` while Step 1C resolved a single-repo lane to the
     legacy `planning/<slug>/`. Taking the default **created** the roadmaps directory; on the next
     run Step 1C rule 1 preferred it, found no lane record there, and `--lane` stopped resolving —
     the first run silently broke the second. Fixed 2026-09-10: both the log and the escalations
     path now derive from the resolved `roadmap_dir`. **If you ever see that default re-derived from
     the slug again, that is the regression** — `scripts/lane_log_watermark.py` already resolves the
     same two locations in the same order, and the two must stay in step.

   - `blocks[]` — every block written in step 7, **in dependency order** (the order later blocks
     depend on earlier ones, matching the Sequence Table), each as
     `{"id": "<BlockID>", "origin_roadmap": "<slug>", "repo": "<this repo's slug>"}`. IDs must be
     the exact ones written to `planning/blocks/<BlockID>.json` — never re-minted here.
   - `budget.heavy` — determine mechanically, never from memory:
     `python3 <path-to-base-template>/scripts/fleet_concurrency_check.py is-heavy --repo-path <this-repo>`.
     Author the result as `budget.heavy`; omit `budget` entirely only if the script cannot run.
   - Leave `isolation` unauthored unless this initiative needs a specific override — plain
     `/begin-orchestration --roadmap <slug> --lane <slug>` resolves isolation itself via its own
     policy table (`--isolation auto`).
   - Validate before handing over: `python3 <path-to-base-template>/scripts/check_lane_records.py --planning planning`.
     A failure here means the lane file, not the plan, is wrong — fix and re-run.

8. **Write the narrative** to `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/plan.md` using the Output Format below. The
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
   - **The target repo's `master-plan.md` carries the `generated:wave-table` sentinel pair.** Check
     it, and add the pair if it is absent:

     ```bash
     grep -q 'generated:wave-table' <repo>/planning/master-plan.md || echo MISSING
     ```

     `mev emit-state --write` splices the derived wave table between
     `<!-- BEGIN generated:wave-table -->` and `<!-- END generated:wave-table -->`. **A file with no
     pair is silently SKIPPED** (`W_EMIT_NO_SENTINEL`) — the blocks register fine, every gate passes,
     and the wave table stays empty forever with nothing pointing at the cause. This check exists
     because `/generate-master-plan` used to add the pair and **D65 retired it without anything
     inheriting the job**, so repos scaffolded in that window have no sentinel. base-template's
     scaffold now ships the pair, but an older repo will not have it. Never hand-author rows between
     the sentinels; add the empty pair and let `emit-state` fill it.
   - **Nothing actionable exists only in this document.** Every open question, follow-up, agreed
     red-team finding and "we should also" in `plan.md` is either a row in `state.json` — a block,
     an operator/approval edge, a `carryover[]` entry, a `reference[]` fact, a backlog row — or a
     cut-list line with a reason. Those are the only two destinations. Prose gates nothing and
     surfaces on no board, so an item held only here is lost, not deferred.
   - **When `--lane` was passed, the lane record (7c) validates AND is reachable** — two separate
     checks, because the first cannot detect the path mistake that step 7c warns about.
     `check_lane_records.py` clean, every `blocks[].id` matching a real `planning/blocks/<ID>.json`,
     and array order matching the Sequence Table's dependency order. **Then prove mev can actually
     see it:** `mev lanes` names the lane and reports its first block. If the lane is absent from
     that output, the record is in the wrong directory — re-read step 7c's path block. A green
     `check_lane_records.py` on an unreachable file is the exact false-clean this pairing exists to
     catch.
   - **The consistency pass (7b) ran and is reported** — C1–C6, with what it found and what was
     left standing deliberately. Specifically: no `depends_on` edge is `{"type": "external"}` for
     work that lives in a fleet repo; every block whose `files[]` reach outside its own repo's tree
     says so and names the lane it may not run beside; no two blocks split from one row carry
     identical inherited edges; every oversized-flagged block carries a split-now-or-defer decision;
     and every operator `exit` names an artifact you can point at on disk or at the block that
     creates it — otherwise it says `UNRESOLVED`.

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

## Session boundary — end here

**This command ends its session. `/generate-tasks` runs fresh, one session per block.**

The reason is structural, not hygienic. `/generate-tasks` reads **only** the target block's record —
not this narrative, not sibling blocks — because that is what makes a block record self-sufficient
and what lets a block be decomposed months later. Run in this session, it would lean on plan context
the record does not carry, and the record's incompleteness would never surface. The property the
self-check asserts is only actually tested by a fresh session.

Per block, not all at once: a later block's tasks depend on an earlier block's code, and
decomposing everything now burns tokens on work that gets re-derived (D65).

Close with `/handoff` and tell the operator:

```
Plan authored: $BRAIN_ROOT/planning/open-work/pre-plan/<slug>/plan.md — <N> phases, <M> blocks in planning/blocks/
Handoff test on <first block ID>: PASS | FAIL — <what was missing>

Start a FRESH session per block — Sonnet is right for most; use Opus when the block
has breaking public-surface changes or several un-gateable criteria — and run:
  /generate-tasks <first runnable block ID>

Fresh matters: /generate-tasks reads only the block record. If it cannot decompose
the block from that record alone, the record is thin and should come back here —
do not repair it from memory in the decomposition session.

Handoff written to planning/handoff.md.
```

## Codebase Structure

- `CLAUDE.md` — standing rules, stack, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — validation commands + UI-test config
- `planning/blocks/` — block records; `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/` — initiative narratives
- `.claude/workflows/block.schema.json` — the block record field contract
- `.claude/workflows/block-registration.md` — the shared registration procedure
- `.claude/workflows/lane.schema.json` — the lane record field contract (`--lane` output, D71).
  The record itself goes to `$BRAIN_ROOT/planning/<slug>/`, NOT this repo's `planning/` — step 7c
- `scripts/check_lane_records.py` — validates a lane record; run it before handing over a `--lane` run

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
created: <YYYY-MM-DD — today's date>
updated: <YYYY-MM-DD — today's date; bump this whenever you revise the file>
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
$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/plan.md            (<N> phases, <M> blocks)
planning/blocks/                   <M> block records written
state.json: <created | already existed>, <M> blocks registered
$BRAIN_ROOT/planning/<slug>/lane-<slug>.json
                                   <written, <M> blocks, mev lanes discovers it | not requested>

Blocks ready to decompose:
  - <ID> — <name>
  ...

Pre-plan input:   sequence.md <used | absent> <, departures: ...>
Consistency pass: C1 <n> · C2 <n> · C3 <n> · C4 <n> · C5 <n> · C6 <n> — <fixed | none found>
Handoff test on <first block ID>: PASS | FAIL — <what was missing>
Red team: <x> attacks landed, <y> rejected

Next (turn the first block into a runnable spec):
  /generate-tasks <ID>

<If --lane was passed — ALWAYS include the --log flag, see step 7c:>
Or drive the whole initiative through the SDLC engines in order:
  /begin-orchestration --roadmap <slug> --lane <slug> --log planning/<slug>/lane-log.jsonl

<If --lane was NOT passed, and the work is not deliberate hand-work — recommend it in one line:>
No lane record (--lane not passed). This initiative's <M> blocks are sequential, so
/begin-orchestration can drive them; to add the record without re-planning:
  /plan --lane "<the same description>"
```
