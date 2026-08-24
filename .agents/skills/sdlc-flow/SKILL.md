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
   --worktree: SUSPENDED FLEET-WIDE (D81, 2026-08-23). The engine refuses the flag
   unconditionally and exits before any setup — no override, no environment escape hatch.
   Run on a plain branch instead. The sparse-checkout worktree machinery survives
   intact for when D81 lifts.

 A compact, COMMITTED, AUTHORITATIVE state.json + one worklog.md replace the 5×N
 per-stage report files: resume + review + wrap-up read a structured index instead
 of re-reading verbose prose. This inverts the harness's usual "committed report
 files are authoritative, state JSON is gitignored" rule on purpose (see D31).

 USAGE
   /sdlc-flow <spec-slug>                  run every task in the spec, open a PR, stop
   /sdlc-flow <spec-slug> 1-3              scope to a task range (1-3, 1,3,5, 5)
   /sdlc-flow <spec-slug> --auto-merge     merge the PR + clean up on success
   /sdlc-flow <spec-slug> --no-pr          stop after wrap-up; do not create a PR
   /sdlc-flow <spec-slug> --resume         re-attach the branch, resume from state.json
   (--worktree is refused per D81 -- do not pass it)
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

 POST-COMMIT WORK ASSERTION (D81 lift condition 2 —
 BT.ticket.a-run-must-prove-its-commits-contain-the-work) — the COMMIT-SAFETY GUARD above only
 catches a TOTALLY empty index; it does NOT catch a commit whose index is non-empty but whose
 content is still wrong — e.g. many undeclared deletions with one surviving file (measured live:
 EN.11.O, 443 files changed, 177,867 deletions, zero insertions, and it PASSED the guard above).
 Run this immediately AFTER the PER-TASK work commit in step 3's loop (never before — it reads the
 commit it is checking), chained with `&&` onto the commit itself, substituting the real task id
 for `<task-id>` and the real tasks.json path for `<tasks-json-path>`:
   NAME_STATUS=$(git diff --name-status HEAD~1 HEAD); if [ -z "$NAME_STATUS" ]; then echo "WORK_ASSERTION_ABORT: task <task-id> commit diff is EMPTY (condition 1) - no work was committed"; exit 1; fi; WA_DECLARED=$(python3 -c "
import json
d = json.load(open('<tasks-json-path>'))
t = [x for x in d if x.get('task_id') == <task-id>]
print(chr(10).join(t[0].get('files', []) if t else []))
"); WA_MATCH=0; WA_BADDEL=""; while IFS=$'\t' read -r WA_ST WA_P1 WA_P2; do WA_CHK="$WA_P1"; case "$WA_ST" in R*) WA_CHK="$WA_P2" ;; esac; if printf '%s\n' "$WA_DECLARED" | grep -qFx "$WA_CHK"; then WA_MATCH=1; else case "$WA_ST" in D*) WA_BADDEL="$WA_CHK" ;; esac; fi; done <<< "$NAME_STATUS"; if [ "$WA_MATCH" -eq 0 ]; then echo "WORK_ASSERTION_ABORT: task <task-id> commit's changed paths do not intersect declared files[] (condition 2) - declared: [$WA_DECLARED] - changed: [$NAME_STATUS]"; exit 1; fi; if [ -n "$WA_BADDEL" ]; then echo "WORK_ASSERTION_ABORT: task <task-id> commit deletes undeclared file '$WA_BADDEL' not present in files[] (condition 3) - declared: [$WA_DECLARED]"; exit 1; fi
 It aborts (WORK_ASSERTION_ABORT, nonzero exit) when: (1) the commit's diff is empty; (2) no
 changed path matches the task's declared `files[]`; (3) the commit DELETES a path that is NOT in
 `files[]` (the EN.11.O shape — undeclared/collateral deletion). Deleting a file the task DID
 declare is fine and passes. If this prints WORK_ASSERTION_ABORT, treat the task as FAILED —
 investigate, fix, and re-commit; do not report success. EXEMPT (never run this check at these
 sites): the worktree-init commit, the D16 `chore: derive tasks.json ...` fallback commit, the
 consolidated review-fix commit and the docs commit (neither is scoped to one task's `files[]`),
 and the vault commit — the vault commits into a different repo whose own HEAD~1 and
 `planning/`-prefixed paths this check does not attempt to reconcile, and which other concurrent
 lanes also write to.

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

1. **Setup — plain branch only**:
   - **`--worktree` is REFUSED (D81 worktree moratorium, suspended fleet-wide as of 2026-08-23).** If
     the invocation includes `--worktree`, stop immediately: report that --worktree is suspended per
     D81 and the run must use a plain branch (drop the flag and re-invoke). Do NOT create a worktree,
     a branch, or any commit. This mirrors the real engine, which refuses unconditionally right after
     parsing the flag, before any setup — see `.claude/workflows/sdlc-flow.js` around the
     `useWorktree = hasFlag('--worktree')` line. No override flag, no environment escape hatch.
   - Otherwise (the normal path today): check out branch `sdlc-flow/<spec-slug>` IN THE MAIN WORKING
     TREE — no sparse-checkout worktree, so a relative `planning/` symlink (brain-vaulted repos) stays
     intact. (The sparse-checkout worktree recipe under `trees/<spec-slug>-flow/` is left intact in
     the machinery for when D81 lifts; it is not the normal path today.)
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
     - Immediately after that commit, run the POST-COMMIT WORK ASSERTION above (`&&`-joined onto
       the commit). A `WORK_ASSERTION_ABORT` means the commit did not actually contain the task's
       declared work — treat the task as failed and fix/re-commit before proceeding.
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






