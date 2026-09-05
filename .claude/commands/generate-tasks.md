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
     so `/sdlc-flow <slug>` can run it. If `<path>` does not exist, stop and say
     so. Then resolve which block to read:
     - If a `phaseN-blockX` selector follows `--from <path>` (accept any of `phase0-blockA`,
       `phase0blockA`, `0-A`, `Phase 0 Block A`), that names the block to decompose.
     - If no selector is given, inspect `<path>`: a **single standalone block file** (no `## Phase` /
       `### Block` headings — legacy D34) is decomposed whole; a **master-plan-format file** with
       exactly one block defaults to that block; a master-plan-format file with **more than one
       block** has no safe default — STOP, list the blocks, and ask which one (plan-quality floor:
       never guess a load-bearing target). To run the whole multi-block plan instead, point the user
       at `/orchestrate <path>`.
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
     **non-interactive / preflight context** (invoked by `/orchestrate` / `/sdlc-flow` to auto-generate
     a missing spec), **ABORT with a specific message naming exactly what's missing** so the human can
     fix the block. This is the proactive complement to the D19 thin-spec abort: D19 catches a thin
     spec after the fact; this prevents writing a confidently-wrong one in the first place.

5a. **Normalise every path to the REPO ROOT before you copy it into a task's `files[]`.** A block
   record writes paths **fleet-root-relative** (`core/okf-core/src/doc/learning_artifact.rs`); the
   engines' work assertion compares against `git diff --name-status HEAD~1 HEAD` run **inside the
   repo**, which emits `src/doc/learning_artifact.rs`. The comparison is `grep -qFx` — an exact
   whole-line match — so a verbatim copy never intersects and the task is a guaranteed
   `WORK_ASSERTION_ABORT`.

   **Strip the repo prefix when the block's repo matches the target repo.** If the record says
   `core/okf-core/src/foo.rs` and you are authoring for `okf-core`, the task's `files[]` entry is
   `src/foo.rs`. Verify rather than assume: run `git diff --name-status HEAD~1 HEAD` in the target
   repo and confirm the form matches what you are about to write.

   Measured 2026-09-04: `OK.ticket.learning-artifact-missing-title-description` task 1 cost a full
   attempt to this before the engine self-corrected the path mid-run; the block record still
   carries the fleet-root form.

5b. **Read the actual source the block names — before writing any task.** This is not optional and
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
   - Bound this read to what `files[]` and their immediate siblings require — it is not a licence
     to load the codebase.

   Line numbers move between authoring and execution: name **symbols**, not line numbers, in every
   task you write.

   **Re-verify the record's premises too, not only `files[]`.** A block record's premises live in
   three places — `files[]`, `acceptance_criteria`, and any design document it cites — and reading
   only the first lets the other two expire silently between authoring and execution. Before
   writing any task:
   - **Re-verify every acceptance criterion against the current tree.** Use this triage test to
     keep it cheap: a criterion naming a path, symbol, or count that this block does **not itself
     modify** is the one nothing else forces anyone to read — check those against the tree now. A
     criterion about the code the block is already changing gets re-read anyway by the `files[]`
     pass above, so it needs no separate check. Concrete instance: a criterion demanding
     `rg OperatorTransport crates/engine-core` stay empty was authored true and became false the
     same day when a sibling block in the same lane deliberately put that trait there — a
     files[]-scoped read could never reach it, because the criterion named a path this block does
     not touch.
   - **Re-read any design document the record cites** in `related` or names in `what`/`why`. Why:
     the next block's author reads the cited design doc, not another block's amendment log — a
     correction recorded only in a sibling block's amendment log reaches nobody who reads the doc
     instead.
   - **On a mismatch, amend the record and say so (D18) — never silently reword the criterion, and
     never delete merged sibling work to satisfy a stale one.** A criterion that now contradicts
     landed work is evidence the record is stale, not evidence the landed work is wrong.

6. THINK HARD about correct scope:
   - Do not invent work beyond what the block defines.
   - Size tasks to roughly 21 hours spread across Mon/Wed/Fri sessions.
   - Enforce **the project's standing rules** as written in `CLAUDE.md` — do not assume any stack, locale-parity, or content-layout rule unless written there. Every task must leave the project's gated checks (`planning/harness.json` → `validation.checks[]` with `gates: true`) passing.
   - **Compilable task boundaries.** `/sdlc-flow` and `/sdlc-task` — the only two engines this
     command feeds — run every task **sequentially on one branch/worktree with no inter-task merge
     step** — `sdlc-flow.js`'s own header says so explicitly ("sequential tasks (no inter-task merge
     conflicts)") — and both gate the project's checks after **every single task** (the
     `runTests()` call inside each engine's per-task loop: `sdlc-flow.js`'s and `sdlc-task.js`'s
     `test-${taskNum}-${attempt}` gate). Under both engines **every task must leave the gating
     suite passing** — for a compiled or type-checked stack that means the repository must compile
     (and typecheck) at every task boundary, not just at the end of the spec. When a single logical
     change cannot be split without leaving an intermediate task non-compiling — a renamed public
     type, a struct's changed fields, an altered trait/interface signature, and every call site each
     one touches — do **not** split it across tasks. Put the whole change in **one** task instead.
     This applies unconditionally: both engines are sequential, so there is no parallel-merge model
     to weigh it against.
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
     from a `tasks.md`/block source — the other is each engine's own D16 preflight.** `sdlc-task.js`
     and `sdlc-flow.js` now derive `tasks.json` from an existing `tasks.md` themselves (rather than
     aborting with "No tasks.json (D16)") when a spec ships prose-only, using the same
     author-a-fresh-decomposition discipline this step describes — they are a recovery backstop for
     an already-written spec, not a substitute for running this command up front.

7. Create the directory `planning/<spec-slug>/` if it does not exist, then write
   `planning/<spec-slug>/tasks.json` (the task list) using the Output Format below. When a block
   record exists at `planning/blocks/<BlockID>.json`, that is the whole deliverable — the engines
   read the block record plus `tasks.json` directly (D65 stage 2); do not author or render a
   `tasks.md` for it.

   If no block record exists for this spec (a legacy directory predating D65), fall back to
   authoring `tasks.md` from the Output Format below, and say so in the report — a spec with no
   block record has no durable statement of *why* it exists, which is the gap D65 closes. This is
   the only case in which `/generate-tasks` still produces a `tasks.md`.

8. **Property self-check (before committing).** A structurally valid spec can still be substantively
   thin and waste pipeline tokens. Re-read what you just wrote and confirm every required property
   holds; **revise the spec in place** if any fails, then re-check:
   - **`tasks.json` parses as valid JSON** and is a non-empty array (not wrapped in an object —
     orchestrator's `LoadTaskStateNode` expects a bare array).
   - **Every task names ≥1 file** in its `files[]` — including the last one; there is no exemption
     for a validation task (see "NEVER give the final task `files: []`" below) (so the dependency
     analysis and the gate-passing boundary review below can see boundaries). This does **not**
     imply the named files must be disjoint *across* tasks — two tasks are free to touch the same
     file under the sequential engines, since there is no inter-task merge to collide. This property
     and the gate-passing boundary check below do not contradict each other: naming files is about
     visibility, not ownership.
   - **Gate-passing task boundaries — can fail.** The bar is **the project's gating suite passing**
     (`planning/harness.json` → `validation.checks[]` with `gates: true`) at every task boundary —
     not merely "it compiles". Compiling is one stack's instance of that bar and never the whole of
     it: a task can compile fine and still leave `fmt`, `clippy -D warnings`, a lint, a type-check,
     a schema check or the test suite red, and both engines run the full gating suite after every
     single task, so such a task fails its gate and burns a fix loop. Check whether any single
     change is split across two or more tasks such that an intermediate task would leave any gated
     check failing — a renamed public type, a struct's changed fields, an altered trait/interface
     signature and every call site each one touches; a lint that only passes once the old code path
     is deleted; a test updated in one task for behaviour that lands in the next. If so, this check
     **fails**: merge those tasks into one before proceeding, per the compilable task boundaries
     rule in step 6, then re-run this self-check — a task that cannot pass the gate on its own is
     never valid, under either engine.
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
   - **Every `expect_red` entry is a subset of that same task's own `validation_commands`, and never
     names a project-wide harness check.** A task with no `expect_red` field is unaffected by this
     rule and needs no further check. Where `expect_red` is set, confirm each entry string also
     appears, verbatim, in that task's own `validation_commands` array — an entry that does not is a
     spec error, revise it in place — and confirm no entry names one of
     `planning/harness.json` → `validation.checks[]`'s `gates: true` commands; `expect_red` may only
     invert a command the task itself declared, never a harness gate shared by every concurrent lane.
   - **No task's `files[]` names a path under `planning/` — can fail.** `planning/` is a symlink
     into the private HQ vault, excluded from this repo's git by `base-template/.gitignore:20` (the
     bare rule `/planning`). Code that references such a path — an `include_str!`, a fixture path,
     a test data file — compiles on every developer machine, because the vault checkout is present
     locally, and on no CI runner, because a **CI checkout cannot see the private vault**: every
     local gate passes and the build fails only in CI, where nothing reachable from the developer's
     machine could have caught it. If a task's `files[]` names such a path, revise the spec in
     place — put the fixture or test data under `tests/` instead — then re-run this self-check.
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
   one of the single-spec engines; `/orchestrate` is named only to redirect when the block belongs to a
   multi-block roadmap.

   - **`/patch`** — trivial, single-file hotfix with no new tests. Not produced by this command (a
     spec implies enough scope to decompose), so name it only to redirect when the "spec" turns out to
     be a one-line fix.
   - **lean `/sdlc-task <spec-slug> [range]`** — one small unit of behavior change: a handful of
     tightly-coupled tasks that want a fast test→fix loop but no review / docs / PR ceremony. The
     cheapest real engine and the natural runner for `/ticket` and `/chore` outputs. In-place by
     default; `--worktree` to isolate.
   - **`/sdlc-flow <spec-slug>`** (default for non-trivial feature work) — one whole spec in a
     dedicated worktree terminating in a PR: sequential tasks (no inter-task merge conflicts), per-task
     test→fix loop (≤3 attempts, Opus escalation), one consolidated end review over the integrated
     tree. Use when the work has many moving parts or a reviewable PR is wanted. `--auto-merge` to
     merge + clean the worktree on a clean PASS; `--no-pr` to stop after wrap-up; `--resume` to
     re-attach after an interruption.
   - **`/orchestrate`** — the rung *above* a single spec: a multi-block roadmap. If this block is
     one of several in `planning/master-plan.md` or a `/plan` output, drive the whole roadmap with
     `/orchestrate` instead of running this one block alone — it runs `/generate-tasks` and the right
     single-spec engine per block, in dependency order, in one session.
   - **`/sdlc-task <spec-slug> <N>`** — not a strategy for the whole spec; name it only when the right
     move is one specific task in isolation (a high-risk surgical change, or resuming after a failure on
     task N). Say which task number and why isolation matters.

   Recommend exactly one primary command (optionally plus `/sdlc-task <N>` when a single task warrants
   isolation). If `breakdown.mode` is `auto` and any tasks were flagged in step 9, note that breakdown
   must run first and the recommendation applies to each resulting sub-spec, not this spec directly.

11. **Commit the spec — after the self-check, the assessment and the recommendation, not before.**
    Steps 8–10 can each require revising the spec in place, so committing earlier means committing
    a draft and amending it. Leave the working tree clean so a downstream `/orchestrate` run never
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
    planning/<spec-slug>/tasks.md      <omitted (block record) | authored (legacy)>

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

## Measured pitfalls — every one of these cost real time in a real run

From the `clean-slate-sandbox` build lane, 2026-09-03/04: **16 engine launches for 9 blocks (a 1.8x
relaunch multiplier), and roughly a third of the total ~11 hours went to spec defects the authoring
agent introduced and then had to diagnose.** Each item below is one of those defects, with what it
cost. They are ordered by cost.

### 1. Never put `expect_red` on a command that is a `gates: true` harness row — 40 min, circular

`expect_red` inverts only the command *the task* declared. The **harness** copy of the same command
is not inverted, so a test file already gated in `planning/harness.json` goes red on committed code,
and then **every later task's gating check fails — including the task whose fix would turn it
green.** The block cannot proceed and a retry cannot help.

Measured: `test_build_v2.sh` was gated in one block, then a later block put `expect_red` on it. Cost
was two relaunches plus a manual un-gate/re-gate cycle.

**Rule: once a test file is gated, you can never again pin a NEW red case in that same file.** Stage
new red cases in a separate, ungated file and gate it only after it is green. Better still, prefer
the runtime inversion in item 6.

### 2. Read every acceptance criterion against the others before committing the spec — 77 min

The most expensive single defect in the run was two criteria in one block that **could not both
hold**: one demanded zero pruned decision-citations; the block's own `out_of_scope` and a ratified
decision *accepted* pruning an ~45-slug tail, which is decision-citations. No implementation
satisfies both, so the engine burned a full cycle proving the code was right and the spec was wrong.

A second instance in the same run: a control matrix planted a banned term, ran a full build, and
asserted the *scan* reported it — but sanitize runs before scan and shares the term list, so correct
code redacts the term and the scan correctly finds nothing.

**Rule: before committing, read the criteria as a set and ask which PAIR cannot both be true.** This
is a two-minute check that has twice cost over an hour when skipped.

### 3. Never freeze a count in a criterion — 3 instances in one run

"Exactly 8 items", "all 30 links", "the 11 slugs" all rot on the same clock as the thing they count.
Measured: a seed set that a prose table put at 11 measured **2** on the live tree; a spike's
"30 markdown links, 7 file URIs" was stale; a `public repos walked (8 items)` assertion breaks the
moment a repo is added.

**Rule: assert a DERIVED value — "N == the manifest's repo count" — never a literal.** The test that
proves a derivation is real: add an entry to the config alone, with zero code edits, and observe the
behaviour change. A literal that happens to be right today passes every other test.

### 4. A final validation-only task (`files: []`) cannot satisfy the work-assertion gate — 9 of 10 blocks

A task that declares `files: []` changes nothing, so it produces no commit, and the terminal write
recipe requires a positive commit-derived `workAssertionPassed`. **The block reports `bailed: true`
with every substantive task passed.** Cost was ~5 min of engine time per block plus a manual gate
run, on nine of ten blocks — and any consumer counting bails from these records over-counts by one
per block.

**Until the engine accepts non-commit evidence, expect this and plan for it:** either fold the final
validation into the last task that produces a diff, or accept that the driving agent runs the gates
by hand and closes the block on the measured result.

### 5. A task whose files are gitignored also produces no diff — 1 block

Same gate, different cause, and it is not obvious: a task that moves or edits paths **outside this
repo's git index** (a gitignored sub-repo, a relocation to another directory) does real work that
git cannot see. It bails identically.

**Rule: merge such a task into one that touches a tracked file**, so there is a real diff to assert
on. In the measured case, merging the move into the config edit that accompanied it produced the
run's only `bailed: false` completion.

### 6. Prefer a RUNTIME inversion over a committed red baseline

D68 asks that a case be shown failing before its fix lands. A committed red case does that but
leaves the suite red at that commit — which is item 1's whole problem. A runtime inversion proves
more and costs nothing: **inside the test, break the precondition, assert the failure, restore it,
assert the pass.**

Measured example: to prove a pruner runs *after* an assertion, the test deletes a seed source,
asserts the build stops **and that the pruner was provably never invoked**, then restores it. That
also demonstrates the second rule of ordering tests — **observe whether the later step was CALLED,
not what the tree looks like afterwards.** A final-state check passes against the broken order,
because a pruned tree and a correct tree both validate clean.

### 7. Write the fleet's git prohibitions into the TASK, not only into `CLAUDE.md`

An engine subagent works from the task spec. A rule that lives only in `AGENTS.md` is a rule the
actor never reads.

Measured: a subagent ran a bare `git stash` to set aside ONE file. `git stash` takes no pathspec in
that form, so it swept **16 files across three other lanes' uncommitted work.** Its follow-up
`git stash pop` would have restored a *different session's* stash into the tree.

**Rule: in any repo where one git index owns multiple projects' `planning/` directories, put
"never `git stash` / `git add -A` / `git add .` / `git reset`" in the task description**, with the
positive alternative: build a fixture tree under `mktemp -d`; never hide a real file.

### 8. Anchor every grep in a criterion, and positive-control every empty result

Three separate false results in one run:

- **Unanchored substring match.** `grep -c 'doc_id' README.md | grep -q '^0$'` was meant as "no
  `doc_id` frontmatter field", but matched a prose comment explaining why the file has no `doc_id`.
  The file could never satisfy it. Use `! grep -q '^doc_id:'`.
- **An empty grep over a deleted directory.** Once a path is removed, grepping it for a pattern
  returns empty *for the wrong reason* — indistinguishable from a successful rewrite. **Assert the
  directory is absent (`test ! -d`), not that a grep came back clean.**
- **A broken instrument.** On macOS BSD grep, `grep -rln 'pattern' --include='*.yml' .` returned
  **0** while a direct grep on one of those exact files returned the line. Use
  `find . -name '*.yml' | xargs grep -l`.

**Rule: any criterion asserting an ABSENCE must carry a positive control that HITS** — the identical
command, run where a match is known to exist. And the control must contain the thing being searched
for; an empty control and an empty claim look identical.

### 9. Tasks are INDEPENDENT by design — pay the re-read, shrink it, never remove it

Measured file-read volume per block: **605 KB to 1.3 MB**, spread over 5-10 tasks, with individual
mid-chain tasks reading up to **434 KB**. The final validation tasks read almost nothing
(0.4-18 KB), so the cost is *not* at the end of the chain — it is in the middle, and it is
re-reading.

The cause is structural and **deliberate**: every task is a fresh subagent with no memory of the
previous one, so task 5 re-reads whatever tasks 1-4 wrote. In a six-task block the same files can be
read five times.

**Do not "fix" this by consolidating tasks, and do not reintroduce inter-task reports.** Both are
tempting and both are wrong:

- **Independence is the property being bought.** A task that depends on another task's in-context
  knowledge cannot be resumed alone, cannot be re-run after a bail, and hides coupling the per-task
  gate is supposed to expose. The re-read is what makes every task independently executable.
- **Hand-off reports were tried in this harness and removed.** They cost a large number of extra
  tokens on every task and were only sometimes read by the task that received them — so they were
  paid for unconditionally and used occasionally. Do not propose them again.

**So the lever is precision, not consolidation.** Make each task's read small and targeted:

- **`files[]` is a reading list, so make it exact.** Every path the task must open, and none it does
  not. A vague or over-broad `files[]` is a direct instruction to read more than necessary.
- **Name symbols and line numbers in the `description`**, so the agent opens the right file at the
  right place instead of grepping to orient. `apply_scan (build-v2.sh:809)` costs one read;
  "the scan step" costs a search.
- **Carry forward the decisions, not the context.** If task 5 needs to know why task 2 chose
  something, put that conclusion in task 5's description. One sentence in the spec replaces a file
  read, and unlike a hand-off report it is written once and always present.
- **Split on the gating boundary, never finer.** Add a task when an intermediate state would fail
  the gates (see compilable task boundaries above). Splitting beyond that adds a full re-read and
  buys nothing.

## Session boundary

**`/breakdown` runs in this session** if you flagged a task for it — it reads the same spec and the
same source, and it now writes executable corrections back to `tasks.json`, which wants one writer.

**The engine runs fresh.** `/sdlc-task` and `/sdlc-flow` spawn their own agent stack and are a
different kind of work; carrying an authoring context into them buys nothing and costs room.

**One block per session.** Do not decompose the next block here, even when it looks obvious. The
next block's tasks depend on this block's code, which does not exist yet.

Close by telling the operator:

```
Spec written: planning/<spec-slug>/tasks.json (+ tasks.md, legacy specs only)

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

## Output Format — files on disk (not the chat reply; see `## Report` below)

`planning/<spec-slug>/tasks.json` is the task list the engines execute against, and the block record
`planning/blocks/<BlockID>.json` carries the prose. **Do not write a `tasks.md`** — it is retired
(`BT.ticket.engines-read-block-record`) and `scripts/render_spec.py` is deleted. The headings below
describe the block record's narrative fields; the engines read them from the record directly.

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
  { "task_id": "N", "title": "<Last real change> — and Validate", "description": "<the last substantive change>, then run the FULL validation suite and confirm all pass: <one line per `validation.checks[]` entry using its authoritative `command` — NEVER `fastCommand` — this task owns the real, unscoped gate>.", "acceptance_criteria": [], "validation_commands": ["<full `command` per validation.checks[] entry, in order>"], "max_attempts": 3, "files": ["<path/to/the/last/real/change>"], "dependsOn": [1, 2] }
]
```
**NEVER give the final task `files: []`.** This template used to, and the engines refuse it: the
terminal write recipe's `renderWorkAssertion` (`.claude/workflows/sdlc-task.js:409`) requires a
positive, **commit-derived** `workAssertionPassed` before it will write `done`/`passed` — and a task
that changes nothing produces no commit. The block then reports `bailed: true` **with every
substantive task passed**, which also means any consumer counting bails from these records
over-counts by one per block.

Measured on the `clean-slate-sandbox` run, 2026-09-03/04: **10 of 11 blocks bailed this way.** The
two that did not were the two whose final task also touched a file.

**Pair the validation with the last real change**, as the template above now shows. It is not a
workaround — the gate genuinely belongs with the change it is gating, and folding it there removed
the bail on every block that tried it.

**This applies to `/plan`-derived and `/generate-roadmap`-derived blocks identically.** `/plan` does
not write `tasks.json` (`plan.md:35`); both paths reach the engines through *this* command's
template, so the shape is decided here, once, for both.

The same failure has a second cause worth knowing: a task whose files are **gitignored** — moving a
sub-repo, relocating a directory outside this repo's index — also produces no diff, and bails
identically despite doing real work. Merge it into a task that touches a tracked file.

`task_id` — 1-indexed integers, dependency-ordered, no gaps (the `"N"` above is illustrative — use
the real next integer). `title`/`description` — required; `description` holds what a `### N.`
heading's bullets used to hold (bulleted lines in one string are fine). `acceptance_criteria` /
`validation_commands` — `[]` for any task that touches source the project's checks compile or lint;
the spec-level markdown sections stay authoritative for those. **Set it for a task that CANNOT break
the build** — docs-only, config-only, fixture-only — with the cheap commands that actually verify
that task (file exists, frontmatter present, index updated).

**A task that writes or edits OKF frontmatter must be told what a legal `related:` target is.** An
engine executing the spec has no independent way to know, and the failure mode is that it invents
one — a carryover slug, a block id, a filename, a plausible-looking id for a doc that does not
exist — which red-gates the **whole corpus** for every concurrent lane the next time `--graph`
gates, not just the authoring repo. So spell the constraint out in the task's `description`, and
back it with a check in that task's `validation_commands`:

- A `related:` entry is a **`doc_id`**, not a filename, a slug, a title, or a block id. The
  `doc_id` is the `doc_id:` field in the target document's own frontmatter (defaulting to its
  filename stem when that field is absent).
- The target must be a **real, existing, crawled document**. Verify it before writing the edge —
  `rg -L -n "^doc_id: <id>$" <repo>` , or confirm the file whose stem is `<id>` exists in the
  corpus. A leading `_` in a filename excludes it from the corpus, so such a target is unresolved
  even though the file is on disk.
- A **cross-repo** target must be qualified `<repo>:<doc_id>` (e.g.
  `base-template:D48-downstream-harness-sync-script`). A bare `doc_id` resolves only inside the
  authoring repo and is treated as unresolved everywhere else.
- When no real target exists, **omit `related:` entirely**. An empty or absent edge list is always
  correct; an invented edge never is.

See `docs/okf-frontmatter.md` for the full schema.

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

**`expect_red`** — optional; omit it entirely for the ordinary case, which is unaffected: a task with
no `expect_red` behaves exactly as it always has, no new ceremony required. Set it only for a task
whose declared deliverable IS a test observed FAILING (D68) — e.g. the first task of a TDD-shaped
spec that writes a fixture and must show it red against the unfixed target before anything fixes it.
Shape: `"expect_red": ["<command>", ...]`, a list of command strings. **Every entry MUST also be
present, verbatim, in that same task's own `validation_commands`** — `expect_red` never introduces a
new command, it only marks an existing one as inverted. For each named command the fast-test stage
inverts the verdict: the check PASSES when the command exits NON-ZERO and FAILS when it exits 0.
`expect_red` can **never** name a project-wide `gates:true` harness check — it is scoped strictly to
commands the task itself declared, so a task cannot use it to invert a check that guards every
concurrent lane in the repo; the harness gating checks still render and still gate normally for a
task carrying `expect_red`. Example:
`{ "task_id": 1, "title": "Write the fixture and observe it failing", "validation_commands": ["python3 scripts/test_new_thing.py"], "expect_red": ["python3 scripts/test_new_thing.py"], ... }`


### State refresh (do not hand-author `state.json`'s `tasks` field)

If this repo has a `planning/state.json`, run `mev emit-state --write` after committing — it derives
`tracks[].blocks[].tasks` (a `{ file, generated, counts }` pointer + status summary, **not** a copy
of the task list — see `docs/state/state-schema.md`) from the `tasks.json` you just wrote. Do not
hand-edit a `tasks` array into `state.json` yourself; that field is derived, same as `focus`. (This
derivation isn't implemented in `mev` yet — running the command is a no-op until it ships; it's
listed here so the step is already in place when it does.)

## Report

**<= 10 lines.** First line: outcome + whether it needs the operator. Then <= 6 one-line
bullets. Link paths; never restate a file. See the `report-to-the-operator` skill.

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
  /sdlc-flow <spec-slug>         — <N> tasks, non-trivial feature work; dedicated worktree, per-task test→fix, one end review, PR (<reason: many moving parts / reviewable PR wanted>)
  /sdlc-flow <spec-slug> --auto-merge
                                 — as above; merge PR + clean worktree on clean PASS
  /orchestrate                   — this block is one of several; drive the whole roadmap in one session
  /sdlc-task <spec-slug> <N>     — run task <N> in isolation; <reason isolation matters here>

Next (optional — decompose first):
  /breakdown planning/<spec-slug>/tasks.md

Next (run directly):
  /<recommended-command> <spec-slug>
```
