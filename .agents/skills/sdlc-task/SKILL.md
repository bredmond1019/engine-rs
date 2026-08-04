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
   → commit → lean bookkeep close-out
 and nothing else. No scout, no separate review, no document stage, no ui-test, no
 PR. The bookkeep close-out is deliberately lean: on a passing full run it flips the
 authored status markers (tasks.md task status, the status.md Progress row, the
 state.json block status) and — in place, on main — runs `mev emit-state --write`; it
 does NOT write a log.md narrative, a D18 amendment log, or run review/docs/PR. Run
 /log-work for the narrative. When you need a consolidated review + docs + a PR, use
 /sdlc-flow; for a whole spec in place, /sdlc-run; for a roadmap, /sdlc-block.

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
     → per-task loop → lean bookkeep close-out (on pass) → final state commit

   Per-task loop (sequential):
     implement → fast-test → (triage → fix/bail) ×≤3 → one state write per task
   A triage MAJOR / immediate-bail reason breaks straight out (does NOT burn the
   remaining attempts); the run stops and reports for human pickup.

 STATE (NOT gitignored, but deliberately never committed — at planning/<spec>/sdlc/)
   sdlc-task-state.json   the authoritative run index (per-task summary/issues/fixes/commit +
                          the Block-A `tokens` block). Written to disk after every task and
                          again at the end (cat-visible for crash inspection); read back off
                          disk only, by --resume — never out of git — so it is disk-only, never
                          committed (D46: planning/ may be a vaulted symlink into the brain repo,
                          where a plain `git add planning/...` fails).

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

- `<spec-slug>` (required) — call it `blockId`. Paths derived from it:
  - `blockDir` = `planning/<blockId>`
  - `specFile` = `<blockDir>/tasks.md`
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
   - Spec file exists? `ls <specFile>`.
   - Block status: `grep -iE "<blockId>" planning/status.md | head -5` (title-case Status, or
     `"Unknown"` if no row found).
   - **D19 thin-spec gate** — evaluate ONLY when the spec file exists AND this is a **fresh** run
     (never on `--resume`). Flag thin ONLY on high-confidence signals — a false positive blocking a
     valid spec is far costlier than a missed one:
     - Any unfilled `{{TOKEN}}` in `<specFile>` (`grep -n '{{' <specFile>`).
     - The `## Acceptance Criteria` section has no real `- ` bullet (empty, or only a template seed).
     - Do NOT flag bare `TODO`/`TBD` prose, do NOT treat `<...>` as a token (legitimate in `Vec<T>` /
       globs), never flag the Amendment Log seed `_No amendments yet._`.
     - If thin: **abort immediately** — `ABORTED (D19)`, report the reason, and tell the user to flesh
       out the spec (or `/generate-tasks --force`) and re-run. Do not proceed to Step 2.
   - Capture the **emoji-gate diff base**: `baseSha = git rev-parse --short HEAD` — the HEAD sha as it
     stands right now, before any task commits. Every later test stage diffs against this sha.
- If the spec file is missing entirely: abort — `Missing spec`, tell the user to run
  `/generate-tasks <blockId>` (and `/breakdown`), commit, then re-run.

From here on, every Bash call in every later step is prefixed with `cd <runDir> &&` — shell state does
not persist between calls.

### Step 2 — Plan: enumerate tasks (D16 lint) + load resume state + load the harness policy

1. **D16 preflight lint.** Read `<tasksJsonFile>`. It MUST parse as a **non-empty bare JSON array** of
   task objects (each with at least `task_id`; matches the SDLCTask shape, not wrapped in an object).
   If it's missing, invalid, or an empty array: **abort** — `ABORTED (D16)`, tell the user to run
   `/generate-tasks <blockId>` to author `tasks.json`, commit, then re-run. **Never infer the task
   count or structure from `tasks.md` prose** — `tasks.json` is the only source of truth for what
   tasks exist.
   - `allTasks` = every `task_id`, in array order.
   - **Per-task validation override**: for each task whose `validation_commands` is a non-empty array,
     remember `{taskId, validationCommands}` — this task's test stage runs ONLY these commands,
     **replacing** the harness gating checks for that task alone (copy the commands verbatim; every
     other task still uses the harness/spec checks below).
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
     context.
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
     path). Run the spec's `## Validation Commands` for this task to confirm correctness locally.
   - **Commit** (never `git add -A`/`git add .` — stage files explicitly by name):
     ```
     git commit -m "$(cat <<'EOF'
     feat: implement <stem>
     EOF
     )"
     ```
     (fix pass: `fix: fix pass <attempt-1> for <stem>`, e.g. attempt 2's fix commit reads
     `fix: fix pass 1 for <stem>`.) Capture the short hash via `git log --oneline -1`.
   - If the implement/fix agent step produced nothing usable (a dead/empty turn): treat this exactly
     like a test failure below (triage it as `NULL_RESULT — the agent died or returned nothing`) —
     do NOT silently retry without triaging.
   - **Test step** — run ONLY the applicable check set, never invent checks:
     - If this task declared its own `validation_commands` override (Step 2.1): run exactly those
       commands, each one gating.
     - Else, render the harness checks: if `testDepth == fast`, filter to checks with `gates:true`
       AND `perTask !== false`; if `full`, run the whole `validation.checks[]` list. If no
       `harness.json`/no matching checks, fall back to the spec's `## Validation Commands` in order
       (or one informational no-op row if the spec has none).
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
       `harness.json`): diff `git diff --name-only <baseSha>..HEAD`, inspect every changed
       `.md`/`.mdx` file, and fail the check on any stray emoji in docs.
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
   always refresh `updated_at`. **Never run `git add`/`git commit`/`git checkout`/`git switch`/`git
   branch` on this file** — it is read back off disk only, by `--resume`, never out of git history.
4. If this task bailed, stop the per-task loop entirely (do not proceed to the next `taskNum`).

### Step 4 — Lean bookkeep close-out (only on a non-bailed run)

Skip this entire step if the run bailed. Otherwise:

- `fullRun` = true iff no task selection was given (every task in the spec ran this pass).
- `blockDone` = true iff `fullRun` AND every task in `taskList` passed.
- **Re-detect the vault** (same check as Step 1c, from `runDir`):
  `[ -L planning ]` → symlink (vaulted) vs plain directory; resolve the real path via
  `python3 -c "import os; print(os.path.realpath('planning'))"`.
1. Mark every passed task done in `<specFile>` (Edit tool), using the spec's existing task-done marker
   convention if it has one (e.g. a leading `[done]`); if the spec has no such convention, leave it
   and note that tasks weren't marked.
2. Update `planning/status.md` (surgical Edit): if `blockDone`, flip this spec's Status to "Done" in
   the Progress Table and update "Current focus"; otherwise keep it "In progress" (a task subset ran)
   and optionally point "Current focus" at the next task. Refresh "Last updated" to today's date.
3. **Flip the block's status in `planning/state.json`** — skip silently if there's no
   `planning/state.json`, OR if `blockDone` is false. Resolve the canonical block id from the
   status.md Progress Table row (the only judgment call in this step), then run this exact scripted
   mutation (never a hand Edit) — it searches every `tracks[].blocks[]` entry and only ever mutates
   the one matching block's `status` field, leaving the file byte-unchanged on a miss:
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
   " "<RESOLVED_ID>"
   ```
   Read the script's own stdout to decide success — on `NOT_FOUND`, report it; never fabricate a
   block entry. Validate the file is still valid JSON afterward.
4. **Regenerate derived surfaces** — run this whenever bookkeep runs at all, NOT only when `blockDone`
   (status.md/tasks.md already changed either way):
   - **In-place (no `--worktree`)**: run `mev emit-state --write` from `runDir`. If `mev` or
     `brain.toml` is absent (standalone repo), skip silently.
   - **`--worktree`**: do NOT run `mev emit-state --write` — it refuses to run inside a linked
     worktree. Derived surfaces regenerate on MAIN when the branch merges (`/clean-worktree` or
     `/merge-train`).
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
     git -C <vaultRealPath> diff --cached --quiet || git -C <vaultRealPath> commit -m "$(cat <<'EOF'
     chore: sdlc-task bookkeep — <blockId>
     EOF
     )"
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
  `"blocked"` (bailed) or `"done"` (otherwise), capturing the final token roll-up.
- Report to the user:
  - Which tasks passed / bailed, and the final branch (plus the worktree path, under `--worktree`).
  - **On bail**: point the user at `<stateFile>` for the per-task detail, tell them to fix the
    blocker, then **re-run with `--resume`** to pick back up (already-passed tasks are skipped; the
    existing worktree/branch is reused by name; the D19 thin-spec gate is skipped on resume).
  - **On a clean finish**: under `--worktree`, remind the user the branch still needs integrating
    (`git checkout main && git merge <branchName>`, then remove the worktree/branch); in-place, note
    the commits already landed on the current branch.
  - Either way, remind the user to run **`/log-work`** afterward for the narrative `log.md` entry —
    the lean bookkeep close-out above only flips status markers, it never writes prose.
































