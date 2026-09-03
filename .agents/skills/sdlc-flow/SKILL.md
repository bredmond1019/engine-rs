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
   --worktree: creates an isolated sparse-checkout worktree under trees/<spec>-flow/ for
   true isolation. Was suspended fleet-wide 2026-08-23 to 2026-08-28 (D81,
   worktree-moratorium) after three whole-repo-deletion incidents behind a green PASS;
   lifted after BT.ticket.worktree-smoke-fixture verified a real --worktree run end to
   end and confirmed the guards added during the suspension hold (binding/brain-root/
   population guards, the commit-safety guard, the post-commit work assertion).
   Replicating this pipeline by hand: create the worktree only when --worktree was
   explicitly passed.

 A compact, COMMITTED, AUTHORITATIVE state.json + one worklog.md replace the 5×N
 per-stage report files: resume + review + wrap-up read a structured index instead
 of re-reading verbose prose. This inverts the harness's usual "committed report
 files are authoritative, state JSON is gitignored" rule on purpose (see D31).

 USAGE
   /sdlc-flow <spec-slug>                  run every task in the spec, open a PR, stop
   /sdlc-flow <spec-slug> 1-3              scope to a task range (1-3, 1,3,5, 5)
   /sdlc-flow <spec-slug> --auto-merge     merge the PR + clean up on success
   /sdlc-flow <spec-slug> --no-pr          stop after wrap-up; do not create a PR
   /sdlc-flow <spec-slug> --resume         re-attach the branch (or worktree), resume from state.json
   /sdlc-flow <spec-slug> --worktree       run in an isolated trees/<spec>-flow/ checkout
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

 VAULT-ONLY TASKS (D46): if EVERY path in the task's declared files[] begins with `planning/`,
 the work landed in the VAULT repo, not this one, and this repo's history structurally cannot
 contain it — the assertion aborts on condition 1 (empty diff) forever and no retry clears it.
 That is a false negative, not missing work. In that case only, run the same diff against the
 vault repo (`git -C <vault.planningPath> diff --name-status HEAD~1 HEAD`) and confirm the
 changed paths correspond to the declared files[] with the leading `planning/` replaced by this
 repo's subdirectory name in the vault; set workAssertionPassed=true only if that vault-side
 diff is non-empty AND corresponds. A task with a MIX of vaulted and non-vaulted files is NOT
 this case and must still pass the ordinary assertion.

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

1. **Setup — plain branch, or isolated worktree with `--worktree`**:
   - **Without `--worktree` (default):** check out branch `<spec-slug>-flow` IN THE MAIN WORKING
     TREE — no sparse-checkout worktree, so a relative `planning/` symlink (brain-vaulted repos) stays
     intact. `main` stays on the branch until the PR merges; refuse to start on a dirty working tree.
   - **With `--worktree`:** create (or, with `--resume`, reuse/re-attach) an isolated sparse-checkout
     worktree, mirroring `/sdlc-task`'s Steps 1b/1c (see `.agents/skills/sdlc-task/SKILL.md`) with the
     branch name `<spec-slug>-flow` instead of `<blockId>-task`:
     - `--resume`: try to reuse first — `git worktree list | grep "trees/<spec-slug>-flow"` and
       `git branch --list "<spec-slug>-flow"`. Worktree exists → reuse verbatim. Branch exists but
       worktree missing (orphaned) → re-attach with `git worktree add --no-checkout` (no `-b`). Neither
       exists → fall through to a fresh create.
     - Fresh create:
       ```
       mkdir -p trees
       git worktree add --no-checkout trees/<spec-slug>-flow -b <spec-slug>-flow
       git -C trees/<spec-slug>-flow sparse-checkout init --cone
       git -C trees/<spec-slug>-flow sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
       git -C trees/<spec-slug>-flow checkout
       ```
       Then seed gitignored `.env`/`.env.*` files (same recipe as `/sdlc-task` Step 1b(f)) and commit
       `chore: init worktree <spec-slug>-flow --allow-empty`.
     - Repair the `planning/` symlink inside the worktree if the repo is brain-vaulted (same recipe as
       `/sdlc-task` Step 1c — absolute symlink to the same vault target, never relative, never a real
       directory).
     - `runDir = repoRoot/trees/<spec-slug>-flow`.
   - **Spec location.** Paths are `planning/<spec-slug>/...` at the git root by default. If no spec
     exists there, ALSO check `<invoking-dir-relative-to-root>/planning/<spec-slug>/...` — a
     sub-brain tier (e.g. `business/`) has its own `planning/` without being its own git repo. The
     ROOT always wins when a spec exists at both locations. If found at neither, abort and name BOTH
     paths you searched, not just one.
   - **Binding / brain-root / population guards** (BT.ticket.worktree-setup-can-adopt-the-brain-root-as-repo-root)
     — run these BEFORE the D16 preflight lint below and before any task work, comparing against the
     `repoRoot` you resolved at the top of this step (never re-derive it):
     - **BINDING GUARD.** `runGitCommonDir = git -C <runDir> rev-parse --path-format=absolute
       --git-common-dir`. If it does not resolve under `repoRoot`, abort — `Setup binding guard
       failed`, naming both `runGitCommonDir` and `repoRoot`. This is the check that catches the
       run silently adopting a different repo (e.g. the brain root) than the one it resolved.
     - **BRAIN-ROOT GUARD.** If `<runDir>/brain.toml` exists but a `brain.toml` did NOT exist at the
       invocation root, abort — `Setup binding guard failed`, naming both paths. Never identify a
       brain root by counting harness checks or by a hardcoded path — brain.toml presence at the two
       roots is the only signal.
     - **POPULATION GUARD (worktree mode only).** Every path in `git -C <runDir> ls-files` must exist on disk at `<runDir>/<path>`; if
       any are missing, abort — `Setup binding guard failed`, naming the missing count and up to
       five example paths.
     Log each guard's verdict, pass or fail — the transcript must show the check ran, not merely
     that nothing exploded.
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
   - **`expect_red` (D68) — an INVERTED verdict, easy to get backwards.** A task may carry
     `"expect_red": ["<command>", ...]`, and every command listed there must also appear in that
     same task's own `validation_commands`. Each named command **passes on a NON-ZERO exit and
     fails on exit 0** — the opposite of every other check. This is for a task whose deliverable IS
     a test observed failing; a zero exit means the deliverable is missing, not that the task
     succeeded. Every other check on that task's list is judged normally. If an `expect_red` entry
     names a command that is NOT in that task's `validation_commands`, that is a hard spec error:
     abort with `ABORTED (spec error)` — never silently ignore it, and never invert a project-wide
     `gates:true` harness check, which `expect_red` can never reach.
   - **The derive source follows `specSource`.** If the spec came from an authored block record
     (`planning/blocks/<BlockID>.json`), derive `tasks.json` by decomposing its `what`, `why`,
     `files`, `acceptance_criteria`, `testing_strategy` and `validation_commands`. Otherwise derive
     from `tasks.md`'s step list. Either way it is a real decomposition, never a verbatim copy.
   - Only if that source is also missing, or has no derivable content, abort: report `ABORTED
     (D16)` and tell the user to run `/generate-tasks <blockId>` to author `tasks.json`, commit,
     then re-run. Deriving from an authored block record or `tasks.md` is not guessing the task
     structure; fabricating one from nothing is what D16 still refuses to do.
3. **Execute Tasks sequentially in the worktree**:
   - For each task in the specified range (or all if not specified):
     - Run `/update-task` to flip status to `In progress` in the worklog and local files.
     - Implement the task following instructions.
     - Run fast validation tests.
     - Fix failures (up to 3 triage/fix attempts).
     - Run the COMMIT-SAFETY GUARD above, `&&`-joined with the commit itself, then commit the task
       state on the branch (`feat: implement <slug> task N`). If a vault commit is also needed
       (D46), run the same guard against `git -C <vault path>` before that commit too.
     - **If this task's fix loop ended in a triage MAJOR or an exhausted-attempts bail
       (BT.ticket.bails-must-be-append-only):** append one fully-populated entry to the committed
       `state.json`'s top-level `bails` array — `{occurred_at, task_id, check_id, failing_artifact,
       ownership, bail_class, reason, resolution: null}` — never overwrite or truncate the array; a
       second bail in the same run appends a second entry, the first stays byte-identical.
       `bail_reason` is still set too, as a plain mirror of the newest entry's `reason`. On
       `--resume`, read `state.json`'s prior `bails` array and carry it forward verbatim before
       appending anything new — re-initialising it instead of merging silently deletes a bail that
       was later retried successfully, which is the exact defect this record exists to prevent.
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
   - Update the status and log. In-place (non-worktree) only, after `mev emit-state --write`
     succeeds: if `planning/harness.json` declares an OPTIONAL `postEmitCommitCommand`
     (BT.ticket.bookkeep-leaves-derived-output-uncommitted), run it — this is project policy, never
     an engine default, so an absent key is a silent no-op. A hook failure must be reported, never
     swallowed, and never blocks the wrap-up commit below (the hook owns its own transaction).
   - Run the COMMIT-SAFETY GUARD `&&`-joined before the wrap-up commit — both the repo-local one and,
     in a vaulted repo, the vault one (`git -C <vault path>`) — then commit.
   - Create a pull request (PR) using git CLI or GitHub CLI (unless `--no-pr` is specified).






