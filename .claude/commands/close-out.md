# Close Out — Verify test coverage, patch docs, and hand off cleanly.

Run this after `/sdlc-run`, `/sdlc-flow`, or any implementation session to close the
quality loop before handing off: run the full test suite, fill coverage gaps, patch stale
docs, then produce a clean `/handoff`.

## Variables

$ARGUMENTS — optional. Parsed left to right:
  - `--gap-check-only` — run Steps 1–3 only (validation + coverage + docs); skip Step 4
    (`/handoff`). Designed for automated per-block close-out from `/sdlc-block` where
    handing off mid-run makes no sense. Preserves all gating and coverage logic.
  - `--skip-coverage` — skip Step 2 (coverage scan + gap fill); use when coverage is
    already known good or was verified by a prior `/review-task`.
  - `--clean-worktree` — run Step 5 (clean-worktree) at the very end to merge a **worktree** branch
    into `main` and remove the worktree. Default is false (do not clean) to protect the "never
    auto-merge" rule.
  - `--merge-branch` — run Step 5b at the very end to merge the current **plain** (non-worktree)
    branch into the base and regenerate derived surfaces (`mev emit-state --write`) on the base. Use
    this for a `/sdlc-flow` run in its default branch mode (no worktree to remove). Default is false.
    Mutually exclusive with `--clean-worktree` (that one handles worktree branches; this one handles
    plain branches).
  - `--base <ref>` — optional. Explicit diff base (branch, tag, or commit-ish) for the Step 1 emoji
    gate and the Step 2a coverage sweep. Overrides the auto-resolution in Step 0.5 entirely — use it
    when that resolution would guess wrong, or when Step 0.5 has nothing to resolve from (no
    `flow.prBase`, no `origin/HEAD`, no local `main`/`master`) and refuses to run.
  - Remaining text — passed through verbatim as the narrative note to `/handoff`. If
    omitted, `/handoff` derives context from git history and status.md.

Examples:
  - (no args) — run all steps; `/handoff` derives the narrative
  - `--gap-check-only` — run Steps 1–3 only; no handoff (used by automated orchestration)
  - `--clean-worktree` — run all steps, and clean/merge the worktree at the end
  - `--merge-branch` — run all steps, and merge the current plain branch into the base + emit-state at the end
  - `--base develop` — scope the gates to `develop` instead of auto-resolving
  - `shipped D36 close-out command` — run all steps; pass note to `/handoff`
  - `--skip-coverage shipped D36` — skip coverage scan; pass note to `/handoff`

## Execution Model

Run inline — do NOT spawn a subagent. `/update-docs`, `/handoff`, and `/clean-worktree` are
invoked as Skill tool calls or commands from the main agent context; they have their own confirmation gates.

## Instructions

### Step 0 — Parse $ARGUMENTS

Strip `--gap-check-only` if present (record whether it was set — when set, Step 4 is skipped).
Strip `--skip-coverage` if present (record whether it was set).
Strip `--clean-worktree` if present (record whether it was set).
Strip `--merge-branch` if present (record whether it was set).
Strip `--base <ref>` if present (record the ref value; empty string if not passed).
If BOTH `--clean-worktree` and `--merge-branch` were passed, stop and tell the user they are mutually
exclusive (worktree branch vs plain branch) — pick one.
Treat the remainder as the handoff note (may be empty).

### Step 0.5 — Resolve the diff base for Steps 1 and 2

Both the emoji gate (Step 1) and the coverage sweep (Step 2a) must scope to the **same** base — a
hard-coded two-dot diff against a literal `main` is empty by definition whenever `HEAD` is the base
branch itself (the default state after an in-place run, a plain-branch run, or right after `--auto-merge`/
`--merge-branch` land), which reports a vacuous clean instead of "nothing considered." Resolve the
base **once**, from real evidence, and refuse to proceed if nothing resolves — never fall back to
the literal string `main`.

Run this once, substituting the `--base` value parsed in Step 0 for `BASE_ARG` (empty string if
`--base` was not passed):

```bash
BASE_ARG="<value of --base, or empty>"
RESOLVED_BASE=""

if [ -n "$BASE_ARG" ]; then
  if ! git rev-parse --verify "$BASE_ARG" >/dev/null 2>&1; then
    echo "CLOSE-OUT: --base '$BASE_ARG' does not resolve to a valid ref. Aborting."; exit 1
  fi
  RESOLVED_BASE="$BASE_ARG"
else
  CONFIGURED_BASE=$(python3 -c "
import json
try:
    cfg = json.load(open('planning/harness.json'))
    print(cfg.get('flow', {}).get('prBase', ''))
except Exception:
    print('')
")
  if [ -n "$CONFIGURED_BASE" ] && git rev-parse --verify "$CONFIGURED_BASE" >/dev/null 2>&1; then
    RESOLVED_BASE="$CONFIGURED_BASE"
  else
    ORIGIN_DEFAULT=$(git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@')
    if [ -n "$ORIGIN_DEFAULT" ] && git rev-parse --verify "$ORIGIN_DEFAULT" >/dev/null 2>&1; then
      RESOLVED_BASE="$ORIGIN_DEFAULT"
    elif git rev-parse --verify main >/dev/null 2>&1; then
      RESOLVED_BASE="main"
    elif git rev-parse --verify master >/dev/null 2>&1; then
      RESOLVED_BASE="master"
    fi
  fi
fi

if [ -z "$RESOLVED_BASE" ]; then
  echo "CLOSE-OUT: could not resolve a diff base — no --base given, no planning/harness.json flow.prBase, no origin/HEAD, no local main or master. Re-run with --base <ref>. Aborting."
  exit 1
fi

CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" = "$RESOLVED_BASE" ]; then
  # HEAD IS the resolved base — a two-dot/three-dot diff against it is empty by definition. The
  # only real evidence left is a merge commit (e.g. --auto-merge's `gh pr merge --merge`): its
  # first parent is the pre-merge base, so HEAD^1..HEAD is what the merge actually brought in.
  if git rev-parse --verify -q HEAD^2 >/dev/null 2>&1; then
    RANGE="HEAD^1..HEAD"
    echo "CLOSE-OUT: HEAD is base '$RESOLVED_BASE' via a merge commit — scoping to what it brought in: $RANGE"
  else
    echo "CLOSE-OUT: HEAD IS the base branch '$RESOLVED_BASE' with no merge commit to scope from — there is no diff range that means anything here. Refusing to report a vacuous clean. Re-run with --base <ref> naming the commit this session's work started from, or run /close-out from the feature branch before it merges. Aborting."
    exit 1
  fi
else
  RANGE="${RESOLVED_BASE}...HEAD"
fi

echo "$RANGE" > .git/CLOSE_OUT_RANGE
echo "CLOSE-OUT: resolved diff range = $RANGE"
```

If this script exits non-zero: **stop. Do not proceed to Step 1.** Surface its message to the user
verbatim — it already states what to do (pass `--base <ref>`, or run from the feature branch).

### Step 1 — Run the validation suite

Read `planning/harness.json`. Run every check listed in `validation.checks[]` in order
(lint, type, test, build). Then always run the universal emoji gate last, scoped to the range
resolved in Step 0.5:

```bash
python3 - "$(cat .git/CLOSE_OUT_RANGE)" <<'PYEOF'
import subprocess, re, sys
RANGE = sys.argv[1]
EMOJI = re.compile(r'[\U0001F300-\U0001FAFF\U00002600-\U000027BF]')
FOOTER = 'Generated with Claude Code'
diff = subprocess.run(['git','diff','-M','-U0', RANGE, '--', '*.md', '*.mdx'], capture_output=True, text=True).stdout.splitlines()
hits = []
cur_file = None
cur_line = None
for line in diff:
    if line.startswith('diff --git '):
        cur_file = None; cur_line = None
    elif line.startswith('+++ '):
        p = line[4:]
        cur_file = None if p == '/dev/null' else (p[2:] if p.startswith('b/') else p)
    elif line.startswith('@@'):
        m = re.match(r'@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@', line)
        cur_line = int(m.group(1)) if m else None
    elif cur_file and cur_line is not None and line.startswith('+') and not line.startswith('+++'):
        content = line[1:]
        if EMOJI.search(content) and FOOTER not in content:
            hits.append(f'{cur_file}:{cur_line}: {content.rstrip()[:100]}')
        cur_line += 1
if hits:
    print('EMOJI CHECK FAIL:'); [print(h) for h in hits[:25]]; sys.exit(1)
print(f'EMOJI CHECK: OK (diff-scoped added lines only, range={RANGE})'); sys.exit(0)
PYEOF
```

If any **gating** check (`gates: true`) fails, or the emoji gate fails:
- Surface the failure with the exact command and relevant output.
- **Stop. Do not proceed to Steps 2–4.**
- Tell the user: which check failed and what it produced; suggest `/fix <spec>` if a
  spec is in flight, or direct the failing command at the problem.
- Do NOT attempt to fix failures here — this command closes out done work, not in-flight work.

If all gating checks pass (non-gating failures are surfaced but don't block): proceed.

### Step 2 — Coverage gap scan (skip if `--skip-coverage` was passed)

Assess whether recently changed source files have adequate test coverage, and fill blocking
gaps.

**2a — Identify changed source files**

Run `git diff "$(cat .git/CLOSE_OUT_RANGE)" --name-only` — the **same** range Step 1 resolved in
Step 0.5; the coverage sweep must never diverge from the emoji gate's base. Filter to source files
only — exclude: `*.md`, `*.mdx`, `*.json`, `*.toml`, `*.yaml`, `*.yml`, `planning/`, `docs/`,
`scaffold/`.

If no source files changed (docs/config-only session): skip to Step 3 silently.

**2b — Classify each file**

For each changed source file, check whether the changed code paths are exercised by tests:
- Look for a sibling test file (`src/foo.rs` → inline `#[cfg(test)]` block or
  `tests/foo_test.rs`; `lib/foo.ts` → `lib/foo.test.ts` or `__tests__/foo.test.ts`).
- Skim the diff for new public functions, new exported types, new error branches, and new
  command/handler registrations — these are the test targets.

Classify as:
- **Adequate** — changed code is exercised by existing or clearly new tests.
- **Non-blocking gap** — private helper, trivial wrapper, constant, config change, or
  internal-only path with no direct user-observable behavior. Note; don't block.
- **Blocking gap** — new public function, exported type used in a call path, new error
  branch the caller can observe, or new CLI command/handler with zero test coverage.

**2c — Fill blocking gaps**

For each blocking gap:
- Write a minimal, targeted test that exercises the specific function or path. One test
  per gap. Use the project's existing test style (read a neighbor test file to match
  conventions). No mocking unless the code itself requires it.
- If a gap is too ambiguous to test confidently — complex multi-file setup, unclear
  invariant, would require a test harness that doesn't exist yet — **ask the user**:
  "No clear way to test `<symbol>` minimally. Write a skeleton, skip it, or note in
  handoff?" Do not guess; do not write a vacuous test that asserts nothing.

After writing any tests: re-run the gating checks from Step 1 to confirm the new tests
pass. If they fail: fix them before proceeding (you wrote them; they are yours to fix).

Record non-blocking gaps for the handoff note (Step 4).

### Step 3 — Patch documentation

Invoke the `/update-docs --patch` skill. Wait for it to complete.

### Step 4 — Hand off

**Skip this step if `--gap-check-only` was passed.** Instead, print a one-line summary:
`Gap-check complete. Gating: <PASS|FAIL>. Coverage gaps filled: <N>. Docs patched: <yes|no>.`

Otherwise, invoke the `/handoff` skill.

Pass the handoff note (the $ARGUMENTS remainder after stripping `--skip-coverage`, `--gap-check-only`, `--clean-worktree`, and `--merge-branch`). If non-blocking coverage gaps were found in Step 2, prepend a brief line to the note:

```
Coverage note: <comma-separated list of files with non-blocking gaps> — not blocking.
<original note, if any>
```

### Step 5 — Clean worktree (skip unless `--clean-worktree` was passed)

If `--clean-worktree` was passed:
1. Determine the current git branch name:
   ```bash
   git branch --show-current
   ```
2. If the current branch is `main`, print: "Already on main; skipping worktree cleanup." and skip this step.
3. Otherwise, run the `/clean-worktree` command for the current branch:
   ```
   /clean-worktree <branch-name>
   ```
   *Note: This will merge the branch into main and remove the worktree/branch. By default, close-out does NOT run this cleanup to protect the "never auto-merge" rule; it must be explicitly opted into via `--clean-worktree`.*

### Step 5b — Merge plain branch (skip unless `--merge-branch` was passed)

For a `/sdlc-flow` run in its **default branch mode**, the work lives on a plain branch checked out
in the main working tree — there is no worktree to remove, so `--clean-worktree` does not apply. This
step merges that branch into the base and regenerates derived surfaces, mirroring `/clean-worktree`'s
merge + emit-state steps without the worktree teardown.

If `--merge-branch` was passed:

1. Resolve the base branch (`main` unless `planning/harness.json` `flow.prBase` is set) as `<base>`,
   and the current branch:
   ```bash
   git branch --show-current
   ```
2. If the current branch is already `<base>`, print "Already on base; nothing to merge." and skip.
3. **Show what will merge** (the branch's commits not yet on the base):
   ```bash
   git log --oneline <base>..HEAD
   ```
4. **Merge into the base (fast-forward only):** switch to the base and fast-forward it to the branch.
   ```bash
   git checkout <base>
   git merge --ff-only <branch-name>
   ```
   - **If fast-forward succeeds:** continue to step 5.
   - **If fast-forward fails** (the base advanced since the branch was created): stop, delete nothing,
     print the divergence (`git log --oneline <branch-name>..<base>`) and the resolution options —
     Option A `git merge <branch-name>` (create a merge commit), or Option B rebase the branch onto
     `<base>` then re-run `/close-out --merge-branch`. Leave the branch intact. Do NOT proceed to
     step 5 or 6. (The branch-mode `/sdlc-flow` wrap-up already committed status.md / log.md / the
     amendment log on the branch, so a successful merge carries them onto the base automatically — no
     separate task-log application is needed.)
5. **Regenerate derived surfaces (`mev emit-state --write`):** the merged branch carries an authored
   `planning/state.json` block-status flip to `"closed"` (the branch-mode wrap-up deferred emit-state
   because it ran on the feature branch, not the base). Now that it has landed on `<base>`, regenerate
   every derived surface from the authored graph — the one-way derivation (`focus`, rollups, cache
   `synced_from` watermarks, tier tables, the HQ Operating Board, `master-plan.md` wave tables):
   ```bash
   mev emit-state --write
   ```
   Run it from the base branch (never a linked worktree — `emit-state` refuses there). If `mev` or
   `brain.toml` is absent (a standalone repo), skip this silently — the authored flip already merged
   and still stands. Do NOT hand-reimplement any derived surface. If it reports a `W_EMIT_NO_SENTINEL`
   warning, surface it rather than hand-authoring the missing sentinel.
6. **Delete the merged branch:**
   ```bash
   git branch -d <branch-name>
   ```
   Report: "Branch '<branch-name>' merged into <base> and deleted; derived surfaces regenerated."

## Context / Files to Read

- `planning/harness.json` — validation suite (checks + gating flags); also read for
  `flow.prBase` by Step 0.5's diff-base resolution
- `planning/status.md` — current focus (to scope coverage check to recent work)
- `.git/CLOSE_OUT_RANGE` — scratch file this run writes in Step 0.5 with the resolved diff range
  (e.g. `main...HEAD` or `HEAD^1..HEAD`); Steps 1 and 2a both read it so they can never diverge.
  Lives under `.git/`, so it is never tracked and needs no cleanup.
