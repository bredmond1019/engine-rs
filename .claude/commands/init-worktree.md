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

9. **Copy local env files if present** (both are gitignored and must be copied manually):
   ```bash
   if [ -f .env ]; then cp .env trees/<worktreeName>/.env; echo "Copied .env"; else echo ".env not found — skipping"; fi
   if [ -f .env.local ]; then cp .env.local trees/<worktreeName>/.env.local; echo "Copied .env.local"; else echo ".env.local not found — skipping"; fi
   ```

10. **Create initial empty commit to establish the branch head:**
    ```bash
    git -C trees/<worktreeName> commit --allow-empty -m "chore: init worktree <worktreeName>"
    ```

11. **Verify — run these and display the output:**
    ```bash
    git worktree list
    git -C trees/<worktreeName> sparse-checkout list
    ls trees/<worktreeName>/
    git -C trees/<worktreeName> log --oneline -1
    ```

12. **Report success** and print next-step instructions:
    ```
    Worktree '<worktreeName>' ready at trees/<worktreeName>/

    To run the SDLC pipeline in isolation:
      1. Open a new Claude Code session with working directory set to:
           <absolute-path-to-repo>/trees/<worktreeName>
      2. Run: /sdlc-run <specSlug>[ <taskNum>]

    Note: install the project's dependencies in the worktree before any build/test runs:
      cd trees/<worktreeName> && <install command per project>   (dependencies are NOT shared across worktrees)

    When the pipeline is done, return to the main repo session and run:
      /clean-worktree <original-args>
    ```

## Notes

- Sparse checkout cones **all tracked top-level directories** (step 8), so `planning/` is included in full (the scout, plan, and wrap-up agents read status.md / master-plan.md and write report files) along with every source/content/asset tree the project has — no per-project tuning needed.
- `.claude/` is included so all commands and workflows resolve correctly when the CWD is the worktree.
- Root-level files are included automatically by cone mode — no need to list them explicitly.
- **Dependencies are not part of the checkout and are not shared between worktrees.** Install the project's dependencies inside the worktree before running its validation suite. (`/sdlc-task` handles this itself; only matters for a manual session.)
- `.env` / `.env.local` are gitignored and must be copied manually (step 9).
- All `git commit` calls inside the pipeline will commit to branch `<worktreeName>`, not `main`, because git detects the worktree context automatically.
- When the pipeline finishes, run `/clean-worktree` from the main repo session to merge the branch and clean up.
