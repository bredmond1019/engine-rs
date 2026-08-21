---
name: sdlc-task
description: >
  Lean single-unit SDLC engine — implement → fast-test → fix → commit, in place or in a worktree
---

=============================================================================
 sdlc-task — the LEAN small-work engine (implement → test → fix → commit)
 =============================================================================

 The cheap rung of the pipeline ladder, for one small unit of behaviour-changing
 work (a /ticket or /chore). Runs a spec's task(s) through a tight per-task loop —
   implement → fast gating-test → triage → fix (≤3 attempts, Opus on the last)
   → commit → [terminal authoritative reconcile] → lean bookkeep close-out
 and nothing else. No scout, no separate review, no document stage, no ui-test, no
 PR. The bookkeep close-out is deliberately lean: on a passing full run it flips the
 authored status markers (tasks.md task status, the status.md Progress row, the
 state.json block status) and — in place, on main — runs `mev emit-state --write`; it
 does NOT write a log.md narrative, a D18 amendment log, or run review/docs/PR. Run
 /log-work for the narrative. When you need a consolidated review + docs + a PR, use
 /sdlc-flow; for a whole spec in place, /sdlc-run; for a roadmap, /sdlc-block.

 TERMINAL AUTHORITATIVE RECONCILE (D56) — this engine's per-task tripwire runs
 `fastCommand` in place of `command` (testDepth=fast, the default) and never runs a
 `perTask: false` check at all, so those checks' real, authoritative form was never
 verified anywhere in the run. After the last task passes on a full, non-bailed,
 testDepth=fast run, ONE reconcile pass re-runs — with their real `command`, never
 `fastCommand` — only the gates:true checks the per-task tripwire actually skipped:
 those whose fastCommand differs from command, plus every perTask:false gating check.
 Checks with no fastCommand already ran authoritative on every per-task pass and are
 NOT re-run (redundant cost; see D56). Default-on, no flag, no harness.json opt-out —
 see D56 for why. A failing reconcile bails into a distinct terminal state,
 `reconcile_failed`: bookkeep does NOT run, the block is NOT flipped to done, and all
 per-task commits stand. Resume (--resume, no task selection) re-enters with every
 task already "passed" in state.json, so it naturally re-runs only the reconcile —
 no separate resume path needed. Skipped entirely when testDepth=full (every check,
 including perTask:false ones, already ran authoritative on every per-task pass — see
 renderCheckList) or on a partial task-subset run (the existing fullRun guard).

 ISOLATION
   Default: IN PLACE on the current branch (no worktree) — cheapest, like /sdlc-run.
   --worktree: run in an isolated git worktree on its own branch (you integrate the
   branch yourself when ready). Opt-in only.

 USAGE
   /sdlc-task <spec-slug>                 run every task in the spec, in place
   /sdlc-task <spec-slug> 2               run only task 2
   /sdlc-task <spec-slug> 1-3             run a task range (1-3, 1,3,5, 5)
   /sdlc-task <spec-slug> 2 --worktree    run task 2 in an isolated worktree/branch
   /sdlc-task <spec-slug> --resume        resume from the committed state file
   /sdlc-task <spec-slug> --test-depth full  full gating suite per task (default: fast)

 PIPELINE
   setup (locate repo / create worktree) → enumerate (D16 lint) → [resume load]
     → per-task loop → [terminal authoritative reconcile, D56] → lean bookkeep
     close-out (on pass) → final state commit

   Per-task loop (sequential):
     implement → fast-test → (triage → fix/bail) ×≤3 → one state write per task
   A triage MAJOR / immediate-bail reason breaks straight out (does NOT burn the
   remaining attempts); the run stops and reports for human pickup.

   Terminal reconcile (D56, after every task passes on a full, testDepth=fast run):
     re-run, with their authoritative `command`, only the checks the fast tripwire
     substituted (fastCommand) or skipped (perTask:false) → on failure, status
     "reconcile_failed" — bookkeep is skipped, the block is NOT flipped to done.

 STATE (NOT gitignored, but deliberately never committed — at planning/<spec>/sdlc/)
   sdlc-task-state.json   the authoritative run index (per-task summary/issues/fixes/commit +
                          the Block-A `tokens` block, plus `base_sha` — the pre-task HEAD this
                          run's own emoji gate diffs from). Written to disk after every task and
                          again at the end (cat-visible for crash inspection); read back off
                          disk only, by --resume, and by /close-out's Step 0.5 in-place fallback
                          (base_sha lets close-out scope its diff even when this run committed
                          straight to the base branch) — never out of git — so it is disk-only,
                          never committed (D46: planning/ may be a vaulted symlink into the brain
                          repo, where a plain `git add planning/...` fails).

 COMMIT STRATEGY
   feat: implement <stem>         implement agent (per task)
   fix:  fix pass P for <stem>    fix agent (per pass)
   chore: sdlc-task bookkeep — <…>  bookkeep close-out (on a passing run)

 MODEL TIERING (the token lever — see the MODEL map below)
   haiku : setup, enumerate, state-load, test, state-writer, bookkeep
   sonnet: implement, fix, triage
   opus  : ESCALATION on the FINAL per-task fix pass

 IMPLEMENTATION RULE: engines are self-contained — lift, don't import. No cross-engine
 require. Validation is downstream only; never run this against base-template itself.
 =============================================================================

## Antigravity Execution Guide

When the user asks you to run `/sdlc-task <spec-slug> [task|range] [--worktree] [--resume]
[--test-depth fast|full]` and you cannot invoke `sdlc-task.js` directly (no shell/CLI access to the
`claude` binary or the real workflow engine — only your own file/git/bash tools), replicate the lean
engine by hand, exactly, using the steps below. Do NOT substitute `/sdlc-run` or `/sdlc-flow` — this
is deliberately the cheap rung (no scout, no review stage, no docs stage, no PR). Someone following
only this section — not the `.js` — should end up doing exactly what the real engine does.

### Step 0 — Parse the invocation

- `<spec-slug>` (required) — call it `blockId`. Paths derived from it, initially against the git
  root, then **re-derived under a tier prefix** in Step 1.7 if the spec is only found in a sub-brain
  tier's own `planning/` (e.g. `business/`) — see Step 1.7 below:
  - `blockDir` = `planning/<blockId>`
  - `blockRecordFile` = `planning/blocks/<blockId>.json` — the authored block record (D65 stage 2):
    preferred spec source when present.
  - `specFile` = `<blockDir>/tasks.md` — legacy fallback, only used when `blockRecordFile` is absent.
    (`specFile` is reassigned to `blockRecordFile` once Step 1 determines the block record exists —
    every later step that reads `<specFile>` is reading whichever source actually won.)
  - `tasksJsonFile` = `<blockDir>/tasks.json`
  - `breakdownFile` = `<blockDir>/breakdown.md` (optional, from `/breakdown`)
  - `reportsDir` = `<blockDir>/sdlc/reports`
  - `stateFile` = `<blockDir>/sdlc/sdlc-task-state.json`
- Optional 2nd positional token (or `--tasks <spec>`) — a task selection: a single number (`2`), a
  range (`1-3`), a comma list (`1,3,5`), or a mix (`1-3,7`). Parse into the sorted set of integers it
  names; if it doesn't match `\d+(-\d+)?` per comma-part, or names nothing, stop and report an error —
  do not guess.
- `--worktree` — opt-in isolation (default is in-place on the current branch).
- `--resume` — resume from the on-disk `sdlc-task-state.json`, reusing the existing worktree/branch by
  name and skipping the D19 thin-spec gate (see Step 1).
- `--test-depth fast|full` — default `fast` (only `gates:true`-and-not-`perTask:false` checks run per
  task); `full` runs the whole harness suite on every task. Reject any other value.

### Step 1 — Setup: locate the repo, or create the isolated worktree

Run everything below from the **main repo root** unless noted.

1. `repoRoot` = `git rev-parse --show-toplevel`. `currentBranch` = `git rev-parse --abbrev-ref HEAD`.
   **Before any other `cd`**, also compute `candidateTierPrefix` — the CURRENT working directory's
   path relative to `repoRoot`, with a trailing slash, or `""` when you are already at `repoRoot`
   (e.g. invoked from inside `business/` → `"business/"`). This captures where `/sdlc-task` was
   actually invoked from, which `runDir` (computed later) does not preserve for in-place runs
   (`runDir = repoRoot` regardless of the invoking directory).
2. **Branch naming (worktree mode only).** There is ONE shared branch per spec run — never one branch
   per task number. Compute the base name:
   ```
   baseBranchName = ("<blockId>-task").toLowerCase() with every character outside [a-z0-9.-] replaced by "-"
   ```
   e.g. spec slug `Ticket-Foo_Bar` → `ticket-foo-bar-task`.
   - **Not `--worktree`**: skip straight to "in-place mode" below.
   - **`--worktree` + `--resume`**: try to reuse first —
     `git worktree list | grep "trees/<baseBranchName>"` and `git branch --list "<baseBranchName>"`.
     - Worktree exists → reuse it verbatim: `branchName = baseBranchName`, `wasCreated = false`; skip
       to Step 1c (symlink repair).
     - Worktree missing but the branch exists (orphaned — dir removed) → re-attach, **no `-b`**:
       ```
       mkdir -p trees
       git worktree add --no-checkout trees/<baseBranchName> <baseBranchName>
       git -C trees/<baseBranchName> sparse-checkout init --cone
       git -C trees/<baseBranchName> sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
       git -C trees/<baseBranchName> checkout
       ```
       then run the same env-file copy loop as Step 1b(f) below; `branchName = baseBranchName`,
       `wasCreated = false`; skip to Step 1c.
     - Neither exists → fall through to a fresh create.
   - **`--worktree` fresh create — find a free name.** Starting with `baseBranchName`, for each
     candidate check BOTH `git worktree list | grep "trees/<candidate>"` and
     `git branch --list "<candidate>"`. If both are empty, the candidate is free — use it. Otherwise
     try `<baseBranchName>-2`, `-3`, … up to `-10` and stop (do not go beyond `-10`). Call the winner
     `branchName`.
3. **Step 1b — create the worktree** (replace `[branchName]` with the chosen name):
   ```
   mkdir -p trees
   git worktree add --no-checkout trees/[branchName] -b [branchName]
   git -C trees/[branchName] sparse-checkout init --cone
   git -C trees/[branchName] sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
   git -C trees/[branchName] checkout
   ```
   f. **Env-file seeding** — copy every gitignored `.env`/`.env.*` file from the repo root into the
      worktree at the same relative path (so `app/.env` lands at `trees/[branchName]/app/.env`),
      excluding `node_modules/`, `.venv/`, `venv/`, `trees/`, `vendor/`, and never overwriting a file
      that already exists in the worktree:
      ```
      git ls-files --others --ignored --exclude-standard -- . \
        | grep -E '(^|/)\.env(\.[^/]*)?$' \
        | grep -Ev '(^|/)(node_modules|\.venv|venv|trees|vendor)/' \
        | while IFS= read -r f; do
            dest="trees/[branchName]/$f"
            if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi
          done
      ```
      Record every `ENV_COPIED:` line and report it at the end of setup (report an empty list plainly
      — "none found" — rather than staying silent, since a missing `.env` should surface here, not as
      a confusing downstream failure).
   g. `git -C trees/[branchName] commit --allow-empty -m "chore: init worktree [branchName]"`.
      `wasCreated = true`.
4. **Step 1c — repair the `planning/` symlink inside the worktree** (run from the MAIN repo root, for
   every worktree path — fresh create, re-attach, or reuse alike). Detect vaulting first:
   ```
   [ -L planning ] && echo SYMLINK || echo PLAIN
   python3 -c "import os; print(os.path.realpath('planning'))"
   ```
   - If `planning` IS a symlink (a brain-vaulted repo — e.g. `agentic-portfolio` HQ), its target is
     RELATIVE (`planning -> ../_planning/<repo>`) and breaks once you're inside `trees/[branchName]/`.
     Repoint the worktree's own `planning` at the SAME vault via an ABSOLUTE symlink (never a relative
     one, and never a real directory — that would clobber the link on merge):
     ```
     TARGET="$(python3 -c "import os; print(os.path.realpath('planning'))")"
     rm -f trees/[branchName]/planning
     ln -s "$TARGET" trees/[branchName]/planning
     ```
   - If `planning` is a plain tracked directory (not vaulted), do nothing — sparse-checkout already
     populated it.
5. **In-place mode** (no `--worktree`): `branchName = currentBranch`, `wasCreated = false`,
   `runDir = repoRoot`. Skip Steps 1b/1c entirely.
6. **Compute `runDir`**: `repoRoot/trees/<branchName>` under `--worktree`, else `repoRoot`.
7. **Report pipeline-start inputs**, all run from `runDir`:
   - **Spec source AND location (D65 stage 2 + tier resolution)** — the block record is checked
     FIRST and is preferred; `tasks.md` is only a fallback for a legacy spec that predates the
     block-record migration. Check the **root** first — it always wins whenever the spec exists at
     both locations: `ls <blockRecordFile>` then `ls <specFile>` (both root-relative, as computed in
     Step 0). ONLY IF `candidateTierPrefix` (from Step 1.1) is non-empty, ALSO check the tier
     location: `ls <candidateTierPrefix><blockRecordFile>` then `ls <candidateTierPrefix><specFile>`.
     Resolve, in order:
     - `specFoundInTier` = true ONLY when the spec exists at NEITHER root path AND exists at either
       tier path. Otherwise false — this is what makes the root win when the spec is present at both.
     - `specSource`, evaluated at the WINNING location (root unless `specFoundInTier`):
       `"block-record"` if that location's block record exists; else `"tasks-md"` if that location's
       legacy file exists; else `"missing"`.
     - `specFileExists` = true iff `specSource != "missing"`.
     - **If `specFoundInTier` is true**, re-derive `blockDir`, `blockRecordFile`, `specFile`,
       `tasksJsonFile`, `breakdownFile`, `reportsDir` and `stateFile` (Step 0's list) by prefixing
       each with `candidateTierPrefix` — e.g. `blockDir = "<candidateTierPrefix>planning/<blockId>"`
       — BEFORE proceeding to the `specSource == "block-record"` reassignment below. Every later step
       then reads these tier-qualified paths exactly as it would the root ones.
     - When `specSource == "block-record"`, reassign `specFile := blockRecordFile` (using whichever
       — root or tier — form `blockRecordFile` now holds) for every step below.
   - Block status: `grep -iE "<blockId>" planning/status.md | head -5` (title-case Status, or
     `"Unknown"` if no row found).
   - **D19 thin-spec gate** — evaluate ONLY when `specSource == "tasks-md"` (the legacy prose path —
     a block-record spec is authored structured JSON, so the `{{TOKEN}}`/section checks below do not
     apply to it) AND this is a **fresh** run (never on `--resume`). Flag thin ONLY on high-confidence
     signals — a false positive blocking a valid spec is far costlier than a missed one:
     - Any unfilled `{{TOKEN}}` in `<specFile>` (`grep -n '{{' <specFile>`).
     - The `## Acceptance Criteria` section has no real `- ` bullet (empty, or only a template seed).
     - Do NOT flag bare `TODO`/`TBD` prose, do NOT treat `<...>` as a token (legitimate in `Vec<T>` /
       globs), never flag the Amendment Log seed `_No amendments yet._`.
     - If thin: **abort immediately** — `ABORTED (D19)`, report the reason, and tell the user to flesh
       out the spec (or `/generate-tasks --force`) and re-run. Do not proceed to Step 2.
   - Capture the **emoji-gate diff base**: `baseSha = git rev-parse --short HEAD` — the HEAD sha as it
     stands right now, before any task commits. The emoji gate itself now diffs each of THIS run's
     own recorded commit SHAs against its own parent, not `baseSha..HEAD`; `baseSha` survives only
     as the cannot-scope fallback check (Step 6 below) and as `state.base_sha` for `/close-out`'s
     in-place fallback.
- If the spec is found at NEITHER the root nor the tier location: abort — `Missing spec`, and name
  BOTH the root paths AND (when `candidateTierPrefix` was non-empty) the tier paths that were
  searched — naming only the root reads as "the spec was never written" when the real cause may be
  "the engine looked in the wrong place". Tell the user to run `/generate-tasks <blockId>` (and
  `/breakdown`), commit, then re-run.

From here on, every Bash call in every later step is prefixed with `cd <runDir> &&` — shell state does
not persist between calls.

### Step 2 — Plan: enumerate tasks (D16 lint) + load resume state + load the harness policy

1. **D16 preflight lint.** Read `<tasksJsonFile>`. It MUST parse as a **non-empty bare JSON array** of
   task objects (each with at least `task_id`; matches the SDLCTask shape, not wrapped in an object).
   If it's missing, invalid, or an empty array: **before aborting, check the matching D16 derive
   fallback for this run's `specSource`** (from Step 1 above) — the two fallbacks are mutually
   exclusive per run, not tried in sequence:

   - **`specSource == "block-record"` → derive-from-block-record fallback.** Read
     `<blockRecordFile>` (`planning/blocks/<blockId>.json`, per `block.schema.json`). If it parses as
     JSON and carries a non-empty `what` plus a non-empty `acceptance_criteria` array, author a FRESH
     D45-shaped `tasks.json` from the record's `what` (scope), `why` (intent), `files.new`/
     `files.modified` (task ownership — keep tasks disjoint), `acceptance_criteria`,
     `testing_strategy`, and `validation_commands` fields (a real decomposition, never a verbatim
     copy of the record's prose, never the superseded D44 `{"tasks": [...]}` wrapper — bare array,
     1-indexed integer `task_id`, `description` a single string, `max_attempts: 3`, never author
     `status`/`attempt_count`), write it, and commit it on the current branch with an explicit
     pathspec (`git add <tasksJsonFile>`,
     `git commit -m "chore: derive tasks.json from block record (D16 fallback)"`). Log a distinct
     line — `Derived tasks.json from block record (D16 derive-from-block-record fallback) — <N>
     task(s), commit <hash>.` — then re-run this lint. **Only if the block record is also missing,
     invalid, or has no derivable `what`/`acceptance_criteria`, abort** — `ABORTED (D16)`.

   - **`specSource == "tasks-md"` → derive-from-tasks.md fallback (legacy path).** If `<specFile>`
     (`tasks.md`) exists and carries a `## Step-by-Step Tasks` / `## Step by Step Tasks` section with
     at least one numbered step, author a FRESH D45-shaped `tasks.json` from that decomposition plus
     the spec's Acceptance Criteria / Validation Commands (same D45 shape rules as above), write it,
     and commit it on the current branch with an explicit pathspec (`git add <tasksJsonFile>`,
     `git commit -m "chore: derive tasks.json from tasks.md (D16 fallback)"`). Log a distinct line —
     `Derived tasks.json from tasks.md (D16 derive-from-tasks.md fallback) — <N> task(s), commit
     <hash>.` — so a derived spec is distinguishable from an authored one, then re-run this lint.
     **Only if `tasks.md` is also missing, or has no derivable step content, abort** —
     `ABORTED (D16)`.

   Either way, tell the user to run `/generate-tasks <blockId>` to author `tasks.json`, commit, then
   re-run. Deriving from an authored block record or `tasks.md` is not guessing the task structure;
   fabricating one from nothing is what D16 still refuses to do.

   **Per-task `validation_commands` scoping**: When authoring tasks.json, follow the convention
   documented at `.claude/commands/generate-tasks.md` (search it for "validation_commands"); do not
   restate the rubric in your own words, just apply it. The rule:
   `validation_commands` is `[]` for any task that touches source the project's checks compile or
   lint — those tasks fall back to the project-wide harness checks, which are authoritative for them.
   Set it ONLY for a task that CANNOT break the build (docs-only, config-only, fixture-only), with
   cheap commands that actually verify that task (e.g. file exists, frontmatter present, index
   updated). If you DO author an override that runs tests, it MUST target that task's own tests
   specifically — never a bare/positional filter that could silently match zero or the wrong tests —
   and a command matching nothing must fail rather than pass. Never hardcode a stack-specific
   command (e.g. a particular test runner invocation) into this; that judgment belongs to whoever
   derives or authors the task at run time. Match the intent of the parallel generator in sdlc-block.js
   ("acceptance_criteria/validation_commands can stay [] per task").
   - `allTasks` = every `task_id`, in array order.
   - **Per-task validation override**: for each task whose `validation_commands` is a non-empty array,
     remember `{taskId, validationCommands}`. Per
     [D63](../../../planning/decisions/D63-per-task-validation-commands-augment-gating.md),
     `/sdlc-task` treats this as **augment-gating-only** — it never causes a `gates:true` harness
     check to be skipped. This task's test stage runs the project's `gates:true` harness checks
     (fast form) **in addition to** these commands, copied verbatim; nothing is replaced. (Only if
     `harness.json` defines zero `gates:true` checks does this task run solely its own commands —
     see Step 3's test-step bullet for how that edge case is reported, never silent.) Every other
     task still uses the harness/spec checks below, unchanged.
   - **Engine-parse-safety scan**: for each task, check its `files` array for any path under
     `.claude/workflows/`. Remember `{taskId, files: [...matching paths only...]}` for every task that
     has one. This produces an **unconditional, hardcoded gate** later (independent of
     `harness.json`): any such task's test stage always adds one extra check per matching file —
     `node --check <file>` — that gates the verdict, in both fast and full test depth.
2. Apply the task selection (if any) to `allTasks` to get `taskList`. If nothing matches, stop and
   report an empty selection.
3. **Resume-load** (only under `--resume`): read `<stateFile>`.
   - Missing/invalid → log that no valid state was found and run every selected task fresh.
   - Valid → collect every task number whose `tasks["<N>"].status == "passed"` into a skip-set; those
     tasks are skipped entirely in the per-task loop (logged, not re-run). Also read `bail_reason` for
     context. **Also copy the file's entire top-level `tasks` object verbatim into the in-memory
     `state.tasks` map before the per-task loop starts** — the loop below only ever writes
     `state.tasks[N]` for tasks it actually runs this invocation, so a skipped/already-passed task
     never re-enters it on its own. Without this seed, the very next state write (Step 3.3) would
     serialize `state` wholesale and silently drop every earlier-passed task from the committed file,
     and a *second* resume would then see them as never-passed and re-run them. This is the same fix
     `sdlc-flow.js`/its SKILL.md already carry; `tasks_run` is a different field and is NOT merged this
     way — it stays per-invocation telemetry (see Step 3.3).
4. **Load `planning/harness.json`** (from `runDir`) if present and valid JSON — this project's
   validation policy, `validation.checks[]`. Each check has a `kind` (default `command`; also
   `baseline-diff`, `count-delta`, `warning-scan`, `forbidden-pattern-scan`,
   `skip-count-regression`), a `command`, `gates` (bool), and `perTask` (bool, default true). If
   `harness.json` is absent, unreadable, or has no checks, remember that — every test stage below
   falls back to running the spec's own `## Validation Commands` section, in order (or, if the spec
   has none either, a single informational no-op check).
5. **Resolve test depth**: `--test-depth` flag if given, else `fast`.
6. **Snapshot baselines** (resume-safe — never overwrite an existing baseline) for every
   `baseline-diff` / `skip-count-regression` check that declares a `baselineCommand`, BEFORE any task
   runs:
   - `baseline-diff` → write `<reportsDir>/<slug>-baseline.json` (its `baselineCommand`'s stdout).
   - `skip-count-regression` → write `<reportsDir>/<slug>-skip-baseline.txt` (bare integer count).
   - `mkdir -p <reportsDir>` first; if the file already exists, keep it and log "BASELINE EXISTS
     (kept)" instead of overwriting.

### Step 3 — Per-task loop (sequential, one task at a time)

For each `taskNum` in `taskList` (skip any already in the resume skip-set, logging the skip):

1. `stem = "<blockId>-task<taskNum>"`. Track per-task state: `status`, `attempts`, `summary`,
   `issues[]`, `fixes[]`, `decisions[]`, `files_changed[]`, `commit`, `validated`.
2. **Attempt loop, up to 3 attempts** (`attempt` 1..3):
   - Attempt 1 = **implement**; attempts 2–3 = **fix**. On the FINAL attempt (attempt 3), escalate the
     acting model to Opus (attempts 1–2 use Sonnet) — this is the one and only escalation point.
   - **Implement/fix step**: read `CLAUDE.md` + `planning/context.md`; read `<specFile>` +
     `<tasksJsonFile>` and find the entry whose `task_id == taskNum`; on a fix pass, make the MINIMUM
     targeted change addressing the previous failure's output (do not re-implement from scratch); if
     `<breakdownFile>` exists, use its `### Step <taskNum>:` sub-steps as a finer execution guide
     (`tasks.json` stays authoritative for scope). Run the D8 completeness self-check (no
     `todo!()`/`unimplemented!()`/`NotImplementedError`/`not implemented`/`FIXME` on any in-scope
     path). Run the spec's validation commands for this task to confirm correctness locally — the
     `## Validation Commands` section in prose (`tasks-md` source), or the `validation_commands`
     field in the JSON block record (`block-record` source).
   - **Commit** (never `git add -A`/`git add .` — stage files explicitly by name):
     ```
     git commit -m "$(cat <<'EOF'
     feat: implement <stem>
     EOF
     )"
     ```
     (fix pass: `fix: fix pass <attempt-1> for <stem>`, e.g. attempt 2's fix commit reads
     `fix: fix pass 1 for <stem>`.) Capture the short hash via `git log --oneline -1`.
   - **Vault-aware commit (D46 — if planning/ is a vaulted symlink)**: Planning/ is a relative symlink pointing to
     a brain-owned vault repository (e.g., agentic-portfolio HQ). Its bytes live at a DIFFERENT git repo,
     invisible to the commit made above. If this attempt created or edited ANY file under planning/
     (i.e., it belongs in filesModified with a "planning/" prefix), you MUST ALSO stage and commit it
     through the real vault path — derive the exact set from what you actually wrote, never a fixed list
     of filenames. NEVER run git add -A, git add ., git reset, or git stash against the vault repo —
     another session may have unrelated work staged there right now; touch ONLY your own paths, and
     do not checkout/switch/branch inside it (stay on whatever branch it is already on). For each such file,
     let <relpath> be the part of its path AFTER "planning/":
       ```
       git -C <vault.planningPath> add <vault.planningPath>/<relpath>
       ```
     Then, once every such path is staged, commit ONLY those paths — pass them explicitly to `git commit`
     itself (not merely to `git add`), so a sibling lane's unrelated pre-staged files are never swept
     into this commit even if they happen to already be staged:
       ```
       git -C <vault.planningPath> diff --cached --quiet -- <relpath1> <relpath2> ... || git -C <vault.planningPath> commit -m "$(cat <<'EOF'
     fix: fix pass <attempt-1> for <stem> (vault)
     EOF
     )" -- <relpath1> <relpath2> ...
       git -C <vault.planningPath> log --oneline -1
       ```
     If NOTHING you wrote this attempt lives under planning/, skip this step entirely — do not run any
     vault command. If a vault add/commit fails, report it PLAINLY in notes; never paper over it, and
     never "repair" it by committing on a different branch inside the vault.
     **NOTE**: If planning/ is a plain tracked directory (not vaulted), skip this step entirely — the commit
     above already covered everything.
   - If the implement/fix agent step produced nothing usable (a dead/empty turn): treat this exactly
     like a test failure below (triage it as `NULL_RESULT — the agent died or returned nothing`) —
     do NOT silently retry without triaging.
   - **Vault-commit verification (D46 amendment, BRAIN_ROOT-aware)**: After a vault commit step (if it ran), the
     engine independently re-verifies that every `planning/`-prefixed path a stage self-reported as modified is
     actually COMMITTED — tracked with no staged or unstaged diff. This is not about trusting the stage output —
     a valid commitHash proves nothing about the vault half (observed live: one run returned a valid commitHash
     that covered only the source half, with the vault edit silently uncommitted). A `planning/...`-shaped path is
     ambiguous, though: it may belong to THIS repo's own vault, or — for a command whose own design authors
     directly at HQ (e.g. `/generate-roadmap`, whose Step 1A states "this command runs at HQ") — to the BRAIN
     ROOT repo's `planning/` instead, a different git repository on disk. The check classifies each path into one
     of three buckets: `VAULT_OK` (exists and is committed under this repo's own vault path), `BRAIN_ROOT_OK`
     (does not exist in this repo's vault at all, but exists and is committed under the brain root — found by
     walking up from the vault path to the nearest ancestor containing `brain.toml`), or `UNCOMMITTED` (exists
     nowhere, or exists but is not committed wherever it does exist). **The classification itself runs inside a
     single deterministic Bash script the agent executes verbatim and transcribes** — not agent-driven per-path
     branching. A cheap model given the branching logic as prose reliably follows only the first branch and
     silently skips the brain-root fallback for every path that isn't in this repo's own vault (observed live:
     Haiku checked the vault path for all 6 paths and never attempted the fallback for the 4 that weren't there,
     misclassifying legitimate brain-root writes as failures). Only `UNCOMMITTED` paths are a failure;
     `BRAIN_ROOT_OK` paths are a legitimate cross-repo write, not a vault-commit defect. A vault-commit failure
     surfaces exactly like a test failure: the task is never marked passed on this attempt; triage decides whether
     to RETRYABLE (fix and try again, ≤3 times) or MAJOR (bail to a human right now).
   - **Test step** — run ONLY the applicable check set, never invent checks:
     - If this task declared its own `validation_commands` override (Step 2.1) — **D63,
       augment-gating-only**: run the project's `gates:true` harness checks (fast form —
       `fastCommand`, or `command` if no `fastCommand` is set), numbered first, **PLUS** this task's
       own override commands, numbered to continue the sequence, all gating. Nothing is skipped; the
       override is additive. If `harness.json` defines zero `gates:true` checks, there is nothing of
       the harness's own to add — this task then runs only its own override commands, and the
       `validated:` label records this explicitly as **"ran none of the harness list (tasks.json
       override, /sdlc-flow end review will reconcile)"**, logged to terminal output too (never
       folded silently into a bare "validated" claim). Otherwise, on a normal project, the label is
       **"substituted a documented subset (gates:true checks still ran)"**.
     - Else, render the harness checks (label **"ran the harness list"**): if `testDepth == fast`,
       filter to checks with `gates:true` AND `perTask !== false`; if `full`, run the whole
       `validation.checks[]` list. If no `harness.json`/no matching checks, fall back to the spec's
       `## Validation Commands` in order (or one informational no-op row if the spec has none).
     - **Always additionally add** the engine-parse-safety gate for any `.claude/workflows/` file this
       task's `files[]` names (Step 2.1): `node --check <file>` per file, gating, regardless of
       harness.json.
     - Handle each `harness.json` check `kind` per its own semantics:
       - `command` (default) / `count-delta`: run the command, check exit code; `fastCommand`
         replaces `command` when set and `testDepth == fast`.
       - `baseline-diff`: run `command`, diff its JSON output against the pre-run baseline snapshot
         (Step 2.6) on the declared `compareKeys`; **fails ONLY on net-new items absent from the
         baseline** — pre-existing items are never a failure.
       - `skip-count-regression`: run `command` to get the current skip count; **fails ONLY when the
         current count is GREATER than the pre-run baseline** (coverage silently switched off) —
         never fails on a merely-nonzero absolute count.
       - `warning-scan`: run `command`, gate on its own exit code, then grep its output against
         `warningPatterns`; if `gates:true`, a pattern match ALSO fails the check; if `gates:false`,
         matches are informational only (record them, don't fail).
       - `forbidden-pattern-scan`: for every `rules[]` entry, grep `pattern` over `paths` (optionally
         minus an `allowlistPattern`); the check passes only if EVERY rule is clean.
     - **Always additionally run the emoji-gate** (a harness rule, unconditional — not read from
       `harness.json`): DIFF-SCOPED to this run's own recorded commit SHAs, never the whole
       `<baseSha>..HEAD` range — it judges only lines **added** by commits THIS run itself made, so
       neither a legacy file's pre-existing emoji nor a concurrent sibling session's commit on a
       shared in-place branch can fail a diff this run never touched. For each commit SHA recorded
       in `state.tasks[].commit` (in memory, not re-read from disk — disk writes only happen after
       a task passes, and this gate runs after the commit is made but before the write), run
       `git diff -M -U0 <commit>^..<commit> -- '*.md' '*.mdx'` and scan only `+` content lines
       (never the `+++`/`---` header lines) for emoji. If no commits are recorded but
       `<baseSha>..HEAD` is non-empty, refuse to pass — an unscoped range is a sign that the
       run-state never initialized the commit list correctly. A pure rename with no added content
       lines passes, a brand-new file with an emoji fails (its added lines are its whole content).
     - The task PASSES this attempt only if every gating check passed AND the emoji gate is clean.
   - **On pass**: mark the task `passed`, record which check set validated it, and stop the attempt
     loop for this task (do not run further attempts).
   - **On failure**: **triage** the failure before deciding whether to retry:
     - Classify **RETRYABLE** (transient/infra flake, OR the failure visibly changed from the previous
       attempt — evidence of progress, a bounded fix can plausibly close it) vs **MAJOR** (bail to a
       human right now). "When unsure, BAIL."
     - **Immediate-bail (MAJOR) reasons** — any of these ends the task NOW, without spending the
       remaining attempts:
       1. Missing/undefined upstream dependency or symbol the spec assumes exists.
       2. Spec ambiguity/contradiction — the intended behavior is genuinely undeterminable.
       3. Environment/credential/auth/network failure (not a code defect).
       4. The needed change would be destructive or out-of-scope.
       5. The SAME failure twice with no progress (stuck), or a structural design flaw needing a
          re-plan.
     - Before asserting a failure "pre-dates this task" / "exists at baseline" / "is unrelated to this
       task's scope": that claim requires re-running ONLY the failing check against the base state
       (main working tree, or the task's base commit) FIRST. If actually re-run, record
       `baseStateChecked=true` and the real result as `evidence`; if it cannot be re-run in context,
       set `baseStateChecked=false` and phrase the claim explicitly as an unverified hypothesis, never
       as observed fact. Treat harness-created workspace state (the worktree, sparse-checkout, copied
       `.env` files, the repaired `planning/` symlink) as a candidate cause, not a fixed backdrop —
       an identical failure before/after a change is not evidence of pre-existence when both runs
       share the same possibly-broken environment.
     - `evidence` must be what was actually OBSERVED (quoting the failing output) — no causal
       guessing.
     - If MAJOR: **break the loop immediately** for this task — do not burn the remaining attempts —
       record the bail reason, mark the run blocked, and stop the whole per-task loop (subsequent
       tasks in `taskList` do not run this pass).
     - If RETRYABLE and this is attempt 3 (the last one): the loop is naturally exhausted — bail
       anyway, with a fallback reason noting all 3 attempts failed. This is a different bail path from
       MAJOR but has the same effect: the run stops.
     - If RETRYABLE and attempts remain: record the triage reason as a "fix" note and loop to the next
       attempt.
3. **State write — once per task, disk-only, never committed by any git command.** After the task's
   attempt loop resolves (passed, or bailed), write the full accumulated `state` object (spec_slug,
   mode, branch, worktree_path, status, current_task, tasks_run, the per-task `tasks{}` map,
   bail_reason, and the token roll-up) to `<stateFile>` via a plain file write — `mkdir -p
   <blockDir>/sdlc` first, preserve `started_at` from the file if one already exists (else stamp now),
   always refresh `updated_at`. **`tasks{}` must be the merged map** — the tasks carried forward from
   Step 2.3's resume-load (already-passed tasks from a prior invocation) union the tasks this
   invocation actually ran, keyed by task number; never write only this invocation's tasks. `tasks_run`
   is the opposite — it is deliberately PER-INVOCATION telemetry ("what did THIS invocation run") and
   is never unioned with a prior invocation's `tasks_run`; only `tasks` is the cumulative resume
   breadcrumb. **Never run `git add`/`git commit`/`git checkout`/`git switch`/`git branch` on this
   file** — it is read back off disk only, by `--resume`, never out of git history.
4. If this task bailed, stop the per-task loop entirely (do not proceed to the next `taskNum`).

### Step 3.5 — Terminal authoritative reconcile (D56)

`fullRun` = true iff no task selection was given (every task in the spec ran this pass this run).

Run this step only if the run did **not** bail, `fullRun` is true, AND `testDepth == 'fast'`. In
every other case (bailed; a partial task-subset run; `--test-depth full`, where every check
already ran authoritative on every per-task pass via the same `gatingOnly:false` codepath) skip it
entirely — proceed straight to Step 4.

This is the ONE point in the run where a check's real, authoritative `command` — never
`fastCommand` — and every `perTask: false` gating check are actually verified. The per-task
tripwire in Step 3 never runs either form: it always renders with `gatingOnly:true`, which
substitutes `fastCommand` for `command` when one is configured, and drops every `perTask: false`
check from the per-task list entirely.

1. Build the reconcile check set from `planning/harness.json`'s `validation.checks[]`: every check
   with `gates:true` AND (`fastCommand` is set and differs from `command`, OR `perTask === false`).
   This is deliberately narrow, not a full re-run of every gating check — a check with no
   `fastCommand` already ran its authoritative `command` on every per-task pass, so re-running it
   here buys zero new coverage at real cost (see D56's measurement.md).
2. If the set is empty (no `fastCommand` substitutions, no `perTask:false` checks in this
   project), log that the reconcile is a no-op and skip straight to Step 4 — zero added cost.
3. Otherwise, run exactly that check set with each check's real `command` (never `fastCommand`)
   and no `perTask` filtering — the same rendering Step 3 uses, but with gating set to run every
   check's authoritative form unconditionally. All Bash calls run from `runDir`.
4. If every check in the set passes: log the reconcile passed and proceed to Step 4 normally.
5. If any check fails: set `reconcileFailed = true` and the run's terminal status to
   `"reconcile_failed"` (a distinct terminal state — never folded into an ordinary `"blocked"`
   bail). **Skip Step 4 (bookkeep) entirely** — do not mark tasks done, do not flip
   `planning/status.md` or `planning/state.json`, do not commit. All per-task commits already made
   stand untouched; there is no task to attribute the failure to and no per-task attempt budget
   left to spend retrying it here. Proceed straight to Step 5 to persist this outcome and report
   it. Resuming later (`--resume`, no task selection) re-enters with every task already `"passed"`
   in state.json, so it naturally re-runs only this reconcile step, not the task loop.

### Step 4 — Lean bookkeep close-out (only on a non-bailed, non-reconcile-failed run)

Skip this entire step if the run bailed OR Step 3.5 set `reconcileFailed = true`. Otherwise:

- `blockDone` = true iff `!reconcileFailed` AND every task in `allTasks` (the FULL spec, not
  `taskList`, which is only this run's selected subset) has passed — comparing against `taskList`
  would be trivially true on a subset run, so this is derived from `allTasks` and no longer gated by
  `fullRun` (BT.ticket.resume-cannot-close-its-block). This is only safe because `state.tasks`
  survives a `--resume` (BT.ticket.sdlc-task-resume-truncates-run-state) — before that fix, a
  resumed run's `state.tasks` held only the resumed tasks and this comparison could not be trusted.
  When declining to close, name the outstanding task numbers in the report.
- **Re-detect the vault** (same check as Step 1c, from `runDir`):
  `[ -L planning ]` → symlink (vaulted) vs plain directory; resolve the real path via
  `python3 -c "import os; print(os.path.realpath('planning'))"`.
1. Mark every passed task done in `<specFile>` (Edit tool), using the spec's existing task-done marker
   convention if it has one (e.g. a leading `[done]`); if the spec has no such convention, leave it
   and note that tasks weren't marked. **Never remove or alter a marker already present from a prior
   run** — this step only ADDS markers for the tasks that passed in this run. After marking, COUNT
   the CUMULATIVE total: how many of the spec's tasks now carry a done marker (this run's plus every
   prior run's combined), out of the spec's total task count. This run's own tally (this run's passed
   count out of this run's selected task count) is only a slice — never use that slice alone as "how
   many tasks are done" anywhere a count is written; use the cumulative count derived from the file
   itself. A resumed two-task spec must read "2 of 2" after its second run, not "1 of 1".
2. Update `planning/status.md` (surgical Edit). **"Current focus" is APPEND-ONLY narrative** — never
   delete or rewrite any existing line under it; a prior block's narrative must survive this edit
   VERBATIM. The one exception: if an existing line already refers to THIS spec by name (e.g. from an
   earlier partial run), replace only that one line — never the whole section. If `blockDone`, flip
   this spec's Status to "Done" in the Progress Table and add ONE new line under "Current focus"
   recording that outcome, citing the cumulative task count from step 1 (e.g. "`<blockId>`: done (N of
   M tasks)"); otherwise keep Status "In progress" (a task subset ran) and optionally add a new line
   under "Current focus" pointing at the next task, citing the same cumulative count. Refresh "Last
   updated" to today's date.
3. **Flip the block's status in `planning/state.json`, validate-then-commit** — skip silently if
   there's no `planning/state.json`, OR if `blockDone` is false. Resolve the canonical block id from
   the status.md Progress Table row (the only judgment call in this step), then run this exact
   scripted mutation (never a hand Edit). `json.load()` succeeding is **not** schema validity — mev
   deserializes into typed structs, so a scalar where a struct belongs parses fine as JSON and fails
   the whole file (this is what happened 2026-08-09: a string `origin` where the schema types it as a
   struct). The script therefore captures the pre-write bytes, mutates in memory, runs `mev
   validate-brain --state` BEFORE and AFTER the write, and rejects — byte-exact rollback — any write
   that introduces diagnostic lines not present in the BEFORE baseline. Pre-existing corpus errors
   (a sibling lane's unrelated breakage) never block the write — **net-new only**, the same
   delta-attribution rule the push gate uses under D64. This validation runs identically whether or
   not `--worktree` is in effect: `mev validate-brain --state` reads this repo's own
   `planning/state.json` directly and does not need the cross-repo `BRAIN_ROOT` resolution that makes
   `--graph`/`emit-state --write` unsafe inside a linked worktree — only step 4's `emit-state --write`
   is deferred in worktree mode, never this validation:
   ```
   python3 -c "
   import json, subprocess, sys, shutil

   path = 'planning/state.json'
   bid = sys.argv[1]

   with open(path, 'rb') as fh:
       pre_bytes = fh.read()

   data = json.loads(pre_bytes)
   found = False
   for track in data.get('tracks', []):
       for block in track.get('blocks', []):
           if block.get('id') == bid:
               block['status'] = 'closed'
               found = True
               break
       if found:
           break

   if not found:
       print('NOT_FOUND')
       sys.exit(0)

   mev_available = shutil.which('mev') is not None

   def diagnostics():
       r = subprocess.run(['mev', 'validate-brain', '--state'], capture_output=True, text=True)
       lines = (r.stdout + r.stderr).splitlines()
       return set(l for l in lines if l.strip().startswith('[E_') or l.strip().startswith('[W_'))

   if not mev_available:
       with open(path, 'w') as fh:
           json.dump(data, fh, indent=2, ensure_ascii=False)
           fh.write(chr(10))
       print('FLIPPED:' + bid)
       print('UNVALIDATED: mev not on PATH -- schema check skipped, write landed with only json.load-level parsing')
       sys.exit(0)

   baseline = diagnostics()

   with open(path, 'w') as fh:
       json.dump(data, fh, indent=2, ensure_ascii=False)
       fh.write(chr(10))

   after = diagnostics()
   net_new = after - baseline

   if net_new:
       with open(path, 'wb') as fh:
           fh.write(pre_bytes)
       print('REJECTED:' + bid)
       for line in sorted(net_new):
           print('NET_NEW: ' + line)
       sys.exit(1)

   print('FLIPPED:' + bid)
   " "<RESOLVED_ID>"
   ```
   Read the script's own stdout AND exit code — do not infer success yourself:
   - `NOT_FOUND` (exit 0) — file stays byte-unchanged; report it, never fabricate a block entry.
   - `FLIPPED:<id>` with no `UNVALIDATED:` line (exit 0) — mev validated the write, no net-new
     diagnostics; treat the block as closed.
   - `FLIPPED:<id>` WITH an `UNVALIDATED:` line (exit 0) — mev is not installed, the write landed
     unchecked (a degrade, matching how the harness treats other absent tooling); report the
     UNVALIDATED line verbatim.
   - `REJECTED:<id>` (exit 1) — net-new schema errors; state.json is rolled back byte-exact to its
     pre-write content. Report every `NET_NEW:` line verbatim and do **not** treat the block as
     closed this run, even though `blockDone` said yes — a later run must flip it once the underlying
     cause is fixed.
4. **Regenerate derived surfaces** — run this whenever bookkeep runs at all, NOT only when `blockDone`
   (status.md/tasks.md already changed either way). This includes `state.json`'s top-level `focus`
   object (e.g. `focus.next`) — step 3's scripted mutation above touches only the matched block's
   `status` field and never `focus`, so `focus.next` is stale until this step runs:
   - **In-place (no `--worktree`)**: run `mev emit-state --write` from `runDir`. This re-derives
     `focus.next` from the just-flipped block status. If `mev` or `brain.toml` is absent (standalone
     repo), skip silently.
   - **`--worktree`**: do NOT run `mev emit-state --write` — it refuses to run inside a linked
     worktree. `focus.next` is therefore **DEFERRED**, not updated — it still points at the block
     that just closed until `/clean-worktree` or `/merge-train` runs `mev emit-state --write` on
     merge. Report this explicitly (do not report the run as leaving `focus` fresh); do not attempt
     to hand-edit `focus.next` here.
5. **Commit** (stage explicitly — never `git add -A`). Never run `git checkout`/`git switch`/`git
   branch` outside this repo's own root, or (when vaulted) outside the vault's own root — if a `git
   add` fails, report it; do not relocate the commit to force it through.
   - **Vaulted repo (`planning/` is a symlink — D46, e.g. this very `agentic-portfolio` HQ)**: the
     spec, `status.md`, and `state.json` bytes all live in the vault repo, NOT this one. Stage and
     commit them there via `git -C <vaultRealPath>`, on whatever branch the vault repo is already on —
     **never** a plain `git add planning/...` from this repo root (fails: "pathspec is beyond a
     symbolic link"), and never a `git checkout`/branch operation inside the vault:
     ```
     git -C <vaultRealPath> add <vaultRealPath>/<blockId>/tasks.md 2>/dev/null || true
     git -C <vaultRealPath> add <vaultRealPath>/status.md
     git -C <vaultRealPath> add <vaultRealPath>/state.json 2>/dev/null || true
     Then commit ONLY these three paths — pass them explicitly to `git commit` itself (not merely to
     `git add`), so anything a sibling lane already had staged in this same vault repo is left staged
     and untouched by this commit:
     git -C <vaultRealPath> diff --cached --quiet -- <vaultRealPath>/<blockId>/tasks.md <vaultRealPath>/status.md <vaultRealPath>/state.json || git -C <vaultRealPath> commit -m "$(cat <<'EOF'
     chore: sdlc-task bookkeep — <blockId>
     EOF
     )" -- <vaultRealPath>/<blockId>/tasks.md <vaultRealPath>/status.md <vaultRealPath>/state.json
     git -C <vaultRealPath> log --oneline -1
     ```
     This must be a clean, targeted commit of just those files' changes — not a broader checkout or
     working-branch manipulation of the vault.
   - **Non-vaulted repo (`planning/` is a plain tracked directory)**: commit together as usual —
     ```
     git add <specFile> planning/status.md
     git add planning/state.json 2>/dev/null || true
     git commit -m "$(cat <<'EOF'
     chore: sdlc-task bookkeep — <blockId>
     EOF
     )" || echo "NOTHING_TO_COMMIT"
     git log --oneline -1
     ```
   Do NOT write a `log.md` narrative entry or a D18 amendment log here — that is `/log-work`'s job.

### Step 5 — Final state write + report

- Write `<stateFile>` one final time (same disk-only rules as Step 3.3), with `status` set to
  `"blocked"` (bailed), `"reconcile_failed"` (Step 3.5's authoritative reconcile failed), or
  `"done"` (otherwise), capturing the final token roll-up. On `"reconcile_failed"`, also set
  `bail_reason` to the reconcile's failing check names + a tail of their output.
- Report to the user:
  - Which tasks passed / bailed, and the final branch (plus the worktree path, under `--worktree`).
  - **On bail**: point the user at `<stateFile>` for the per-task detail, tell them to fix the
    blocker, then **re-run with `--resume`** to pick back up (already-passed tasks are skipped; the
    existing worktree/branch is reused by name; the D19 thin-spec gate is skipped on resume).
  - **On a reconcile failure (D56)**: tell the user all per-task commits stand — only the terminal
    authoritative reconcile failed. Point them at the failing check output, tell them to fix it,
    then **re-run with `--resume`**: every task is already `"passed"` in state.json, so the resumed
    run skips straight to Step 3.5 and re-runs only the reconcile (never re-runs the task loop).
    A reconcile failure must never be reported as a clean finish — the block is NOT done.
  - **On a clean finish**: under `--worktree`, remind the user the branch still needs integrating
    (`git checkout main && git merge <branchName>`, then remove the worktree/branch); in-place, note
    the commits already landed on the current branch.
  - Either way, remind the user to run **`/log-work`** afterward for the narrative `log.md` entry —
    the lean bookkeep close-out above only flips status markers, it never writes prose.










































