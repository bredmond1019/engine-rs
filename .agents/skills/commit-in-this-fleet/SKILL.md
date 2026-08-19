---
name: commit-in-this-fleet
description: >
  How to commit in the agentic-portfolio fleet without swallowing another session's work — why
  one git repo owns every planning/ directory, why `git add -A` is banned here rather than
  merely discouraged, what `git commit -o` does and does not stage, and the worktree and
  symlink traps that make a clean-looking commit wrong. Use BEFORE any git
  add/commit/stash/reset/mv in this fleet, before committing after `mev emit-state --write`,
  and whenever `git status` shows changes you did not make.
---

# Committing in this fleet

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

Committing here is not ordinary git. **One repo owns every `planning/` directory in the fleet**, and
several agents write to it at once. The default git verbs are wrong by construction.

## The one fact everything else follows from

Every `planning/` dir — including every `core/<repo>/planning` — is a **symlink into
`core/_planning/<repo>/`, tracked by the single HQ repo**. Check it and see:

```bash
git -C core/bastion/planning rev-parse --show-toplevel   # -> agentic-portfolio, NOT bastion
```

So a sub-repo working tree spans **two** git repos: the repo's own files, and HQ's planning vault.
`git status` in the sub-repo shows you one of them. That is the whole trap.

## The rules

### 1. Never `git add -A`, `git add .`, `git reset`, or `git stash` here

Not style — these stage the **whole index**, which includes any other session's in-flight work in the
shared vault. This has happened repeatedly and is recorded as a fleet trap
(`bastion-web:emit-state-rewrites-sibling-repos`): one lane's `emit-state` run modified
`core/_planning/bastion/state.json`, `core/_planning/engine-rs/state.json`, `README.md` and
`client/_planning/brazilianportugui/status.md`, *several carrying other sessions' uncommitted work* —
a new block, a dependency-edge removal, hand-written focus prose. A bare `git add -A` after that
sweeps all of it into your commit.

**Always commit with an explicit pathspec:**

```bash
git commit -o <path1> <path2> -m "..."
```

### 2. `-o` commits only ALREADY-TRACKED changes under those paths

This is the part that looks like it worked and did not. Measured behaviour:

| Situation | What happens |
|---|---|
| Path has tracked modifications | Committed. Correct. |
| Path contains **new, untracked** files | **They are not committed.** No error if some *other* tracked file matched the same pathspec — the commit succeeds and looks complete. |
| **No** tracked file matches the pathspec at all | `error: pathspec '<p>' did not match any file(s) known to git` and **nothing is committed** — not even the other paths on the same command line. |

So for new files: `git add <paths>` first, then `git commit -o <same paths>`. Then **verify**:

```bash
git status --short <paths>     # must be empty afterwards
```

### 3. Before you commit, look at what you did not touch

`git status` after a corpus-wide operation will show files you never edited — `mev emit-state --write`
regenerates the whole spine, not just your repo. Read the list and scope your pathspec to what you
actually own, leaving the rollups unstaged for whoever owns them. That is what a correct lane did:
pathspecs scoped to its own repo's tree plus its own cache doc and epic file, and nothing else.

### 4. `git mv` fails through the `planning/` symlink

It reports `source directory is empty`. Move against the **real** path
(`core/_planning/<slug>/<name>`), never the symlinked face (`core/<slug>/planning/<name>`).

### 5. A commit from an SDLC engine cannot include `planning/` edits

The engines commit to the *repo*, but `planning/` belongs to HQ — so a task that edits `planning/`
leaves that change uncommitted **while still reporting the task passed**
(`engine-rs:sdlc-engines-cannot-commit-planning-vault-edits`, observed 2026-08-07). After any run
whose spec touches `planning/`, check the brain root by hand:

```bash
git -C <BRAIN_ROOT> status --short
```

### 6. Worktrees contaminate root-level gates

While `git worktree list` shows more than one entry, a root-level `npm test` / `npm run lint` globs
into `.claude/worktrees/` and reports **another agent's** in-flight failures as yours
(`bastiel:worktrees-under-repo-break-root-gate-globs` — 1388 lint errors and 11 test failures that
belonged to a different tree, a full diagnostic cycle wasted). Scope the command, or check
`git worktree list` before believing a red gate.

## Before you commit

- [ ] Pathspec is explicit — no `add -A`, no `add .`
- [ ] New files were `git add`ed, not just named in `-o`
- [ ] `git status --short <paths>` is empty afterwards
- [ ] Nothing in the commit belongs to another session (read the file list, don't assume)
- [ ] If the work touched `planning/`, `git -C <BRAIN_ROOT> status` was checked too
- [ ] No secrets. This is a private repo and several of its sub-repos are public

## If you already swept someone else's work in

Do not `git reset` — that is rule 1 again, and it will disturb the shared index further. Identify the
foreign paths, `git restore --source=HEAD~1 --staged --worktree` **those paths only**, or amend with a
corrected explicit pathspec. Then say so, so the owning session knows.
