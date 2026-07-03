# Merge Train — Merge the block branch train in dependency order.

Reads the `block-orchestration-state.json` produced by `/sdlc-block` (default/PR mode)
and merges each block's PR into the base branch in the recorded dependency order, halting
on the first unresolved conflict.

Run this after all PRs in the branch train have been reviewed (see `/review-PR`).

## Variables

$ARGUMENTS — optional. `[plan-slug]` — the plan slug (e.g. `sdlc-block-and-task-updates`)
used to locate `planning/<plan-slug>/sdlc/block-orchestration-state.json`. If omitted,
searches `planning/` for a unique state file.

Examples:
```
/merge-train
/merge-train sdlc-block-and-task-updates
```

## Instructions

### Step 1 — Locate the orchestration state

If `plan-slug` was provided, read directly:
```bash
cat planning/<plan-slug>/sdlc/block-orchestration-state.json
```

If not provided, search:
```bash
find planning -name "block-orchestration-state.json" 2>/dev/null
```

If multiple files are found and no slug was given, list them and stop:
```
Multiple block-orchestration-state.json files found. Specify the plan slug:
  /merge-train <plan-slug>

Found:
  planning/<slug1>/sdlc/block-orchestration-state.json
  planning/<slug2>/sdlc/block-orchestration-state.json
```

Read and parse the selected state file. Extract:
- `base_branch` — the branch to merge into (typically `main`).
- `train_branch` — the train branch name (for reference / cleanup notes).
- `merge_order` — ordered array of block slugs (topological dependency order).
- `blocks` — per-block map: `{ status, branch, pr: { number, url }, verdict }`.
- `mode` — `pr` | `auto-merge` | `no-pr`.

**If `mode` is `auto-merge`:** stop and report:
```
This plan was run with --auto-merge: blocks were merged into <base_branch> during
the /sdlc-block run. Nothing to do — check git log on <base_branch>.
```

**If `mode` is `no-pr`:** stop and report:
```
This plan was run with --no-pr: there are no PRs to merge via this command.
Merge block branches manually with `git merge`, or re-run /sdlc-block with --auto-merge.
```

If `merge_order` is empty, stop: "No blocks recorded in merge_order — nothing to merge."

### Step 2 — Pre-flight checks

```bash
git status --porcelain
git branch --show-current
```

Working tree must be **clean**. If not, stop:
```
Working tree has uncommitted changes. Stash or commit before running /merge-train.
```

If not on `base_branch`, check it out and pull:
```bash
git checkout <base_branch>
git pull --ff-only origin <base_branch>
```

If `git pull --ff-only` fails (non-linear history), stop and surface the divergence.
The user must resolve the local/remote divergence before proceeding.

### Step 3 — Build the merge plan

For each slug in `merge_order`, determine its status:

| slug | branch | PR # | State |
|---|---|---|---|

Classify each block as one of:
- `already-merged` — `blocks[slug].status` is `merged` in state, or the branch is no
  longer ahead of `base_branch` (`git merge-base --is-ancestor <branch> <base_branch>`).
- `ready` — PR exists, is `OPEN`, and `mergeable == MERGEABLE`.
- `needs-approval` — PR exists but has no `APPROVED` review (informational warning, not blocking).
- `has-conflicts` — PR is not mergeable (`mergeable == CONFLICTING`).
- `no-pr` — `blocks[slug].pr` is null (block ran without a PR despite the mode).
- `escalated` / `skipped` — block did not succeed in the run; note it but do not attempt a merge.

Check each non-already-merged block's PR:
```bash
gh pr view <pr.number> --json state,reviews,mergeable
```

**If any block is `has-conflicts`:** stop before any merge:
```
Cannot merge — PR #<N> (<slug>) has unresolved conflicts with <base_branch>.
Resolve the conflicts on branch <branch>, push, then re-run /merge-train.
```

Print a pre-merge summary table:
```
Merge plan (dependency order):
  1. <slug>   PR #<N>  branch: <branch>  <ready | already-merged | needs-approval | escalated>
  2. ...

Base branch: <base_branch>
Blocks to merge: <N>   Already merged: <N>   Skipped/escalated: <N>
```

If all remaining blocks are `already-merged`, report: "All blocks already merged into
<base_branch>." and stop (success).

**Warn** (do not stop) for `needs-approval` blocks:
```
WARNING: PR #<N> (<slug>) has no APPROVED review. Proceeding at your discretion.
```

**Confirm before merging.** Ask:
```
About to merge <N> PR(s) into <base_branch> in this order:
  <slug1> → <slug2> → ...

Proceed? (yes/no)
```

Stop and wait. If anything other than "yes", abort without merging anything.

### Step 4 — Merge each PR in order

For each slug in `merge_order` (in order) where status is `ready` or `needs-approval`:

```bash
gh pr merge <pr.number> --merge --delete-branch
```

(`--merge` creates a merge commit, preserving the block's history on `base_branch`.)

**If `gh pr merge` fails:**
- Print the full error output.
- Stop immediately. Do NOT continue to the next block.
- Report:
  ```
  Merge halted at block <slug> (PR #<N>).
  Blocks merged so far: <merged slugs, or "none">
  Blocks not yet merged: <remaining slugs>

  Resolve the issue, then re-run /merge-train to continue.
  Already-merged blocks will be detected as done and skipped automatically.
  ```

**If merge succeeds:**
- Report: `Merged: <slug> (PR #<N>) into <base_branch>`
- Sync the local base:
  ```bash
  git pull --ff-only origin <base_branch>
  ```

### Step 5 — Final report

```bash
git log --oneline -10
```

Print a summary:
```
Merge train complete.

Merged into <base_branch>:
  <slug1> — PR #<N>
  <slug2> — PR #<N>

Already on <base_branch> (skipped):
  <slug> — (already merged)

Could not merge (escalated/skipped during /sdlc-block run):
  <slug> — <reason from state>

<base_branch> is now up to date with the block train.
```

List any remaining block branches (orphaned — safe to delete manually):
```bash
git branch | grep "<planSlug>"
```

## Notes

- **Dependency order matters.** The `merge_order` array from `block-orchestration-state.json`
  was recorded in topological order by the orchestrator. Do not reorder — later blocks may
  have been authored on top of earlier ones.
- **Halt on conflict.** A merge conflict at block N means the subsequent blocks, which may
  depend on N's changes, cannot be safely merged without a clean base. Fix N first.
- **Resume is automatic.** Already-merged blocks are detected via the state file's `status`
  field and a live `git merge-base --is-ancestor` check. Re-running after a mid-train failure
  will skip the already-landed blocks and continue from where it left off.
- **`--delete-branch` on merge.** `gh pr merge --delete-branch` deletes the remote block
  branch after the merge. The local branch (if any) is left for cleanup by the user; the
  worktree is already gone (the child `/sdlc-flow` handles that at wrap-up).
- **No-PR mode.** If `/sdlc-block` ran with `--no-pr`, this command exits early. Use
  `git merge <branch>` directly or re-run the orchestrator with `--auto-merge`.
- **Auto-merge mode.** If `/sdlc-block` ran with `--auto-merge`, blocks were already landed
  during the run. This command exits early in that case.
- Run from the **main repo root**, not from inside a worktree.
