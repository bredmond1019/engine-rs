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
  - `--no-review` — skip Step 2.5 (code review).
  - `--review-level <level>` — specify the level of code review (`low`, `medium`, `high`)
    to pass to `/code-review`. Defaults to `low`.
  - `--clean-worktree` — run Step 5 (clean-worktree) at the very end to merge the branch into `main`
    and remove the worktree. Default is false (do not clean) to protect the "never auto-merge" rule.
  - Remaining text — passed through verbatim as the narrative note to `/handoff`. If
    omitted, `/handoff` derives context from git history and status.md.

Examples:
  - (no args) — run all steps (including a low-level code review); `/handoff` derives the narrative
  - `--gap-check-only` — run Steps 1–3 only; no handoff (used by automated orchestration)
  - `--clean-worktree` — run all steps, and clean/merge the worktree at the end
  - `--no-review --clean-worktree` — skip the code review, run all other steps, and clean the worktree
  - `--review-level medium` — run all steps, performing a medium-level code review
  - `shipped D36 close-out command` — run all steps; pass note to `/handoff`
  - `--skip-coverage shipped D36` — skip coverage scan; pass note to `/handoff`

## Execution Model

Run inline — do NOT spawn a subagent. `/update-docs`, `/handoff`, `/code-review`, and `/clean-worktree` are
invoked as Skill tool calls or commands from the main agent context; they have their own confirmation gates.

## Instructions

### Step 0 — Parse $ARGUMENTS

Strip `--gap-check-only` if present (record whether it was set — when set, Step 4 is skipped).
Strip `--skip-coverage` if present (record whether it was set).
Strip `--no-review` if present (record whether it was set).
Strip `--review-level <level>` if present (record the `<level>` string; defaults to `low`).
Strip `--clean-worktree` if present (record whether it was set).
Treat the remainder as the handoff note (may be empty).

### Step 1 — Run the validation suite

Read `planning/harness.json`. Run every check listed in `validation.checks[]` in order
(lint, type, test, build). Then always run the universal emoji gate last:

```bash
python3 - <<'PYEOF'
import subprocess, re, sys, os
EMOJI = re.compile(r'[\U0001F300-\U0001FAFF\U00002600-\U000027BF]')
changed = subprocess.run(['git','diff','main..HEAD','--name-only'], capture_output=True, text=True).stdout.splitlines()
md_files = [f for f in changed if f.endswith(('.md','.mdx')) and os.path.isfile(f)]
hits = []
for path in md_files:
    for n, line in enumerate(open(path, errors='ignore'), 1):
        if EMOJI.search(line): hits.append(f'{path}:{n}: {line.rstrip()[:100]}')
if hits:
    print('EMOJI CHECK FAIL:'); [print(h) for h in hits[:25]]; sys.exit(1)
print('EMOJI CHECK: OK'); sys.exit(0)
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

Run `git diff main..HEAD --name-only`. Filter to source files only — exclude:
`*.md`, `*.mdx`, `*.json`, `*.toml`, `*.yaml`, `*.yml`, `planning/`, `docs/`, `scaffold/`.

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

### Step 2.5 — Code review (skip if `--no-review` was passed)

Unless `--no-review` was passed, run the `/code-review` command to review code quality for changes:
```
/code-review <level>
```
Substitute `<level>` with the value from `--review-level` (defaulting to `low`).

If the code review surfaces any critical errors or issues, stop and do not proceed to subsequent steps.

### Step 3 — Patch documentation

Invoke the `/update-docs --patch` skill. Wait for it to complete.

### Step 4 — Hand off

**Skip this step if `--gap-check-only` was passed.** Instead, print a one-line summary:
`Gap-check complete. Gating: <PASS|FAIL>. Coverage gaps filled: <N>. Docs patched: <yes|no>.`

Otherwise, invoke the `/handoff` skill.

Pass the handoff note (the $ARGUMENTS remainder after stripping `--skip-coverage`, `--gap-check-only`, `--no-review`, `--review-level`, and `--clean-worktree`). If non-blocking coverage gaps were found in Step 2, prepend a brief line to the note:

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

## Context / Files to Read

- `planning/harness.json` — validation suite (checks + gating flags)
- `planning/status.md` — current focus (to scope coverage check to recent work)
