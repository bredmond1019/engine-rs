# Init Worktree — Create an isolated git worktree for an SDLC spec or task.

## Variables

$ARGUMENTS — spec slug with optional task number.

Examples:
- `<spec-slug>`   → worktree name: `<spec-slug>`   at `trees/<spec-slug>/`
- `<spec-slug> 3` → worktree name: `<spec-slug>-task3` at `trees/<spec-slug>-task3/`

The spec slug is the directory name under `planning/` (e.g. `<spec-slug>`,
`2.2-learn-paths-accuracy-refresh`). This matches the worktree naming `/sdlc-task` uses, so
`/clean-worktree` can find and merge whatever this command (or `/sdlc-task`) created.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and print usage:
   ```
   Usage: /init-worktree <spec-slug> [task-N]
   Examples:
     /init-worktree <spec-slug>
     /init-worktree <spec-slug> 3
   ```

2. **Parse arguments:** split `$ARGUMENTS` on whitespace. First token is `specSlug`. If a second token exists and is a number, it is `taskNum`; otherwise no task number.

3. **Derive worktree name:** lowercase `specSlug`, append `-task<taskNum>` if `taskNum` is set.
   - `<spec-slug>`    → `<spec-slug>`
   - `<spec-slug> 3`  → `<spec-slug>-task3`

4. **Verify CWD is repo root:**
   ```bash
   git rev-parse --show-toplevel
   ```
   If the output does not match the current directory, stop with: "Run this command from the repo root, not from inside a subdirectory."

5. **Check for name collision:**
   ```bash
   git worktree list
   git branch --list <worktreeName>
   ```
   - If `trees/<worktreeName>` appears in worktree list → stop: "Worktree '<worktreeName>' already exists. Run `/clean-worktree <args>` first."
   - If branch `<worktreeName>` exists but worktree directory does not → stop: "Branch '<worktreeName>' exists as an orphan. Delete it first with: `git branch -D <worktreeName>`"

6. **Create the trees directory:**
   ```bash
   mkdir -p trees
   ```

7. **Create the worktree without checkout:**
   ```bash
   git worktree add --no-checkout trees/<worktreeName> -b <worktreeName>
   ```

8. **Configure sparse checkout (cone mode):**
   ```bash
   git -C trees/<worktreeName> sparse-checkout init --cone
   # Cone ALL tracked top-level directories — stack-agnostic, no project layout assumptions.
   git -C trees/<worktreeName> sparse-checkout set $(git ls-tree HEAD --name-only -d | tr '\n' ' ')
   git -C trees/<worktreeName> checkout
   ```
   `git ls-tree HEAD --name-only -d` lists every tracked top-level **directory**, so the cone set
   adapts to whatever trees the project has (source, tests, `docs/`, `planning/`, `.claude/`, …)
   without naming any one stack's layout. Root-level files (`CLAUDE.md`, manifests/lockfiles,
   build/config files, etc.) are included automatically by cone mode.

9. **Discover and copy EVERY gitignored env-shaped file** (`.env`, `.env.local`, `.env.*` in any
   directory — config commonly lives below the repo root, e.g. `app/.env`, not just at root).
   Preserve each file's path relative to the repo root (creating parent directories as needed —
   so `app/.env` lands at `trees/<worktreeName>/app/.env`). Only copy files git actually ignores;
   exclude `node_modules/`, `.venv/`, `venv/`, `trees/`, and `vendor/`; never overwrite a file that
   already exists in the worktree — a copy step that pulls in an unexpected file is a worse
   failure than a missing one:
   ```bash
   git ls-files --others --ignored --exclude-standard -- . | grep -E '(^|/)\.env(\.[^/]*)?$' | grep -Ev '(^|/)(node_modules|\.venv|venv|trees|vendor)/' | while IFS= read -r f; do dest="trees/<worktreeName>/$f"; if [ ! -f "$dest" ]; then mkdir -p "$(dirname "$dest")"; cp "$f" "$dest"; echo "ENV_COPIED: $f"; fi; done
   ```
   Record the `ENV_COPIED:` lines printed above — report them in step 13 below, so a run missing
   config says so at setup time instead of surfacing later as a confusing downstream failure (e.g.
   a fallback database connection producing "column does not exist" errors).

10. **Create initial empty commit to establish the branch head:**
    ```bash
    git -C trees/<worktreeName> commit --allow-empty -m "chore: init worktree <worktreeName>"
    ```

11. **Repair the `planning/` symlink** (run from the MAIN repo root). In brain-vaulted repos
    (D46) the main repo's `planning` is a **relative** symlink into a vault (e.g.
    `planning -> ../_planning/<repo>`). `git worktree add` copies that link verbatim, but the
    worktree sits at a different depth, so the relative path no longer resolves from there — an
    agent hitting the dangling link tends to delete it and write a real `planning/` dir in its
    place, silently forking planning state away from the vault. Point the worktree's `planning/`
    at the same real vault via an **absolute** symlink instead (gitignored, so it is never
    committed or merged). This mirrors `sdlc-flow.js` STEP 3.5 / `sdlc-task.js` STEP 2c:
    ```bash
    if [ -L planning ]; then
      TARGET="$(python3 -c "import os; print(os.path.realpath('planning'))")"
      rm -f trees/<worktreeName>/planning
      ln -s "$TARGET" trees/<worktreeName>/planning
      echo "PLANNING_SYMLINK_FIXED -> $TARGET"
    else
      echo "PLANNING_REAL_DIR (no symlink fix needed)"
    fi
    ```
    If `planning` is a real tracked directory (non-vaulted repo), the sparse-checkout already
    populated it — the `else` branch above fires and nothing further is needed. Re-running this
    step against an already-repaired worktree is a safe no-op: `planning` in the main repo root is
    still the same symlink, so the same absolute target is recomputed and re-linked.

12. **Verify — run these and display the output:**
    ```bash
    git worktree list
    git -C trees/<worktreeName> sparse-checkout list
    ls trees/<worktreeName>/
    git -C trees/<worktreeName> log --oneline -1
    ls trees/<worktreeName>/planning/ >/dev/null 2>&1 && echo "PLANNING_OK"
    ```

13. **Report success** and print next-step instructions, including the env files seeded in step 9
    (or a note that none were found) and the worktree's actual path:
    ```
    Worktree '<worktreeName>' ready at trees/<worktreeName>/

    Env files copied:
      <one line per ENV_COPIED: entry from step 9, e.g. "app/.env" — or "none found" if empty>

    To run the SDLC pipeline in isolation:
      1. Open a new Claude Code session with working directory set to:
           <absolute-path-to-repo>/trees/<worktreeName>
      2. Run: /sdlc-run <specSlug>[ <taskNum>]

    Note: install the project's dependencies in the worktree before any build/test runs:
      cd trees/<worktreeName> && <install command per project>   (dependencies are NOT shared across worktrees)

    Note: the worktree path is derived from the spec slug (<worktreeName>), not any block ID —
    if you need to locate it from outside this session, use `git worktree list` rather than guess.

    When the pipeline is done, return to the main repo session and run:
      /clean-worktree <original-args>
    ```

## Notes

- Sparse checkout cones **all tracked top-level directories** (step 8), so `planning/` is included in full (the scout, plan, and wrap-up agents read status.md / master-plan.md and write report files) along with every source/content/asset tree the project has — no per-project tuning needed.
- `.claude/` is included so all commands and workflows resolve correctly when the CWD is the worktree.
- Root-level files are included automatically by cone mode — no need to list them explicitly.
- **Dependencies are not part of the checkout and are not shared between worktrees.** Install the project's dependencies inside the worktree before running its validation suite. (`/sdlc-task` handles this itself; only matters for a manual session.)
- Every gitignored env-shaped file under the repo (`.env`, `.env.local`, `.env.*`, at any depth)
  is gitignored and must be copied manually (step 9) — not just the root pair, since some
  projects keep config below the root (e.g. `app/.env`).
- **`planning/` symlink repair (step 11) only applies to brain-vaulted repos** (D46) — repos
  where `planning` is a symlink into `../_planning/<repo>`. In a non-vaulted repo `planning` is a
  real tracked directory and the step is a documented no-op. This keeps parity with the same
  repair the SDLC engines perform (`sdlc-flow.js` STEP 3.5, `sdlc-task.js` STEP 2c) so a
  manually created worktree behaves identically to a pipeline-created one.
- All `git commit` calls inside the pipeline will commit to branch `<worktreeName>`, not `main`, because git detects the worktree context automatically.
- When the pipeline finishes, run `/clean-worktree` from the main repo session to merge the branch and clean up.
