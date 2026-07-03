# Generate Tasks — Generate a task spec for a specified phase and block.

## Variables

$ARGUMENTS — one of two input modes:
             - **Master-plan slug mode (default):** the spec's `planning/` directory name (its
               phase-dotted slug), e.g. `<spec-slug>` or `2.1-learn-paths-structural-fixes`. New
               master-plan specs follow the `P.N-slug` convention (see `planning/index.md` → *Task
               directory naming convention*). The block definition is read from `master-plan.md`.
             - **Plan-file mode (`--from <path> [phaseN-blockX]`):** decompose a block from a
               standalone plan file instead of `master-plan.md`. The file may be either a single
               standalone block definition (legacy D34) or a master-plan-format `/plan` output with
               `## Phase N` / `### Block X` headings. For a master-plan-format file, append the
               `phaseN-blockX` selector to pick which block to decompose (required when the file has
               more than one block). Used for ad-hoc / experimental features kept out of the roadmap
               (see `planning/decisions/D34-adhoc-planning-seam.md`).
             Required. If omitted, stop and say: "Usage: /generate-tasks <P.N-slug>  (e.g.
             <spec-slug>), or /generate-tasks --from planning/plan-<slug>/plan.md [phaseN-blockX]"

## Instructions

1. Run `/prime` to orient to the repo (standing rules, architecture).

2. **Resolve the input mode and the spec slug.**
   - **If `$ARGUMENTS` contains `--from <path>` (plan-file mode):** the source is the plan file at
     `<path>`. Derive the spec slug from the **parent directory name** of `<path>` (e.g.
     `planning/plan-add-rate-limiter/plan.md` → slug `plan-add-rate-limiter`); the decomposed
     `tasks.md` is written **into that same directory** (`planning/plan-add-rate-limiter/tasks.md`),
     so `/sdlc-flow <slug>` / `/sdlc-run <slug>` can run it. If `<path>` does not exist, stop and say
     so. Then resolve which block to read:
     - If a `phaseN-blockX` selector follows `--from <path>` (accept any of `phase0-blockA`,
       `phase0blockA`, `0-A`, `Phase 0 Block A`), that names the block to decompose.
     - If no selector is given, inspect `<path>`: a **single standalone block file** (no `## Phase` /
       `### Block` headings — legacy D34) is decomposed whole; a **master-plan-format file** with
       exactly one block defaults to that block; a master-plan-format file with **more than one
       block** has no safe default — STOP, list the blocks, and ask which one (plan-quality floor:
       never guess a load-bearing target). To run the whole multi-block plan instead, point the user
       at `/sdlc-block <path>`.
   - **Otherwise (master-plan slug mode):** parse `$ARGUMENTS` to extract the phase number and
     block/project identifier (e.g. `phase0-blockC` → phase 0, block C). Accept any of these forms:
     `phase0-blockC`, `phase0blockC`, `0-C`, `Phase 0 Block C`. The spec slug is the normalized
     directory form (e.g. `<spec-slug>`). If the argument cannot be parsed into a phase + block, stop
     and explain the expected format.

3. Check whether a spec already exists at `planning/<spec-slug>/tasks.md` + `tasks.json` (using the slug resolved in
   step 2; in `--from` mode the slug is the source file's parent directory).
   - If it exists, read it and report: "Spec already exists at <path>. Overwrite? (re-run with
     `--force` appended to overwrite, or run `/breakdown <path>` to decompose it instead.)"
   - If `$ARGUMENTS` contains `--force`, proceed and overwrite.

4. **Read the source block definition.**
   - **Plan-file mode (`--from <path>`):** read the plan file at `<path>`. When it is master-plan
     format, read ONLY the section for the block resolved in step 2 (its `## Phase N` → `### Block X`
     subsection) — not the overview, not sibling blocks; when it is a single standalone block file,
     read the whole file. Treat its substance — the goal/description, problem/solution, relevant
     files, and acceptance criteria — as the block definition. **Author a fresh decomposed
     `tasks.json` from it; do not merely copy a pre-existing step list verbatim** (apply the same scoping
     and disjoint-ownership rigor below). Do NOT read `master-plan.md` in this mode.
   - **Master-plan slug mode:** read ONLY the relevant section for the requested block in
     `planning/master-plan.md` (the phase/block definition).
   - In both modes: do NOT read status.md — the target is given explicitly.
   - **Use what the block already gives you.** A well-authored block (see `/generate-master-plan`)
     names its **Files** (New vs Modified, by path), an **Out of scope** boundary, and an optional
     **Interfaces / shared surface**. When present, **carry these through** rather than re-deriving:
     the named files seed each task's disjoint ownership (step 6), and **Out
     of scope is a hard boundary** — do not generate tasks beyond it. Only fall back to deriving file
     ownership yourself when the block doesn't name files.

5. **Clarify gate (only when enabled).** Read `planning/harness.json` → `planning.clarify`. When it is
   `true` **or** `$ARGUMENTS` contains `--clarify`, and the block definition is genuinely ambiguous (its
   scope, deliverables, or task boundaries could be read more than one way), pause and ask the user
   **2–4 targeted clarifying questions** before writing the spec; fold the answers into the tasks. If the
   block is already unambiguous, skip the questions and proceed even when the gate is on. When
   `planning.clarify` is absent/`false` and no `--clarify` flag is present, skip this step entirely and
   behave exactly as before. (`--clarify` is a control flag only — do not treat it as part of the
   phase/block slug when parsing `$ARGUMENTS`.)
   - **Plan-quality floor — clarify-or-abort, never fabricate (holds even when the gate is off).** If
     decomposing the block would require *inventing* a load-bearing fact you cannot ground in the
     block definition, `CLAUDE.md`, `planning/context.md`, or the repo (e.g. which files a task owns,
     an observable acceptance criterion, a real dependency edge) — do not emit a fabricated `tasks.md`.
     Instead: in an **interactive session**, STOP and ask the user a targeted question; in a
     **non-interactive / preflight context** (invoked by `/sdlc-block` / `/sdlc-flow` to auto-generate
     a missing spec), **ABORT with a specific message naming exactly what's missing** so the human can
     fix the block. This is the proactive complement to the D19 thin-spec abort: D19 catches a thin
     spec after the fact; this prevents writing a confidently-wrong one in the first place.

6. THINK HARD about correct scope:
   - Do not invent work beyond what the block defines.
   - Size tasks to roughly 21 hours spread across Mon/Wed/Fri sessions.
   - Enforce **the project's standing rules** as written in `CLAUDE.md` — do not assume any stack, locale-parity, or content-layout rule unless written there. Every task must leave the project's gated checks (`planning/harness.json` → `validation.checks[]` with `gates: true`) passing.
   - **Disjoint file ownership (parallel-merge safety).** A block's tasks run as parallel pipelines that merge independently, so two tasks editing the same existing file collide at merge. Decompose so each task **owns a distinct set of files**. When two tasks would touch the same file, either (a) make one `dependsOn` the other so `/sdlc-block` serializes them into different waves, or (b) restrict the shared file to **append-only** edits (the block engine union-merges files declared `additiveFiles`). Name each task's primary files in its step so the dependency analysis can see the boundaries — an undeclared overlap escalates the whole block on a merge conflict.
   - Foundational steps come first; the final step is always Validate.
   - **Write the task list as `tasks.json`, not markdown headings.** Every SDLC engine reads
     `planning/<spec-slug>/tasks.json` directly — a **bare array** of `{task_id, title, description,
     acceptance_criteria, validation_commands, max_attempts, files, dependsOn}` objects (see Output
     Format below), the same shape orchestrator's `SDLC_FLOW` workflow already consumes
     (`app/schemas/sdlc_schema.py`'s `SDLCTask`) — instead of parsing `tasks.md` for a heading
     pattern. `tasks.md` still carries the prose (Goal, Context Pointers, Acceptance Criteria,
     Validation Commands, Notes, Amendment Log) but the Step-by-Step Tasks section in it is just a
     one-line pointer at the JSON file, not the task list itself.

7. Create the directory `planning/<spec-slug>/` if it does not exist, then write **both**
   `planning/<spec-slug>/tasks.md` (prose) and `planning/<spec-slug>/tasks.json` (task list) using
   the Output Format below. (In `--from` mode the directory already exists — it holds the source
   block file — so the two new files land beside it.)

8. **Property self-check (before committing).** A structurally valid spec can still be substantively
   thin and waste pipeline tokens. Re-read what you just wrote and confirm every required property
   holds; **revise the spec in place** if any fails, then re-check:
   - **`tasks.json` parses as valid JSON** and is a non-empty array (not wrapped in an object —
     orchestrator's `LoadTaskStateNode` expects a bare array).
   - **Every task except the final Validate task names ≥1 file** in its `files[]` (so the dependency
     analysis and disjoint-ownership guard can see boundaries).
   - **`dependsOn` ids are all valid** — every id referenced exists as some task's `task_id` in the
     same array, and the final Validate task depends on every other task's id.
   - **Acceptance Criteria are non-empty and observable** — each criterion can be judged true/false.
   - **Validation Commands are present** (or `planning/harness.json` → `validation.checks[]` supplies
     them as the fallback).
   - **No leftover template sentinels** — no `{{TOKEN}}`, no literal seed strings the Output Format
     ships (`<placeholder>`-style angle stubs left unfilled, empty AC/Validation bullets, or a
     `tasks.json` task still reading `<Foundational step>`). Do **not** treat legitimate `<...>` in
     code/prose (e.g. `Vec<T>`, "the `<concept>` folder") or a bare `TODO`/`TBD` inside authored
     content as a sentinel.

9. **Commit the spec.** Leave the working tree clean so a downstream `/sdlc-block` run never trips
   its clean-tree merge guard (an uncommitted `tasks.md`/`tasks.json` blocks every merge):
   ```bash
   git add planning/<spec-slug>/
   git commit -m "chore: add spec for <spec-slug>"
   ```
   (Use the slug resolved in step 2 — the master-plan directory slug, or in `--from` mode the source
   file's parent directory. The `git add` stages the source block file too, which is fine.)

10. **Decomposition assessment.** Before reporting, evaluate each task you just wrote against the
   coarseness heuristic and recommend which (if any) warrant a `/breakdown` first. The real predictor
   is SEPARABLE STRUCTURE, not raw file count. A task is a breakdown candidate when ANY hold: it bundles
   multiple separable concerns ("implement X AND refactor Y AND add Z"), OR it spans multiple layers
   (data model + API + UI), OR it carries a large acceptance-criteria set over several independently-
   testable units, OR it touches more than `breakdown.complexityThreshold` distinct files
   (`planning/harness.json`; default 3) AND those files are HETEROGENEOUS (different shapes/roles or
   spanning more than one concern/layer). Do NOT flag on file count alone when the many files are the
   same shape serving one concern (e.g. a content path's metadata + N near-identical lesson pairs) —
   decomposition yields little there. List the flagged task numbers with a one-line reason in the report
   (the SDLC engines apply the same heuristic at run time per `breakdown.mode`, so this is the
   authoring-time preview of that decision).

11. **Pipeline recommendation.** After writing the tasks, recommend the run command that fits this
   spec, with a one-line reason. The harness is a ladder of escalating ceremony — match the spec to
   the lowest rung that fits. This command decomposes **one** block, so the recommendation is normally
   one of the single-spec engines; `/sdlc-block` is named only to redirect when the block belongs to a
   multi-block roadmap.

   - **`/patch`** — trivial, single-file hotfix with no new tests. Not produced by this command (a
     spec implies enough scope to decompose), so name it only to redirect when the "spec" turns out to
     be a one-line fix.
   - **lean `/sdlc-task <spec-slug> [range]`** — one small unit of behavior change: a handful of
     tightly-coupled tasks that want a fast test→fix loop but no review / docs / PR ceremony. The
     cheapest real engine and the natural runner for `/ticket` and `/chore` outputs. In-place by
     default; `--worktree` to isolate.
   - **`/sdlc-run <spec-slug>`** — one whole spec, full lifecycle (implement→test→review→document→
     wrap-up) in a single shared implement context, in place on the current branch, no PR. Best for
     small / homogeneous / sequential specs where one context holds all the tasks without blurring or
     overflowing.
   - **`/sdlc-flow <spec-slug>`** (default for non-trivial feature work) — one whole spec in a
     dedicated worktree terminating in a PR: sequential tasks (no inter-task merge conflicts), per-task
     test→fix loop (≤3 attempts, Opus escalation), one consolidated end review over the integrated
     tree. Use when the work has many moving parts or a reviewable PR is wanted. `--auto-merge` to
     merge + clean the worktree on a clean PASS; `--no-pr` to stop after wrap-up; `--resume` to
     re-attach after an interruption.
   - **`/sdlc-block <plan-file>`** — the rung *above* a single spec: a multi-block roadmap. If this
     block is one of several in `planning/master-plan.md` or a `/plan` output, drive the whole roadmap
     with `/sdlc-block <plan-file>` — it ensures each block's `tasks.md` and fans out one `/sdlc-flow`
     per independent block as a branch train of reviewable PRs (reviewed with `/review-PR`, merged with
     `/merge-train`) — instead of running this one block alone. In slug mode `<plan-file>` is
     `planning/master-plan.md`; in `--from` mode it is the path you passed to `--from`.
   - **`/sdlc-task <spec-slug> <N>`** — not a strategy for the whole spec; name it only when the right
     move is one specific task in isolation (a high-risk surgical change, or resuming after a failure on
     task N). Say which task number and why isolation matters.

   Recommend exactly one primary command (optionally plus `/sdlc-task <N>` when a single task warrants
   isolation). If `breakdown.mode` is `auto` and any tasks were flagged in step 10, note that breakdown
   must run first and the recommendation applies to each resulting sub-spec, not this spec directly.

12. Report the path written and suggest the next step:
    "Spec written and committed to planning/<spec-slug>/tasks.md. Run `/breakdown planning/<spec-slug>/tasks.md` to decompose into atomic sub-steps."

## Context / Files to Read

- `planning/master-plan.md` (target block section only) — **or**, in `--from <path>` mode, the
  standalone block file at `<path>` instead
- `CLAUDE.md` (the project's standing rules)
- `planning/harness.json` (the project's validation checks)

## Output Format

Two files, same directory, same basename. `tasks.md` carries the prose; `tasks.json` carries the
task list the engines actually execute against.

`planning/<spec-slug>/tasks.md`:
```md
# Task Spec — Phase <N>, <Block/Project> <X>

**Status:** Not started · **Last run:** never

## Goal
<one sentence, taken directly from the plan>

## Context Pointers
<which plan sections are relevant + which repo files / CLAUDE.md sections apply>

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- <specific, measurable condition>
- <specific, measurable condition>

## Validation Commands
```
<the project's validation commands — see `planning/harness.json` (`validation.checks[]`) or CLAUDE.md; one command per line, in order>
```
<!-- Add any spec-specific checks above the standard project checks. -->

## Notes
<filled in as work happens>

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
```

`planning/<spec-slug>/tasks.json` — a **bare array** (not wrapped in an object), matching
orchestrator's `SDLCTask` schema (`core/orchestrator/app/schemas/sdlc_schema.py`) field-for-field
plus two additive fields (`files`, `dependsOn`) orchestrator ignores harmlessly:
```json
[
  { "task_id": 1, "title": "<Foundational step>", "description": "<bulleted actions, one string>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [] },
  { "task_id": 2, "title": "<Next step>", "description": "<bulleted actions, one string>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [1] },
  { "task_id": "N", "title": "Validate", "description": "Run the Validation Commands listed below and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
]
```
`task_id` — 1-indexed integers, dependency-ordered, no gaps (the `"N"` above is illustrative — use
the real next integer). `title`/`description` — required; `description` holds what a `### N.`
heading's bullets used to hold (bulleted lines in one string are fine). `acceptance_criteria` /
`validation_commands` — usually `[]`; the spec-level markdown sections stay authoritative.
`max_attempts` — defaults to 3, only set per-task to override. `files` — every task but the final
Validate task needs ≥1 entry. `dependsOn` — ids that must complete first; the final Validate task
depends on every other id.


### State refresh (do not hand-author `state.json`'s `tasks` field)

If this repo has a `planning/state.json`, run `mev emit-state --write` after committing — it derives
`tracks[].blocks[].tasks` (a `{ file, generated, counts }` pointer + status summary, **not** a copy
of the task list — see `core/planning/state-schema.md`) from the `tasks.json` you just wrote. Do not
hand-edit a `tasks` array into `state.json` yourself; that field is derived, same as `focus`. (This
derivation isn't implemented in `mev` yet — running the command is a no-op until it ships; it's
listed here so the step is already in place when it does.)

## Report

Output the path to the file created, the decomposition assessment, the pipeline recommendation, and the next-step options:
```
planning/<spec-slug>/tasks.md + tasks.json

Decomposition assessment:
  <"All tasks appropriately scoped." OR a list like:>
  - Task 3 — touches 6 files across model + API + UI; recommend /breakdown
  - Task 5 — bundles two separable concerns; recommend /breakdown

Pipeline recommendation:
  <one of:>
  /sdlc-task <spec-slug>         — <N> tasks, one small tested unit; fast test→fix loop, no review/docs/PR
  /sdlc-run <spec-slug>          — <N> tasks, small/homogeneous/sequential; one shared implement context, in place, no PR
  /sdlc-flow <spec-slug>         — <N> tasks, non-trivial feature work; dedicated worktree, per-task test→fix, one end review, PR (<reason: many moving parts / reviewable PR wanted>)
  /sdlc-flow <spec-slug> --auto-merge
                                 — as above; merge PR + clean worktree on clean PASS
  /sdlc-block <plan-file>        — this block is one of several; drive the whole roadmap as a branch train of PRs
  /sdlc-task <spec-slug> <N>     — run task <N> in isolation; <reason isolation matters here>

Next (optional — decompose first):
  /breakdown planning/<spec-slug>/tasks.md

Next (run directly):
  /<recommended-command> <spec-slug>
```
