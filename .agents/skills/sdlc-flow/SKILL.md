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

## Execution Guide (no shell/CLI access to `sdlc-flow.js` — replicate it by hand)

You cannot invoke `sdlc-flow.js` (no `claude` CLI, no Workflow runtime). When the user asks you to run
`/sdlc-flow <spec-slug> [range] [flags]`, perform every phase below yourself, using only your own
file/git tools. Do exactly what is written here — do not invent conventions, commit messages, or
shortcuts not listed.

### 0. Parse the invocation

```
/sdlc-flow <spec-slug> [range] [--auto-merge] [--no-pr] [--worktree] [--resume] [--test-depth fast|full] [--tasks <range>]
```

- `<spec-slug>` (`blockId`) is required.
- Task selection: `--tasks 1-7` OR a bare positional 2nd token that isn't a flag (`1-3`, `1,3,5`, `5`).
  Parse forms `N`, `N-M`, comma-separated lists, e.g. `1-3,7`. If given but unparseable, abort with an
  error — do not guess.
- `--worktree` opts INTO an isolated worktree. **Its absence is the default and means a plain branch
  in the main working tree** — this is the opposite of naive assumptions; do not default to a worktree.
- `--resume` re-attaches the existing branch/worktree for this spec and resumes from its committed... no —
  from its **on-disk** `sdlc-flow-state.json` (see Phase 1c: this file is never committed).
- `--test-depth fast|full` overrides the per-task validation depth (default: `fast`, i.e. gating checks
  only per task; the end-review always runs the FULL suite regardless of this flag).
- `--no-pr` stops after wrap-up; no PR is opened.
- `--auto-merge` merges the PR and tears down the branch/worktree, but only fires later under strict
  conditions (see Phase 5).

Derived paths/names (fixed formulas — do not vary them):
- `blockDir` = `planning/<spec-slug>`
- `specFile` = `planning/<spec-slug>/tasks.md`
- `tasksJsonFile` = `planning/<spec-slug>/tasks.json`
- `breakdownFile` = `planning/<spec-slug>/breakdown.md`
- `stateFile` = `planning/<spec-slug>/sdlc/sdlc-flow-state.json` — the run's authoritative index.
  **Written to disk on every state-write step. NEVER committed to git**, by design (see Phase 1c).
- `worklogFile` = `planning/<spec-slug>/sdlc/worklog.md` — human-readable trail, same disk-only rule.
- `branchName` = **`<spec-slug>-flow`** — plain, no `sdlc-flow/` namespace prefix.
- Worktree path (only when `--worktree`): `trees/<spec-slug>-flow` (i.e. `trees/<branchName>`).

### 1. Phase: Setup — branch (default) or worktree (`--worktree`)

Everything below runs from the main repo root first.

**1a. Branch mode (default, no `--worktree`):**
1. `git branch --list "<branchName>"`.
   - If it exists and `--resume` was passed: `git checkout <branchName>`. Reuse it; do not recreate.
   - If it exists and `--resume` was NOT passed: **hard-abort.** Do not bump to `<branchName>-2` and do
     not silently restart — that orphans a prior run's progress. Report: a branch named `<branchName>`
     already exists from a prior `/sdlc-flow` run on this spec; re-run with `--resume` to continue it,
     or `git branch -D <branchName>` to discard and start over. Stop here.
   - If it does not exist: continue to step 2.
2. Guard against a dirty tree — uncommitted changes would ride onto the branch: `git status --porcelain`.
   If it prints anything, **abort** (do not create the branch): report that the working tree is not
   clean, list the dirty paths, and suggest committing/stashing or using `--worktree` instead.
3. `git checkout -b <branchName>`. No sparse-checkout, no env-file copy, no init commit — you're in the
   real repo checkout, so any relative `planning/` symlink (vaulted repos) is already intact.
4. Verify: `git branch --show-current` prints `<branchName>`; `ls planning/ .claude/` both resolve.
5. The working directory for every later step is the repo root itself (there is no separate worktree
   directory in this mode).

**1b. Worktree mode (`--worktree`):**
1. If `--resume`: check `git worktree list | grep "trees/<branchName>"` and
   `git branch --list "<branchName>"`. If the worktree exists, reuse it as-is. If only the branch exists
   (worktree dir was removed), re-attach: `mkdir -p trees && git worktree add --no-checkout trees/<branchName> <branchName>`,
   then `sparse-checkout init --cone`, `sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')`,
   `checkout`, then re-copy gitignored env files (see step 4 below). If neither exists, fall through to
   step 2 as a fresh run.
2. If NOT resuming: check the same two things for the exact candidate `<branchName>`. If either exists,
   **hard-abort** with the identical message as branch mode above (re-run with `--resume`, or
   `git worktree remove trees/<branchName> --force && git branch -D <branchName>` to discard). Do not
   bump to `-2`. Only when genuinely free, proceed (a name collision with something unrelated to this
   spec may bump to `-2`…`-10`, but a match on the spec's own base name never does).
3. Create the worktree:
   ```
   mkdir -p trees
   git worktree add --no-checkout trees/<branchName> -b <branchName>
   git -C trees/<branchName> sparse-checkout init --cone
   git -C trees/<branchName> sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
   git -C trees/<branchName> checkout
   ```
4. Copy every gitignored env-shaped file (`.env`, `.env.local`, `.env.*`, at any depth, excluding
   `node_modules/`, `.venv/`, `venv/`, `trees/`, `vendor/`) from the repo root into the worktree,
   preserving relative path, never overwriting an existing file:
   ```
   git ls-files --others --ignored --exclude-standard -- . \
     | grep -E '(^|/)\.env(\.[^/]*)?$' \
     | grep -Ev '(^|/)(node_modules|\.venv|venv|trees|vendor)/' \
     | while IFS= read -r f; do dest="trees/<branchName>/$f"; [ -f "$dest" ] || { mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; }; done
   ```
5. `git -C trees/<branchName> commit --allow-empty -m "chore: init worktree <branchName>"` — this is
   the ONLY commit made purely as part of setup, and it only happens in worktree mode.
6. Fix the `planning/` symlink for the worktree (run from the MAIN repo root, always — fresh create,
   re-attach, or reuse alike). If the main repo's `planning` is a relative symlink (vaulted), that
   relative target breaks when evaluated from inside `trees/<branchName>/`. Point the worktree's
   `planning/` at the SAME real target via an absolute symlink (gitignored, never committed):
   ```
   if [ -L planning ]; then
     TARGET="$(python3 -c "import os; print(os.path.realpath('planning'))")"
     rm -f trees/<branchName>/planning
     ln -s "$TARGET" trees/<branchName>/planning
   fi
   ```
   If `planning` is a real tracked directory, the sparse-checkout already populated it — do nothing.
7. Verify: `git worktree list`; `ls trees/<branchName>/` contains at least `planning/` and `.claude/`;
   `ls trees/<branchName>/planning/` resolves.
8. The working directory for every later step is `trees/<branchName>` (the "worktree root"). Every Bash
   call in the phases below must `cd` there first — shell state does not persist between calls.

**1c. Both modes — post-setup checks (from the live checkout):**
- Spec exists: `ls <specFile>`. If missing, abort — instruct the user to run `/generate-tasks
  <spec-slug>` (and `/breakdown`) on `main`, commit, then re-run.
- **D19 thin-spec guard** (fresh runs only — skip this check entirely on `--resume` or when the
  branch/worktree already existed): flag as thin ONLY on high-confidence signals — an unfilled `{{TOKEN}}`
  anywhere in the spec, or an empty/template-only "## Acceptance Criteria" section (no real `- ` bullet).
  Do not flag bare `TODO`/`TBD`, do not treat `<...>` as a token, never flag the Amendment Log seed
  `_No amendments yet._`. If thin: **abort** — instruct fleshing out the spec (or `/generate-tasks
  --force` to regenerate), commit, re-run. When in doubt, do NOT flag it — a wrongly-blocked valid spec
  is worse than a missed thin one.

### 2. Phase: Plan — enumerate tasks (D16) + resolve policy + load resume state

1. Read `<tasksJsonFile>`. It is a **bare JSON array** of task objects (not wrapped in an object),
   matching the SDLCTask schema — each has at least `task_id`, and may carry `files[]` and
   `validation_commands[]`.
2. **D16 preflight lint**: if the file is missing, invalid JSON, or an empty array, **abort** — instruct
   running `/generate-tasks <spec-slug>` to author it, commit, then re-run. Never guess the task
   structure.
3. Collect `allTasks` = every `task_id`, in array order. If a task range/selection was given, filter to
   it; otherwise use every task. This is `taskList`.
4. Per-task validation overrides: for each task whose `validation_commands` is a non-empty array, note
   it — running that task's tests means running ONLY those commands (they fully replace the
   harness/spec checks for that task's fast tripwire). Tasks with no `validation_commands` fall back to
   the harness/spec checks as normal. The end-review always runs the full harness/spec suite regardless.
5. Engine-parse-safety scan: for each task, look at its `files[]` for any path under
   `.claude/workflows/`. Record those paths per task — they get an unconditional
   `node --check <path>` gate (see Phase 3), independent of `harness.json`, in both the fast per-task
   tripwire and the full end-review suite. Skip tasks with no such path.
6. **If `--resume`**: read `<stateFile>` off disk (`cat`, not `git show` — it was never committed).
   - Missing or invalid → log that resume was requested but no valid state was found; run all
     selected tasks fresh.
   - Present → collect every `tasks[N]` whose `status == "passed"` into a skip-set, AND carry forward
     the **entire** prior `tasks` object verbatim as your in-memory task history (this is required —
     without it, the next state write would silently drop the earlier-passed tasks' history, and the
     *next* resume after that would see them as never-passed and re-run them).
7. Load `planning/harness.json` if present (parse as JSON; on invalid JSON, treat as absent — never
   fabricate policy). Resolve:
   - `testDepth` = `--test-depth` flag, else `harness.json`'s `flow.testDepth` (if `fast`/`full`), else
     `fast`.
   - `autoMerge` = `--auto-merge` flag OR `harness.json`'s `flow.autoMerge === true`.
   - `prBase` = `harness.json`'s `flow.prBase`, else `main`.
   - Extra immediate-bail reasons = `harness.json`'s `flow.bailReasons[]` (appended to the built-in
     list in Phase 3).
   - Validation checks = `harness.json`'s `validation.checks[]` (kind: `command` (default) /
     `baseline-diff` / `skip-count-regression` / `warning-scan` / `forbidden-pattern-scan`). The fast
     per-task tripwire runs only checks with `gates: true` and `perTask !== false`; the end-review runs
     every check regardless of `gates`/`perTask`. **If `harness.json` is absent, or carries no matching
     checks: fall back to running the spec's own `## Validation Commands` section, in order** — the
     engine ships no stack defaults. If the spec has no such section either, run no project checks.
   - Always, in addition to whatever checks above: scan changed `.md` files (`git diff --name-only
     <prBase>..HEAD`) for stray emoji — this "universal emoji gate" always runs, project-agnostic. The
     literal `🤖 Generated with Claude Code` PR-footer line is the one exception, and only inside a PR
     body, never inside docs.
   - For any `baseline-diff` / `skip-count-regression` check with a `baselineCommand`: snapshot a
     baseline once, before task 1, if one doesn't already exist on disk (resume-safe — never overwrite
     an existing baseline).

### 3. Phase: Tasks — sequential per-task loop, `MAX_TASK_ATTEMPTS = 3`

For each `taskNum` in `taskList` (in order), skip it entirely if it's in the resume skip-set (log
"already passed — skipping" and move on). Otherwise, note there is **no separate "flip to In Progress"
step and no `/update-task` call anywhere in this pipeline** — task status lives ONLY in the in-memory
`state.tasks[N].status`, persisted by the state-write steps below.

For `attempt = 1..3` (stop early on pass or bail):

1. **Implement (attempt 1) or Fix (attempt > 1).** Read `CLAUDE.md` and `planning/context.md` first.
   Read the spec (`specFile`) and find this task's object in `tasksJsonFile` by `task_id`. If
   `<breakdownFile>` exists, use its `### Step N:` sub-steps as the execution guide for this task
   (`tasks.json` stays authoritative for scope). On a fix pass, make the MINIMUM targeted change to
   address the specific failure from the previous attempt — do not re-implement from scratch. Run a
   completeness self-check (no stubs/placeholders on any acceptance-criteria path) before committing.
   Run the spec's `## Validation Commands` for this task to sanity check.
   **Commit** (stage files explicitly by name — never `git add -A`/`git add .`):
   - Implement (attempt 1): `feat: implement <spec-slug>-task<N>`
   - Fix (attempt > 1, this is "fix pass P" where P = attempt − 1): `fix: fix pass <P> for <spec-slug>-task<N>`
2. **Fast test.** Run the checks resolved in Phase 2 step 4/7 for this task: if this task declared its
   own `validation_commands`, run ONLY those; otherwise run the gating-only subset when `testDepth ==
   fast`, or the full suite when `testDepth == full`. Always also run the universal emoji gate, and the
   engine-parse-safety `node --check` on any `.claude/workflows/` path this task's `files[]` named.
3. **On full pass:** mark this task's status `passed`, record its validation label, and persist state
   (Phase 3 "state writes" below). Move to the next task.
4. **On any failure:** run the **triage** step —classify the failure as `RETRYABLE` or `MAJOR` against
   this fixed list of immediate-bail reasons (plus any `harness.json` `flow.bailReasons[]` extras):
   1. Missing/undefined upstream dependency or symbol the spec assumes exists.
   2. Spec ambiguity/contradiction — intended behavior is genuinely undeterminable.
   3. Environment/credential/auth/network failure (not a code defect).
   4. Change would require a destructive or out-of-scope action.
   5. Same failure twice in a row with no progress (stuck), or a structural design flaw needing a
      re-plan.
   Any match → `MAJOR`. Otherwise `RETRYABLE` only if the failure is transient/infra OR has genuinely
   changed since the previous attempt (progress is being made). When unsure, prefer `MAJOR`/bail — a
   wasted retry loop costs more than bailing early. Before asserting a failure "pre-dates this task" /
   "is unrelated", re-verify against the base state if at all possible; otherwise phrase it explicitly
   as an unverified hypothesis, never as fact.
   - **`MAJOR` → break immediately.** Do NOT continue burning remaining attempts. Mark this task
     `failed`, set the run's `bail_reason`, persist state, and stop the whole per-task loop (go straight
     to Wrap-up, Phase 5 — Review and Docs are skipped).
   - **`RETRYABLE` and this was attempt 3 (the last one)** → also bail (attempts exhausted), same as
     above, but with a bail reason noting the attempt count and the still-failing checks.
   - **`RETRYABLE` and attempts remain** → loop back to step 1 as a fix pass. **The final fix pass
     (attempt 3) runs on the Opus model** instead of the default Sonnet — every earlier attempt (and
     every other stage) stays on its normally-tiered model.
5. **State write (once per task, after the loop resolves).** Regardless of outcome, persist
   `sdlc-flow-state.json` + append a section to `worklog.md` **to disk only** — this write is never
   `git add`ed or committed, by design (see the state-file note in section 1c/6 below). In practice this
   write is frequently folded into the very same turn as the passing test or the terminal triage call,
   but the effect is identical: after every task, the on-disk state reflects that task's final status,
   attempts, summary, issues, fixes, decisions, files changed, commit hash, and (on pass) which
   validation label applied.
6. If this task bailed, stop the per-task loop entirely — do not start the next task.

### 4. Phase: Review — ONE consolidated review (only if no task bailed)

Runs once, over the fully integrated branch, only when every selected task passed. `MAX_REVIEW_ATTEMPTS
= 3`.

For `attempt = 1..3`:
1. Read the on-disk `sdlc-flow-state.json` as an index (per-task summary/issues/fixes/decisions/files) —
   it is a starting point, not a substitute for verifying against the code.
2. Read the spec's full "## Acceptance Criteria" as the checklist. Read the real diff:
   `git diff --stat <prBase>..HEAD` and `git diff <prBase>..HEAD`.
3. **Re-run the FULL gating suite fresh** — never trust the per-task fast-tripwire results; this run is
   authoritative. This is every check in `harness.json`'s `validation.checks[]` (or the spec's `##
   Validation Commands` fallback), plus the engine-parse `node --check` gate for every
   `.claude/workflows/` path touched by any task in `taskList`, plus the universal emoji gate.
4. Mark every acceptance criterion MET / PARTIAL / NOT_MET against the actual code. Also check CLAUDE.md
   standing-rule compliance and identity-integrity (no handle/URL contradicting CLAUDE.md's verified
   identities) as failing criteria if violated. Do not fix environment/infra issues yourself here —
   report them for the fix loop.
5. Verdict:
   - `PASS` — every in-scope criterion MET and every fresh gating check passes.
   - `PARTIAL` — some criteria partially met, or gating passes but coverage is incomplete.
   - `FAIL` — any criterion NOT_MET, or any fresh gating check fails.
6. On `PASS`: stop the review loop, proceed to Docs (Phase 5).
7. On `FAIL`/`PARTIAL`: classify the findings as **localized** (small, a bounded fix can close them) vs
   **broad/structural** (cross-cutting, ambiguous, needs a human re-plan) — also run the same triage
   step as Phase 3 over the findings.
   - Broad, OR triage says `MAJOR`, OR this was attempt 3 (attempts exhausted) → **bail** immediately:
     set the run's `bail_reason`, mark the run blocked, persist state, and stop — do not run Docs.
   - Localized and attempts remain → run a **bounded fix** over the integrated tree, touching only what
     the findings named (do not re-implement or touch passing criteria); add/adjust tests; run the
     spec's `## Validation Commands`; commit (stage explicitly):
     **`fix: review pass <N> for <spec-slug>`** — where `N` is this review attempt number. This is a
     distinct commit-message family from the per-task fix pass in Phase 3 (`fix: fix pass P for
     <spec-slug>-taskN`) — do not conflate the two. **The fix pass immediately preceding the final
     (3rd) review attempt runs on Opus.** Persist state, then loop back to step 1 for another review
     pass.

### 5. Phase: Docs — gated strictly on `finalVerdict == PASS`

Skip this phase entirely on any bail, and on a `FAIL`/`PARTIAL` verdict that didn't escalate to bail
would never reach here either — Docs only ever runs after a clean `PASS`.

1. Read the on-disk state file's list of changed files across all tasks, plus `git diff --stat
   <prBase>..HEAD`.
2. Check whether `docs/` has any project-facing `.md` files (excluding a `docs/workflows/` subtree). If
   it has **zero** — **bootstrap mode**: read every changed source file in full, and author baseline
   reference docs from scratch (at minimum `docs/architecture.md`; add `docs/cli.md`,
   `docs/api-reference.md`, or `docs/pages.md` as applicable to what the project actually is). Create
   `docs/index.md` if missing, with a row per new doc. Every new file needs OKF frontmatter (`type`,
   `title`, `description` required).
3. Otherwise — **surgical patch only**: for each changed source file, grep `docs/` for references to its
   names, and Edit (never rewrite) only the affected sections — signatures, prop tables, route lists,
   descriptions. Add docs for genuinely new public APIs. Never delete documented items that still exist.
   Never edit `CLAUDE.md`. No emoji.
4. If a top-level architecture/overview/index doc needs a change, **flag it `NEEDS_REVIEW`** instead of
   editing it directly.
5. Commit (stage explicitly) only if something was patched or created:
   **`docs: update docs for <spec-slug>`**. If nothing needed changing, make no commit.
6. Persist state to disk (same disk-only rule as Phase 3/6).

### 6. Phase: Wrap-up — status/log/state, then PR, then optional auto-merge

Wrap-up always runs, whatever the outcome (`PASS`, `PARTIAL`/`FAIL`-bail, or a task-loop bail) — there is
no gating condition on entering this phase.

**6a. Detect the planning vault (D46) — required before any commit in this phase:**
```
[ -L planning ] && echo "SYMLINK" || echo "PLAIN"
python3 -c "import os; print(os.path.realpath('planning'))"
```
`vaulted` = true iff the first line is `SYMLINK`. `vaultRealPath` = the resolved absolute path printed by
the second line (works for both cases). **This determination controls how step 6c below is committed —
get it right before touching git.**

**6b. Update authored docs (Edit tool, surgical):**
1. `planning/status.md`:
   - If bailed: keep the spec's Status as-is (or set `Blocked` if appropriate); set "Current focus" to
     `<spec-slug> — BLOCKED: <bail_reason>`.
   - Else: if this run covered the FULL spec (no task-range selection was given), flip the spec's Status
     to `Done`. If a task range/selection was given, leave Status as `In progress` and point "Current
     focus" at the next task — UNLESS this range happened to be the last remaining tasks, in which case
     flip to `Done` too. Update "Current focus" accordingly.
   - Update the "Last updated" date.
2. `planning/state.json` (the **authored block graph** — `tracks[].blocks[]`, a DIFFERENT file from
   `sdlc-flow-state.json`; skip this whole step silently if the repo has no `planning/state.json`):
   - If bailed: do not touch it.
   - If a task range/selection was given and this run did NOT just flip status.md to `Done`: leave it
     untouched (only flip on genuine full-spec completion, never on a partial range).
   - Otherwise: resolve this spec's block id from the `status.md` row you just edited, then run this
     **scripted, non-interactive** mutation (never open the file in an editor, never a manual Edit-tool
     diff) — it only ever mutates the single matching block, and never writes if no match is found:
     ```
     python3 -c "
     import json, sys
     path = 'planning/state.json'
     bid = sys.argv[1]
     data = json.load(open(path))
     found = False
     for track in data.get('tracks', []):
         for block in track.get('blocks', []):
             if block.get('id') == bid:
                 block['status'] = 'closed'
                 found = True
                 break
         if found:
             break
     if found:
         with open(path, 'w') as fh:
             json.dump(data, fh, indent=2)
             fh.write(chr(10))
         print('FLIPPED:' + bid)
     else:
         print('NOT_FOUND')
     " "<resolved-block-id>"
     ```
     Trust only this script's own stdout (`FLIPPED:<id>` or `NOT_FOUND`) — never assume success. Then
     validate: `python3 -c "import json;json.load(open('planning/state.json'))"`.
3. Regenerate derived surfaces via `mev emit-state --write` — run this step whenever wrap-up runs at
   all, independent of whether the spec fully completed (status.md was edited either way):
   - **Worktree mode**: do NOT run it here — `mev emit-state` refuses to run inside a linked git
     worktree. Surfaces regenerate on the base branch when this branch eventually merges (via
     `/clean-worktree`, `/merge-train`, `/close-out --merge-branch`, or the `--auto-merge` path below).
   - **Branch mode**: run it right here, on this branch, in the repo root: `mev emit-state --write`. If
     `mev` or `brain.toml` is absent (standalone/non-brain repo), skip silently.
4. Prepend a new entry to `log.md` (newest first): a dated header, one paragraph summarizing what was
   implemented across the tasks run, the final verdict (and why it bailed, if it did), notable
   decisions, ending with "Next: …", followed by the `git log --oneline -8` of this run's commits.
5. **D18 amendment log**: review the run's per-task issues/fixes/decisions for genuine *deviations* from
   the spec as written (materially different task execution, a scope change, a substitution, a
   deferral). Routine success is NOT a deviation — do not log one for every task. For each real
   deviation, append ONE dated line to the spec's own "## Amendment Log" section (append-only; replace
   the `_No amendments yet._` seed if it's the first entry): `- YYYY-MM-DD [task N] <what changed vs the
   spec, and why>`.

**6c. Commit — the D46 vault-aware split.** Never run `git checkout`/`git switch`/`git branch` outside
this repo's own root (or the vault's root, in the vaulted case) as part of this step; if a `git add`
fails, report it — do not relocate the commit to force it to succeed.

- **If NOT vaulted** (`planning/` is a plain tracked directory): everything commits together, in this
  repo, on this branch:
  ```
  git add planning/status.md log.md
  git add planning/state.json 2>/dev/null || true
  git add <specFile> 2>/dev/null || true
  git commit -m "chore: wrap up <spec-slug>"
  ```
- **If vaulted**: `planning/status.md` and `planning/state.json` do not physically live in this repo —
  their bytes live in the vault repo at `vaultRealPath`. Stage and commit them THERE, through their
  real path, via `git -C`, on whatever branch the vault repo is already on — never `cd` into it, never
  checkout/switch/branch there:
  ```
  git -C <vaultRealPath> add <vaultRealPath>/status.md
  git -C <vaultRealPath> add <vaultRealPath>/state.json 2>/dev/null || true
  git -C <vaultRealPath> diff --cached --quiet || git -C <vaultRealPath> commit -m "chore: wrap up <spec-slug>"
  ```
  and, as a **separate commit**, the repo-local files, in this repo, on this branch:
  ```
  git add log.md
  git add <specFile> 2>/dev/null || true
  git commit -m "chore: wrap up <spec-slug>"
  ```
  Never fold these into one `git add planning/...` from the repo root — that fails with "pathspec is
  beyond a symbolic link", and never repair that failure by operating inside the vault repo's own
  branch/checkout.

**6d. Final state write.** Persist `sdlc-flow-state.json` (status: `blocked` if bailed, else `done`) +
append a `## Wrap-up — <verdict>` section to `worklog.md`, disk-only, same rule as every other state
write in this pipeline — never committed.

**6e. Open a PR** (unless `--no-pr`). Check `gh` and the remote first
(`command -v gh`, `git remote -v`); if either is absent, do not fail — report the branch name and print
manual instructions (`git push -u origin <branchName> && gh pr create --base <prBase> --head
<branchName>`) and stop here.
1. Push: `git push -u origin <branchName>`.
2. Build the PR body from the on-disk state file, with these sections in order: `## What & why`
   (one paragraph — spec goal + what each task delivered), `## Tasks` (per task: number, status,
   one-line summary, commit), `## Validation` (the review verdict + what the end-review re-ran), then
   either `## Why this is a DRAFT / blocked` (bailed runs — the exact bail reason, for human pickup) or
   `## Remaining / follow-ups` (clean runs — anything deferred), then `## How it was validated` (the
   gating checks the end-review ran). End the body with this exact footer line — **the only place an
   emoji is permitted anywhere in this pipeline**:
   ```
   🤖 Generated with Claude Code
   ```
3. Title:
   - Bailed → `[BLOCKED] <spec-slug>: <bail_reason, truncated to 60 chars>`
   - Otherwise → `<spec-slug>: <N> task(s), review <finalVerdict>`
4. Create it: `gh pr create --base <prBase> --head <branchName> [--draft] --title "..." --body "..."`
   (`--draft` iff the run bailed). If `gh` reports a PR already exists for this branch, treat it as
   created and capture its URL/number instead of erroring.

**6f. `--auto-merge`** — only fires when ALL of: the run did not bail, `finalVerdict == PASS`, a PR was
created, and it is not a draft. Otherwise log that it was skipped and why; leave the branch/worktree
intact for manual review.
1. `gh pr merge <number or url> --merge --delete-branch`. If this errors (not mergeable, checks
   pending), STOP — do not clean up; report the failure.
2. `git checkout <prBase> && git pull --ff-only`.
3. Tear down:
   - Worktree mode: `git worktree remove trees/<branchName> --force && git worktree prune`, then
     `git branch -D <branchName>` (ignore errors — it's usually already gone via `--delete-branch`).
   - Branch mode: `git branch -d <branchName> 2>/dev/null || git branch -D <branchName> 2>/dev/null ||
     true` (no worktree to remove).
4. On `<prBase>` now, in the main tree: run `mev emit-state --write` (unconditionally safe here, unlike
   in worktree-mode wrap-up) to re-derive rollups/focus/tables from the merged block-status flip. Skip
   silently if `mev`/`brain.toml` are absent.

### Reference — exact commit-message strings used in this pipeline

| Message | When |
|---|---|
| `feat: implement <spec-slug>-task<N>` | Phase 3, first attempt on a task |
| `fix: fix pass <P> for <spec-slug>-task<N>` | Phase 3, fix attempt P (P = attempt − 1) on a task |
| `fix: review pass <N> for <spec-slug>` | Phase 4, bounded fix after review attempt N found localized issues |
| `docs: update docs for <spec-slug>` | Phase 5, only if something was patched/created |
| `chore: wrap up <spec-slug>` | Phase 6c — once (non-vaulted), or twice — vault commit + repo-local commit, both with this exact message (vaulted) |
| `chore: init worktree <branchName>` | Phase 1b step 5, worktree mode only, one allow-empty commit at setup |

**`sdlc-flow-state.json` and `worklog.md` are never committed, in any phase, by any agent role.** They
are written to disk only and read back off disk only (never via `git show`/`git log`) — this is
deliberate: a resume only ever needs the latest bytes on disk, and committing them was the source of a
past bug where an agent "repaired" a vault pathspec failure by committing inside the brain vault's own
branch. Do not add a commit step for these two files under any circumstance.

### Quick checklist before you start

- [ ] Confirm `--worktree` was or wasn't passed — branch is the default, not worktree.
- [ ] Confirm branch name is `<spec-slug>-flow`, no prefix.
- [ ] If a branch/worktree for this spec already exists and `--resume` was not passed: abort, don't
      silently restart or bump to `-2`.
- [ ] Confirm `tasks.json` exists as a non-empty bare array before touching any task.
- [ ] Never call anything resembling `/update-task` to flip status — state lives only in
      `sdlc-flow-state.json`.
- [ ] Detect the planning vault (`[ -L planning ]`) before Phase 6c's commit, every time.
- [ ] Docs only runs after a clean `PASS` — never on `PARTIAL`, `FAIL`, or a bail.
- [ ] Never commit `sdlc-flow-state.json` or `worklog.md`.




























