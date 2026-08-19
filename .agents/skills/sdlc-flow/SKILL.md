---
name: sdlc-flow
description: >
  Run a spec sequentially on one branch (or --worktree) with a per-task test→fix loop, one end review, a docs patch, and a PR
---

=============================================================================
 sdlc-flow — single-branch, single-review, PR-terminating SDLC engine
 =============================================================================

 The default engine for non-trivial feature work. Runs one spec's tasks
 SEQUENTIALLY on a SINGLE shared branch (so there are no inter-task merges to
 conflict — sdlc-block's #1 failure mode), with a per-task test→fix loop, ONE
 consolidated review at the end, a docs patch, and a PR as the terminal step.

 ISOLATION MODE
   Default: a plain branch (<spec>-flow) checked out IN THE MAIN WORKING TREE. No
   sparse-checkout worktree, so a relative planning/ symlink (brain-vaulted repos)
   stays intact. main is left on the branch until the PR merges.
   --worktree: the isolated sparse-checkout worktree under trees/<spec>-flow/ —
   opt in when you need true isolation (e.g. /sdlc-block fans out parallel children).

 A compact, COMMITTED, AUTHORITATIVE state.json + one worklog.md replace the 5×N
 per-stage report files: resume + review + wrap-up read a structured index instead
 of re-reading verbose prose. This inverts the harness's usual "committed report
 files are authoritative, state JSON is gitignored" rule on purpose (see D31).

 USAGE
   /sdlc-flow <spec-slug>                  run every task in the spec, open a PR, stop
   /sdlc-flow <spec-slug> 1-3              scope to a task range (1-3, 1,3,5, 5)
   /sdlc-flow <spec-slug> --auto-merge     merge the PR + clean up on success
   /sdlc-flow <spec-slug> --no-pr          stop after wrap-up; do not create a PR
   /sdlc-flow <spec-slug> --worktree       run in an isolated worktree (default: plain branch)
   /sdlc-flow <spec-slug> --resume         re-attach the branch/worktree, resume from state.json
   /sdlc-flow <spec-slug> --test-depth full  run the FULL gating suite per task (default: fast)

 PIPELINE
   worktree-setup → enumerate (D16 lint) → [resume load] → per-task loop
     → end-review → docs (gated on PASS) → wrap-up(PR)

   Per-task loop (sequential, on the one branch):
     implement → fast-test → (triage → fix/​bail) ×≤3
     One state-commit per task. A triage MAJOR / immediate-bail reason breaks
     straight to wrap-up (draft PR) — it does NOT burn three attempts.

   End-review: ONE review over the integrated tree, fed state.json as the index but
   reading `git diff <prBase>..HEAD` + tasks.md criteria directly + re-running the
   FULL gating suite (authoritative). PASS → docs; FAIL/PARTIAL → triage findings:
   small/localized → bounded fix→test→review (≤2, Opus last); broad → bail.

 COMMIT STRATEGY (crash recovery — everything lands on the branch)
   feat: implement <stem> task N      implement agent (per task)
   fix:  fix pass P for <stem> task N  fix agent (per pass)
   chore: flow state — <label>         state-writer (state.json + worklog.md + checkbox)
   docs: update docs for <spec>        docs agent
   chore: wrap up <spec>               wrap-up agent (status/log/amendment-log)

 MODEL TIERING (the token lever — see the MODEL map below)
   haiku : setup, enumerate, scout/state-load, test, state-writer
   sonnet: implement, fix, review, triage, docs, wrap-up
   opus  : ESCALATION on the FINAL per-task fix pass and the FINAL review attempt

 STATE  (committed — NOT gitignored — at planning/<spec>/sdlc/)
   sdlc-flow-state.json   the authoritative run index (per-task summary/issues/fixes/commit)
   worklog.md             the human-readable trail — one short section per task
 =============================================================================

## Antigravity Execution Guide

When the user asks you to run `/sdlc-flow <spec-slug> [range]`, do NOT run `sdlc-flow.js`. Instead, perform the flow execution yourself:

1. **Worktree Setup**:
   - Create (or re-attach) the one shared worktree at `trees/<spec-slug>-flow` and checkout branch `sdlc-flow/<spec-slug>`.
2. **D16 preflight lint — do not guess the task structure.**
   - If the spec's `tasks.json` already exists, skip to task execution.
   - If it is missing but `tasks.md` has derivable step content, derive a FRESH `tasks.json` from
     `tasks.md`'s step list plus its Acceptance Criteria / Validation Commands sections (a real
     decomposition, never a verbatim copy of the prose). Write it as a BARE ARRAY (D45 shape — not
     the superseded `{"tasks": [...]}` wrapper), each entry `{ task_id, title, description,
     acceptance_criteria, validation_commands, max_attempts, files, dependsOn }` — `task_id` a
     1-indexed integer in dependency order with no gaps, `max_attempts: 3`, and never author
     `status`/`attempt_count` (engine-owned). Commit it on the current branch with an explicit
     pathspec: `git add <tasksJsonFile>`, `git commit -m "chore: derive tasks.json from tasks.md
     (D16 fallback)"`. Log a distinct line — `Derived tasks.json from tasks.md (D16
     derive-from-tasks.md fallback) — <N> task(s), commit <hash>.`
   - **Per-task `validation_commands` scoping** — follow the convention documented at
     `.claude/commands/generate-tasks.md` (search it for "validation_commands"); do not restate the
     rubric in your own words, just apply it: `validation_commands` is `[]` for any task that
     touches source the project's checks compile or lint — those tasks fall back to the
     project-wide harness checks, which are authoritative for them. Set it ONLY for a task that
     CANNOT break the build (docs-only, config-only, fixture-only), with cheap commands that
     actually verify that task (file exists, frontmatter present, index updated). If you DO author
     an override that runs tests, it MUST target that task's own tests specifically — never a
     bare/positional filter that could silently match zero or the wrong tests — and a command
     matching nothing must fail rather than pass. Never hardcode a stack-specific command into
     this; that judgment belongs to whoever derives or authors the task at run time. Match the
     intent of the parallel generator in `sdlc-block.js` ("acceptance_criteria/validation_commands
     can stay `[]` per task").
   - **D63 — pure substitute, unchanged (this engine only, unlike `sdlc-task`'s augment-gating
     semantics):** a task whose `validation_commands` is a non-empty array runs ONLY those commands
     on its per-task tripwire — zero `planning/harness.json` `gates:true` checks — and the end
     review's full gating suite is the backstop that still runs everything at the end.
   - Only if `tasks.md` is also missing, or has no derivable step content, abort: report `ABORTED
     (D16)` and tell the user to run `/generate-tasks <blockId>` to author `tasks.json`, commit,
     then re-run. Deriving from an authored `tasks.md` is not guessing the task structure;
     fabricating one from nothing is what D16 still refuses to do.
3. **Execute Tasks sequentially in the worktree**:
   - For each task in the specified range (or all if not specified):
     - Run `/update-task` to flip status to `In progress` in the worklog and local files.
     - Implement the task following instructions.
     - Run fast validation tests.
     - Fix failures (up to 3 triage/fix attempts).
     - Commit the task state on the branch (`feat: implement <slug> task N`).
4. **Consolidated End-Review**:
   - Once all tasks are complete, run the full validation/test suite.
   - Run the acceptance criteria check.
   - If PASS -> proceed to docs. If FAIL/PARTIAL -> run targeted fix loop.
5. **Docs & Wrap-up**:
   - If PASS, run `/update-docs --patch` to update documentation.
   - Update the status and log.
   - Create a pull request (PR) using git CLI or GitHub CLI (unless `--no-pr` is specified).






