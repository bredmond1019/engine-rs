# Generate Tasks — Generate a task spec for a specified phase and block.

## Variables

$ARGUMENTS — one of two input modes:
             - **Master-plan slug mode (default):** the spec's `planning/` directory name — its block
               ID. **Write new spec directories in the canonical form `<REPO>.<phase>.<block>`**
               (e.g. `BA.0.A`, `EN.8.A`) — a repo-unique two-or-three-letter code from `brain.toml`, a
               phase number, and a block letter or number; the directory name equals the block ID
               exactly, no title suffix. This is the form `/generate-master-plan.md` and `/plan.md`
               author (see `/generate-master-plan.md` step 4 for the full rule). **Legacy directories
               still resolve** for as long as they exist — older specs may be spelled
               `<phase>.<block>-<title>` (e.g. `2.1-learn-paths-structural-fixes`) or
               `<repo>-<phase><block>-<title>`; this command does not require migrating them, it only
               writes new ones canonically. The block definition is read from `master-plan.md`.
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

> **The three checks that can fail this command.** They live in full in steps 6 and 8 below; they
> are hoisted here because they are the reason a spec is rejected, and they are easy to skim past
> in a long procedure.
>
> 1. **Compilable task boundaries** — under `/sdlc-flow` and `/sdlc-task` every task must leave the
>    gating suite passing, so a breaking public-surface change may never be split across tasks.
>    This outranks disjoint file ownership: **the tasks merge, not the constraint.**
> 2. **Un-gateable acceptance criteria must be declared (D64)** — any criterion whose evidence
>    lives in another process, another repo, a generated artifact, or an **installed** artefact
>    needs a named failing command or a dedicated fixture-evidence task. A green suite is never
>    itself the evidence.
> 3. **Never fabricate a load-bearing fact** — which files a task owns, an observable criterion, a
>    real dependency edge. Ask in an interactive session; abort with a specific message in a
>    preflight context.

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
   - **Block-record mode (the default since D65):** read
     `planning/blocks/<BlockID>.json` — the authored definition of this block. It is the source of
     truth for `what`, `why`, `files`, `interfaces`, `out_of_scope`, `acceptance_criteria`,
     `validation_commands`, and `depends_on`. Do **not** read `master-plan.md`: it is a generated
     view of the block graph, and reading it instead of the record gets you a stale summary.
     If no record exists (a legacy directory predating D65), fall back to the block's section in
     `planning/master-plan.md` and note the fallback in the report.
   - In every mode: do NOT read status.md — the target is given explicitly.
   - **Use what the block record already gives you.** It names its **files** (new vs modified, by
     path), an **out_of_scope** boundary, and optional **interfaces**. **Carry these through**
     rather than re-deriving: the named files seed each task's disjoint ownership (step 6), and
     **`out_of_scope` is a hard boundary** — do not generate tasks beyond it. Only derive file
     ownership yourself when the record does not name files (a `forward_looking: true` block may
     name them provisionally, in which case refine them and update the record's `updated` date).
   - **Carry the un-gateable criteria through (D64).** An acceptance criterion written in the
     object form with `gateable: false` needs a dedicated fixture-evidence task in `tasks.json` —
     the record names the fixture in its `evidence` field. Do not silently drop it into an
     ordinary task.
   - **Carry the operator edges through.** If the record's `depends_on` holds an `operator` or
     `approval` edge, the spec cannot be run to completion until it clears. Say so in the report
     rather than generating tasks that will stall.

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

5a. **Read the actual source the block names — before writing any task.** This is not optional and
   it is the difference between a spec an engine can execute and one that names things that do not
   exist. For each file in the record's `files[]`:
   - **Modified files:** open them. Get the real function names, signatures, struct fields and
     surrounding conventions. A task that says "update `parse_config`" when the function is called
     `load_config` costs a full implement→test→fix cycle to discover.
   - **New files:** read an existing sibling of the same kind, so the task describes the project's
     established pattern rather than a generic one.
   - **If a file named as modified does not exist**, the record is wrong. Stop and say which file,
     rather than emitting a task against a path that is not there. If a file named as new already
     exists, say so — the block may be re-treading work that landed.
   - Read only what the named files and their immediate siblings require. Do not load the codebase.

   Line numbers move between authoring and execution: name **symbols**, not line numbers, in every
   task you write.

6. THINK HARD about correct scope:
   - Do not invent work beyond what the block defines.
   - Size tasks to roughly 21 hours spread across Mon/Wed/Fri sessions.
   - Enforce **the project's standing rules** as written in `CLAUDE.md` — do not assume any stack, locale-parity, or content-layout rule unless written there. Every task must leave the project's gated checks (`planning/harness.json` → `validation.checks[]` with `gates: true`) passing.
   - **Compilable task boundaries (outranks decomposition preferences under the sequential engines).**
     `/sdlc-flow` and `/sdlc-task` run every task **sequentially on one branch/worktree with no
     inter-task merge step** — `sdlc-flow.js`'s own header says so explicitly ("sequential tasks (no
     inter-task merge conflicts)") — and both gate the project's checks after **every single task**
     (the `runTests()` call inside each engine's per-task loop: `sdlc-flow.js`'s and `sdlc-task.js`'s
     `test-${taskNum}-${attempt}` gate). Under those two engines **every task must leave the gating
     suite passing** — for a compiled or type-checked stack that means the repository must compile
     (and typecheck) at every task boundary, not just at the end of the spec. When a single logical
     change cannot be split without leaving an intermediate task non-compiling — a renamed public
     type, a struct's changed fields, an altered trait/interface signature, and every call site each
     one touches — do **not** split it across tasks to satisfy disjoint ownership below. Put the whole
     change in **one** task instead. **This constraint outranks the disjoint-file-ownership rule
     whenever the two conflict: the tasks merge, not the constraint.** Since this command decomposes
     before the consuming engine is chosen (the recommendation is step 10, after decomposition), apply
     this constraint by default unless the block is already known to run under `/sdlc-block` (e.g. it
     is one block of a multi-block roadmap being decomposed by `/sdlc-block` itself) — in that case the
     disjoint-ownership rule below governs instead.
   - **Disjoint file ownership (parallel-merge safety) — `/sdlc-block` only.** This rule is scoped to
     `/sdlc-block`'s parallel-merge model, where each task runs as its own pipeline and the pipelines
     merge independently; it does **not** apply under `/sdlc-flow` or `/sdlc-task` (see the compilable
     task boundaries rule above, which governs task boundaries there instead — including when it
     requires two tasks that would otherwise be disjoint to merge into one). Under `/sdlc-block`: a
     block's tasks run as parallel pipelines that merge independently, so two tasks editing the same
     existing file collide at merge. Decompose so each task **owns a distinct set of files**. When two
     tasks would touch the same file, either (a) make one `dependsOn` the other so `/sdlc-block`
     serializes them into different waves, or (b) restrict the shared file to **append-only** edits
     (the block engine union-merges files declared `additiveFiles`). Name each task's primary files in
     its step so the dependency analysis can see the boundaries — an undeclared overlap escalates the
     whole block on a merge conflict.
   - Foundational steps come first; the final step is always Validate.
   - **Write the task list as `tasks.json`, not markdown headings.** Every SDLC engine reads
     `planning/<spec-slug>/tasks.json` directly — a **bare array** of `{task_id, title, description,
     acceptance_criteria, validation_commands, max_attempts, files, dependsOn}` objects (see Output
     Format below), the same shape orchestrator's `SDLC_FLOW` workflow already consumes
     (`app/schemas/sdlc_schema.py`'s `SDLCTask`) — instead of parsing `tasks.md` for a heading
     pattern. `tasks.md` still carries the prose (Goal, Context Pointers, Acceptance Criteria,
     Validation Commands, Notes, Amendment Log) but the Step-by-Step Tasks section in it is just a
     one-line pointer at the JSON file, not the task list itself.
   - **This command's `--from` mode is one of two derivation surfaces that both author `tasks.json`
     from a `tasks.md`/block source — the other is each engine's own D16 preflight.** `sdlc-task.js`,
     `sdlc-flow.js`, and `sdlc-run.js` now derive `tasks.json` from an existing `tasks.md` themselves
     (rather than aborting with "No tasks.json (D16)") when a spec ships prose-only, using the same
     author-a-fresh-decomposition discipline this step describes — they are a recovery backstop for
     an already-written spec, not a substitute for running this command up front.

7. Create the directory `planning/<spec-slug>/` if it does not exist, then write
   `planning/<spec-slug>/tasks.json` (the task list) using the Output Format below.

   **Then render the prose view — do not author it (D65).**
   `python3 scripts/render_spec.py <BlockID>` writes `planning/<BlockID>/tasks.md` from the block
   record at `planning/blocks/<BlockID>.json`. The engines read that file as the spec document
   (`sdlc-task.js` sets `specFile = <blockDir>/tasks.md`), so it must exist — but it is
   **generated**. Never hand-write or hand-edit it: change the block record and re-render, or the
   two copies drift within a week. The renderer preserves an existing Amendment Log section, so
   re-rendering mid-run is safe.

   If no block record exists for this spec (a legacy directory predating D65), fall back to
   authoring `tasks.md` from the Output Format below, and say so in the report — a spec with no
   block record has no durable statement of *why* it exists, which is the gap D65 closes.

8. **Property self-check (before committing).** A structurally valid spec can still be substantively
   thin and waste pipeline tokens. Re-read what you just wrote and confirm every required property
   holds; **revise the spec in place** if any fails, then re-check:
   - **`tasks.json` parses as valid JSON** and is a non-empty array (not wrapped in an object —
     orchestrator's `LoadTaskStateNode` expects a bare array).
   - **Every task except the final Validate task names ≥1 file** in its `files[]` (so the dependency
     analysis, the `/sdlc-block` disjoint-ownership guard, and the compilable-boundary review below
     can see boundaries). Under the sequential engines this does **not** imply the named files must be
     disjoint *across* tasks — two tasks are free to touch the same file there, since there is no
     inter-task merge to collide. This property and the compilable-boundary check below do not
     contradict each other: naming files is about visibility for both guards, not about which guard's
     ownership rule applies.
   - **Compilable task boundaries — can fail.** Check whether any single breaking public-surface
     change (a renamed public type, a struct's changed fields, an altered trait/interface signature)
     is split across two or more tasks such that an intermediate task would leave the repository
     non-compiling. If it is, this check **fails**: merge those tasks into one before proceeding, per
     the compilable task boundaries rule in step 6, then re-run this self-check. (This still applies
     even when the block is known to run under `/sdlc-block`: that engine's disjoint-ownership rule
     governs *which* files a task owns, but a task that cannot compile on its own is never a valid
     task under any engine.)
   - **`dependsOn` ids are all valid** — every id referenced exists as some task's `task_id` in the
     same array, and the final Validate task depends on every other task's id.
   - **Acceptance Criteria are non-empty and observable** — each criterion can be judged true/false.
   - **Un-gateable criteria are declared (D64) — can fail.** This repo's checks are all in-repo and
     in-language (`node --check` plus a set of Python scripts here; `cargo fmt`/`clippy`/`nextest`/
     `build` for a Rust repo) and structurally cannot observe evidence living outside that boundary.
     Apply this mechanical test to **every** Acceptance Criterion, keyed on *where the criterion's
     evidence lives* — never on how important or risky it feels:

     | Evidence location | Verdict |
     |---|---|
     | this repo, this language, observable in-process | **gated** — say nothing further |
     | another process (an external CLI, e.g. `gh`), another repo (a sibling git index), a generated
       artifact (e.g. a `status.md` a tool emits), or an **installed artefact** (the binary/distributed
       copy the fleet runs, as opposed to the source tree the checks compile) | **declare it** |

     Any criterion whose evidence lives in another process, another repo, a generated artifact, or
     an installed artefact, and carries neither a named failing command nor a dedicated
     fixture-evidence task in `tasks.json` (a task whose `acceptance_criteria` name the concrete
     fixture standing in for the missing gate — a retro-fixture against a known-bad instance, or a
     corpus sweep), fails this check: revise the
     spec in place — declare the criterion explicitly and add the evidence task — then re-run this
     self-check. **`tasksPassed` is evidence of gate agreement, not correctness** — a green suite is
     never itself the evidence for an un-gateable criterion. Ordinary criteria ("the function
     returns X", "the diagnostic fires", "the field validates") resolve to the first row instantly,
     need no ceremony, and get no added step — this rule must stay quiet on the common case or it
     destroys the lean lane. A verification task that shells out to an installed binary (`mev`,
     `bastion`, or similar) must state explicitly whether it is checking **source** or **installed**
     behaviour — the two diverge, and the divergence is invisible unless named.
   - **Validation Commands are present** (or `planning/harness.json` → `validation.checks[]` supplies
     them as the fallback).
   - **No leftover template sentinels** — no `{{TOKEN}}`, no literal seed strings the Output Format
     ships (`<placeholder>`-style angle stubs left unfilled, empty AC/Validation bullets, or a
     `tasks.json` task still reading `<Foundational step>`). Do **not** treat legitimate `<...>` in
     code/prose (e.g. `Vec<T>`, "the `<concept>` folder") or a bare `TODO`/`TBD` inside authored
     content as a sentinel.

9. **Decomposition assessment.** Before reporting, evaluate each task you just wrote against the
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

10. **Pipeline recommendation.** After writing the tasks, recommend the run command that fits this
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
   isolation). If `breakdown.mode` is `auto` and any tasks were flagged in step 9, note that breakdown
   must run first and the recommendation applies to each resulting sub-spec, not this spec directly.

11. **Commit the spec — after the self-check, the assessment and the recommendation, not before.**
    Steps 8–10 can each require revising the spec in place, so committing earlier means committing
    a draft and amending it. Leave the working tree clean so a downstream `/sdlc-block` run never
    trips its clean-tree merge guard (an uncommitted `tasks.md`/`tasks.json` blocks every merge):
    ```bash
    git add planning/<spec-slug>/
    git commit -m "chore: add spec for <spec-slug>"
    ```
    (Use the slug resolved in step 2 — the master-plan directory slug, or in `--from` mode the
    source file's parent directory. The `git add` stages the source block file too, which is fine.)

12. **Report — and name the command step 10 actually chose.** Do not hardcode a next step; the
    pipeline recommendation is the answer, and `/breakdown` is only the next step when step 9
    flagged a task for it.

    ```
    planning/<spec-slug>/tasks.json    <N> tasks
    planning/<spec-slug>/tasks.md      <rendered from block record | authored (legacy)>

    Source files read: <count> (<any that were named but missing>)
    Un-gateable criteria declared: <n, or none>
    Operator/approval edges on this block: <list, or none — these stall the run>
    Breakdown candidates: <task numbers + one-line reason, or none>

    Run:  <the single recommended engine command>   — <one-line reason>
    <optionally: plus /sdlc-task <N> in isolation — <why>>
    <if any breakdown candidates and breakdown.mode is auto:
     First: /breakdown planning/<spec-slug>/tasks.md — the recommendation applies to each
     resulting sub-spec, not this spec directly.>
    ```

## Session boundary

**`/breakdown` runs in this session** if you flagged a task for it — it reads the same spec and the
same source, and it now writes executable corrections back to `tasks.json`, which wants one writer.

**The engine runs fresh.** `/sdlc-task` and `/sdlc-flow` spawn their own agent stack and are a
different kind of work; carrying an authoring context into them buys nothing and costs room.

**One block per session.** Do not decompose the next block here, even when it looks obvious. The
next block's tasks depend on this block's code, which does not exist yet.

Close by telling the operator:

```
Spec written: planning/<spec-slug>/tasks.json (+ rendered tasks.md)

<If a task was flagged for breakdown:>
  Running /breakdown in this session first.

Start a FRESH session and run:
  <the recommended engine command>       — <one-line reason>

Then come back for the next block in a new session:
  /generate-tasks <next block ID>

<If the block carries an operator or approval edge:>
  This block cannot run to completion until <edge> clears. It will stall.
```

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
<the project's PER-TASK validation commands — one line per `planning/harness.json` → `validation.checks[]` entry (or CLAUDE.md if harness.json has none), in order. For each check that has a `fastCommand`, use `fastCommand` here, not `command` — this block is what every non-final task in this spec runs for its scoped, fast signal. The final Validate task below restates the full authoritative `command` for each check separately; the two are NOT the same list when any check defines a `fastCommand`.>
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
  { "task_id": "N", "title": "Validate", "description": "Run the FULL validation suite and confirm all pass: <one line per `validation.checks[]` entry using its authoritative `command` — NEVER `fastCommand` — this is the one task in the spec that owns the real, unscoped gate>.", "acceptance_criteria": [], "validation_commands": ["<full `command` per validation.checks[] entry, in order>"], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
]
```
`task_id` — 1-indexed integers, dependency-ordered, no gaps (the `"N"` above is illustrative — use
the real next integer). `title`/`description` — required; `description` holds what a `### N.`
heading's bullets used to hold (bulleted lines in one string are fine). `acceptance_criteria` /
`validation_commands` — `[]` for any task that touches source the project's checks compile or lint;
the spec-level markdown sections stay authoritative for those. **Set it for a task that CANNOT break
the build** — docs-only, config-only, fixture-only — with the cheap commands that actually verify
that task (file exists, frontmatter present, index updated).

**The two engines run an override differently ([D63](../../planning/decisions/D63-per-task-validation-commands-augment-gating.md)) — know which one the spec is targeting:**
- **`/sdlc-flow`** still runs the override commands INSTEAD of the project-wide gating checks for
  that task, so a markdown edit stops paying for a full compile; its end review unconditionally
  re-runs the full gating suite over the integrated tree afterward, so nothing escapes validation.
- **`/sdlc-task` has no end review.** Every `gates:true` harness check's cheap `fastCommand` (or
  `command` if no `fastCommand` is defined) still runs alongside the task's own
  `validation_commands` — the override only ever substitutes for the non-gating portion of the
  harness list. This means `validation_commands` does **not** buy the same skip-the-gates savings
  under `/sdlc-task` that it buys under `/sdlc-flow`; it still avoids the *expensive* authoritative
  form (that only runs once, in `/sdlc-task`'s own terminal reconcile), but a docs-only task cannot
  use it to skip a project's gating checks entirely — under this lean engine, whatever a task's own
  tripwire runs is the only gate that task gets until the terminal reconcile.

In compile-expensive stacks, setting `validation_commands` is still the single cheapest win
available at authoring time for a docs/config/fixture-only task — a docs task in a Rust workspace
can otherwise cost minutes per attempt to validate a paragraph — but under `/sdlc-task` that saving
comes from skipping the expensive `command` form, not the gating checks themselves. Example:
`"validation_commands": ["test -f docs/thing.md", "grep -q '^type:' docs/thing.md", "grep -q 'thing.md' docs/index.md"]`
`max_attempts` — defaults to 3, only set per-task to override. `files` — every task but the final
Validate task needs ≥1 entry. `dependsOn` — ids that must complete first; the final Validate task
depends on every other id.


### State refresh (do not hand-author `state.json`'s `tasks` field)

If this repo has a `planning/state.json`, run `mev emit-state --write` after committing — it derives
`tracks[].blocks[].tasks` (a `{ file, generated, counts }` pointer + status summary, **not** a copy
of the task list — see `docs/state/state-schema.md`) from the `tasks.json` you just wrote. Do not
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
