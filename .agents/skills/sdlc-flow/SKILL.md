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
   reading `git diff <prBase>..HEAD` + the spec's acceptance criteria directly (the block record's
   `acceptance_criteria` array when D65's block record is the spec source, else `tasks.md`'s
   `## Acceptance Criteria` prose) + re-running the
   FULL gating suite (authoritative). PASS → docs; FAIL/PARTIAL → triage findings:
   small/localized → bounded fix→test→review (≤2, Opus last); broad → bail.

 COMMIT STRATEGY (crash recovery — everything lands on the branch)
   feat: implement <stem> task N      implement agent (per task)
   fix:  fix pass P for <stem> task N  fix agent (per pass)
   chore: flow state — <label>         state-writer (state.json + worklog.md + checkbox)
   docs: update docs for <spec>        docs agent
   chore: wrap up <spec>               wrap-up agent (status/log/amendment-log)

 COMMIT-SAFETY GUARD (BT.ticket.worktree-run-can-commit-an-empty-tree) — run before EVERY `git commit`
 in this pipeline, joined with `&&` in the SAME shell call as the commit (a separate preceding call
 runs in a different process whose inherited git environment may differ, which is the whole failure
 mode this guards against). The one exception is the worktree-init `--allow-empty` commit — its index
 is legitimately populated right after checkout, so the guard cannot fire there. Run the identical
 check against the vault repo (`git -C <vault planning path>` in place of `git`) before any vault
 commit too:
   if git rev-parse --verify -q HEAD >/dev/null; then TRACKED=$(git ls-tree -r HEAD --name-only | wc -l | tr -d ' '); STAGED=$(git ls-files -s | wc -l | tr -d ' '); if [ "$TRACKED" -gt 0 ] && [ "$STAGED" -eq 0 ]; then echo "COMMIT_GUARD_ABORT: index holds 0 entries but HEAD tracks $TRACKED files - refusing to commit a tree that deletes everything (BT.ticket.worktree-run-can-commit-an-empty-tree)"; exit 1; fi; fi
 If this prints COMMIT_GUARD_ABORT, STOP — do not run the commit; the index is empty against a
 non-empty HEAD, which is exactly the shape that deletes every tracked file.

 GIT ENVIRONMENT STRIP (BT.ticket.worktree-run-can-commit-an-empty-tree, half (a)) — git exports
 nine repository-scoping variables to the hooks it runs, and a hook-spawned process inherits them;
 they OVERRIDE `-C` and cwd, so a later `git commit` can silently build its tree from a stale/foreign
 index instead of the one you just staged. Run EVERY git command in this guide — including inside
 `$(...)` substitutions and worktree-setup commands — through this prefix instead of a bare `git`:
   env -u GIT_DIR -u GIT_COMMON_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_NAMESPACE -u GIT_PREFIX -u GIT_CEILING_DIRECTORIES git
 e.g. `git status` becomes `env -u GIT_DIR -u GIT_COMMON_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_NAMESPACE -u GIT_PREFIX -u GIT_CEILING_DIRECTORIES git status`.
 Below, commands are written as plain `git ...` for readability — always run them through this
 prefix; only the prose mentions of git (descriptions, prohibitions) stay bare.

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
   - **Spec location.** Paths are `planning/<spec-slug>/...` at the git root by default. If no spec
     exists there, ALSO check `<invoking-dir-relative-to-root>/planning/<spec-slug>/...` — a
     sub-brain tier (e.g. `business/`) has its own `planning/` without being its own git repo. The
     ROOT always wins when a spec exists at both locations. If found at neither, abort and name BOTH
     paths you searched, not just one.
2. **D16 preflight lint — do not guess the task structure.**
   - If the spec's `tasks.json` already exists, skip to task execution.
   - If it is missing but `tasks.md` has derivable step content, derive a FRESH `tasks.json` from
     `tasks.md`'s step list plus its Acceptance Criteria / Validation Commands sections (a real
     decomposition, never a verbatim copy of the prose). Write it as a BARE ARRAY (D45 shape — not
     the superseded `{"tasks": [...]}` wrapper), each entry `{ task_id, title, description,
     acceptance_criteria, validation_commands, max_attempts, files, dependsOn }` — `task_id` a
     1-indexed integer in dependency order with no gaps, `max_attempts: 3`, and never author
     `status`/`attempt_count` (engine-owned). Commit it on the current branch with an explicit
     pathspec: `git add <tasksJsonFile>`, then run the COMMIT-SAFETY GUARD above and `git commit -m
     "chore: derive tasks.json from tasks.md (D16 fallback)"` as one `&&`-joined call. Log a
     distinct line — `Derived tasks.json from tasks.md (D16
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
     - Run the COMMIT-SAFETY GUARD above, `&&`-joined with the commit itself, then commit the task
       state on the branch (`feat: implement <slug> task N`). If a vault commit is also needed
       (D46), run the same guard against `git -C <vault path>` before that commit too.
4. **Consolidated End-Review**:
   - Once all tasks are complete, run the full validation/test suite.
   - Run the acceptance criteria check.
   - If PASS -> proceed to docs. If FAIL/PARTIAL -> run targeted fix loop.
5. **Docs & Wrap-up**:
   - If PASS, run `/update-docs --patch` to update documentation, running the COMMIT-SAFETY GUARD
     `&&`-joined before the docs commit (and its vault counterpart, if any patched/created doc lives
     under `planning/`).
   - Update the status and log.
   - Run the COMMIT-SAFETY GUARD `&&`-joined before the wrap-up commit — both the repo-local one and,
     in a vaulted repo, the vault one (`git -C <vault path>`) — then commit.
   - Create a pull request (PR) using git CLI or GitHub CLI (unless `--no-pr` is specified).






