# Clean Worktree — Merge a completed SDLC worktree branch into main and remove it.

## Variables

$ARGUMENTS — one of:
- `<spec-slug>`            → worktree name: `<spec-slug>`
- `<spec-slug> 3`          → worktree name: `<spec-slug>-task3`
- `<spec-slug>-task8`      → literal worktree name (already lowercased, single token)
- `<spec-slug>-task8-2`    → literal worktree name with suffix (from sdlc-task auto-increment)

The literal single-token form is output by `/sdlc-task` when it creates a suffixed worktree
(e.g. `-2`, `-3`). Pass it exactly as printed.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and print usage:
   ```
   Usage: /clean-worktree <spec-slug> [task-N]
          /clean-worktree <literal-worktree-name>
   Examples:
     /clean-worktree <spec-slug>
     /clean-worktree <spec-slug> 3
     /clean-worktree <spec-slug>-task8
     /clean-worktree <spec-slug>-task8-2
   ```

2. **Parse arguments:**
   - If `$ARGUMENTS` is a **single token with no spaces** that is already lowercase and contains
     only letters, digits, dots, and hyphens → treat it as the literal `worktreeName` directly.
     (Spec slugs carry a phase-dotted prefix, e.g. `<spec-slug>-task8`.)
   - Otherwise, apply the same transform as `/init-worktree`: lowercase `specSlug`, append
     `-task<taskNum>` if a second token is a number. Derive `worktreeName`.
   - Always: `worktreePath = trees/<worktreeName>`
   - Extract `taskNum` from `worktreeName` for use in the task log step:
     - Pattern: `<spec-slug>-task<N>` or `<spec-slug>-task<N>-<suffix>`
     - Extract `taskNum` as the integer after the last `-task` in the name.
     - `logFile = planning/<spec-slug-placeholder>/sdlc/reports/task<taskNum>-log.md`
     - **Note:** the actual `specSlug` (with original casing) is read from the log file itself
       (`**Spec:** <spec-slug>`) rather than derived from the lowercased branch name.
       Use the branch name only to locate the approximate path; then read the log's `**Spec:**`
       field and use that value as the authoritative spec slug for all status.md path lookups.

3. **Check if the worktree exists:**
   ```bash
   git worktree list | grep <worktreePath>
   ```
   If not found, skip to step 8 (branch-only cleanup).

4. **SAFETY — Show uncommitted changes in the worktree:**
   ```bash
   git -C <worktreePath> status --short
   git -C <worktreePath> diff --stat
   ```
   If there are uncommitted changes, print:
   ```
   WARNING: The worktree has uncommitted changes (shown above).
   These will be permanently lost if the worktree is removed.
   Proceed anyway? (yes/no)
   ```
   Stop and wait for user confirmation. If the user says anything other than "yes", abort.

5. **SAFETY — Show unpushed commits on the worktree branch:**
   ```bash
   git -C <worktreePath> log --oneline main..HEAD
   ```
   Display this list clearly. These are the commits that will be merged into `main`.

6. **Merge the branch into main (fast-forward only):**

   Run from the main working tree (not from within the worktree):
   ```bash
   git merge --ff-only <worktreeName>
   ```

   **If fast-forward succeeds:** continue to step 7.

   **If fast-forward fails** (main has advanced since the worktree was created):
   - Print the divergence: `git log --oneline <worktreeName>..main` to show which commits on main are not in the worktree branch.
   - Stop. Do NOT delete the worktree or branch.
   - Print resolution options:
     ```
     Fast-forward merge failed. The worktree branch is behind main.
     The worktree at <worktreePath> has been left intact.

     To resolve, choose one of:
       Option A — Create a merge commit:
         git merge <worktreeName>

       Option B — Rebase the worktree branch onto main (from inside the worktree):
         cd <worktreePath>
         git rebase main
         cd <back to repo root>
         /clean-worktree <original-args>   ← retry after rebasing
     ```

6.5. **Apply task log (if present) — update status.md and log.md:**

   This step only applies when a task number was identified in `worktreeName`.

   ```bash
   ls <logFile> 2>/dev/null && echo "LOG_EXISTS" || echo "NO_LOG"
   grep "^\*\*Applied:\*\* false" <logFile> 2>/dev/null && echo "NOT_YET_APPLIED" || echo "ALREADY_APPLIED"
   ```

   **If log file exists AND `Applied: false`:**

   a. Read `<logFile>` in full.
   b. Extract the `**Spec:**` field from the log file to get the authoritative spec slug
      (e.g. `<spec-slug>`). Use this value — not the lowercased branch name — for
      all `planning/<spec-slug>/` path lookups.
   c. If `## status.md — Spec Status` section is present → flip the spec's Status column
      in `planning/status.md` progress table to the value specified (e.g. "In progress").
      Omit this sub-step if that section is absent from the log file.
   d. Apply `## status.md — Current Focus Line` → replace the `**Current focus:**` line in
      `planning/status.md` with the exact string from the log.
   e. Apply `## status.md — Last Updated Line` → replace the `**Last updated:**` line in
      `planning/status.md` with the exact string from the log.
   f. Apply `## status.md — Notes Column` → update the Notes column of the matching
      spec row in `planning/status.md` with the text from the log.
   g. Prepend the **content under** `## Log Entry` to `log.md` — this means
      everything from the `## YYYY-MM-DD` date header line onward, NOT the `## Log Entry`
      section header itself. Insert it immediately after the `# Log —` header line
      (before existing entries), preserving a blank line between the new entry and the one below.
   g.5. **Apply the amendment log (D18).** If the log file has a `## Amendment Log Entry (D18)` section
      whose body is NOT exactly `_none_`, append each `- YYYY-MM-DD [<stage>] ...` line from it to the
      `## Amendment Log` section of the spec at `planning/<spec-slug>/tasks.md` (append-only, below any
      existing lines; if that section still reads only `_No amendments yet._`, replace that placeholder
      with the line(s)). Merging tasks in task-number order keeps the amendment lines chronological.
      Omit this sub-step if the section is absent or `_none_`.
   h. Edit `<logFile>`: change `**Applied:** false` → `**Applied:** true`.
   i. Stage and commit (include the spec only if step g.5 modified it):
      ```bash
      git add planning/status.md log.md <logFile>
      git add planning/<spec-slug>/tasks.md 2>/dev/null || true
      git commit -m "$(cat <<'EOF'
      chore: apply task log for <stem>
      EOF
      )"
      ```
   j. Report: "status.md and log.md updated from task log." (Note if amendment lines were applied to the spec.)

   **If `Applied: true`:** report "Task log already applied — skipping STATUS/Log update."

   **If log file not found:** report "No task log found — STATUS/Log not updated.
   If this task was run with /sdlc-task, check that the pipeline completed its wrap-up stage."

6.6. **Regenerate derived surfaces (`mev emit-state --write`):**

   The merged branch may carry an authored `planning/state.json` block-status flip to `"closed"`
   (written by `/sdlc-flow`'s or `/sdlc-task`'s in-worktree wrap-up, which could not run `emit-state`
   inside the linked worktree). Now that it has landed on `main`, regenerate every derived surface
   from the authored graph — this is the one-way derivation (`focus`, rollups, cache `synced_from`
   watermarks, tier tables, the HQ Operating Board, `master-plan.md` wave tables):
   ```bash
   mev emit-state --write
   ```
   Run it from the main working tree (never a linked worktree — `emit-state` refuses there). If `mev`
   or `brain.toml` is absent (a standalone repo), skip this step silently — the authored flip already
   merged and still stands. Do NOT hand-reimplement any derived surface. If it reports a
   `W_EMIT_NO_SENTINEL` warning, surface it rather than hand-authoring the missing sentinel.

7. **Remove the worktree and delete the branch:**
   ```bash
   git worktree remove <worktreePath> --force
   git worktree prune
   git branch -D <worktreeName>
   ```

8. **Branch-only cleanup** (worktree directory not found, but branch may still exist):
   ```bash
   git branch --list <worktreeName>
   ```
   If the branch exists:
   - Show any commits not yet on main: `git log --oneline main..<worktreeName>`
   - Delete the branch: `git branch -D <worktreeName>`
   If neither worktree nor branch exists, report: "Nothing to clean — '<worktreeName>' does not exist."

9. **Verify cleanup — run these and display output:**
   ```bash
   git worktree list
   git branch --list <worktreeName>
   git log --oneline -6
   ```

10. **Report one of:**
    - **Success:** "Worktree '<worktreeName>' merged and cleaned. Branch deleted. Pipeline commits are now on main."
    - **Merge failed:** "Fast-forward failed. Worktree left intact at <worktreePath>. See options above."
    - **Branch-only cleanup:** "Worktree directory not found. Branch '<worktreeName>' deleted (if it existed)."
    - **Already clean:** "Nothing to clean — '<worktreeName>' does not exist."

## Notes

- **Merge before delete.** This command never deletes the worktree or branch until the merge succeeds. Work is preserved until confirmed safe.
- **Fast-forward only** is the correct default for this pipeline: worktrees branch from `main` at init time, run the full pipeline, and then merge back. If `main` has advanced concurrently (e.g., a hotfix), `--ff-only` fails with a clear error rather than silently creating a merge commit.
- **Uncommitted changes** in the worktree are unusual — the SDLC pipeline commits after each stage. If they appear, it likely means the pipeline was interrupted mid-stage.
- Run this command from the **main repo session** (CWD: repo root), not from inside the worktree.
- **Task log (sdlc-task only):** When a task was run with `/sdlc-task`, the worktree branch contains a `task<N>-log.md` file instead of status.md/log.md changes. Step 6.5 reads that file and applies the updates to main after the merge. Always merge tasks in task-number order so status.md's "Current focus" ends up pointing to the right next task.
- **Suffix worktrees:** If `/sdlc-task` created `<spec-slug>-task8-2` (due to a collision), pass the full name as a single argument: `/clean-worktree <spec-slug>-task8-2`. The task log is still found at `planning/<spec-slug>/sdlc/reports/task8-log.md` (based on the task number extracted from the branch name).
- **`/sdlc-block` does its own merges.** Do not run `/clean-worktree` for tasks driven by `/sdlc-block` — it merges each wave for you. Use this command only for standalone `/sdlc-task` runs or manual worktrees from `/init-worktree`.
