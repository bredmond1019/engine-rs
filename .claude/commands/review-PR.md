# Review PR — Spec-aware review for a branch-train PR.

Checks out the PR branch, runs the project's gating suite + emoji gate, reviews the
diff against the block's Acceptance Criteria, and posts a verdict via `gh pr review`.

Designed for PRs produced by `/sdlc-block` in default (PR) mode. Can also review any
PR for a spec-based block by pointing at the spec manually.

## Variables

$ARGUMENTS — `<PR#> [plan-slug]`
- `<PR#>` — required. The GitHub PR number (integer).
- `[plan-slug]` — optional. The plan slug (e.g. `sdlc-block-and-task-updates`) to scope
  state lookup when multiple `block-orchestration-state.json` files exist.

Examples:
```
/review-PR 42
/review-PR 42 sdlc-block-and-task-updates
```

## Instructions

### Step 0 — Parse arguments

Extract `PR#` (required integer) and optional `plan-slug`. If `PR#` is absent or not
an integer, stop and print:
```
Usage: /review-PR <PR#> [plan-slug]
  PR#        GitHub PR number (required)
  plan-slug  plan slug to scope state lookup (optional; omit when there is only one plan)
Examples:
  /review-PR 42
  /review-PR 42 my-plan
```

### Step 1 — Fetch PR metadata

```bash
gh pr view <PR#> --json number,title,headRefName,baseRefName,state,url,reviews
```

Capture:
- `headRefName` — the PR's source branch (the block branch).
- `baseRefName` — the branch the PR targets.
- `state` — if `MERGED` or `CLOSED`, stop and report: "PR #<N> is already <state>."
- `reviews` — existing reviews (context only; do not let prior reviews override this run's findings).

### Step 2 — Locate the orchestration state

Search for the state file:
```bash
find planning -name "block-orchestration-state.json" 2>/dev/null
```

If `plan-slug` was given, filter to `planning/<plan-slug>/sdlc/block-orchestration-state.json`.

If multiple files remain after filtering, pick the one whose `blocks` map contains an
entry with `branch == headRefName`. If still ambiguous, list the candidates and ask the
user to re-run with a `plan-slug`.

Read the selected state file (parse as JSON). Find the block entry where
`blocks[slug].branch == headRefName`. Extract `blockSlug` (the map key). Derive
`specFile = planning/<blockSlug>/tasks.md`.

If no state file is found or no block matches the branch: set `blockSlug = null`,
`specFile = null`. The AC review step will note the gap.

### Step 3 — Check out the PR branch

```bash
gh pr checkout <PR#>
```

Confirm the working branch:
```bash
git branch --show-current
```

If checkout fails, stop and surface the error. Note the original branch so you can
restore it in Step 8.

### Step 4 — Run the gating suite

Read `planning/harness.json`. Run every check listed in `validation.checks[]` in order.

**If `harness.json` is absent or has no `validation.checks[]`** (the config-absent convention every
engine follows), fall back to the spec's `## Validation Commands` block: read
`planning/<blockSlug>/tasks.md` (the `specFile` from Step 2) and run each command listed there in
order, treating each as a gating check. If neither a harness config nor a spec Validation Commands
block can be found, do **not** silently APPROVE — run no gating checks, record "gating suite: not
found", and downgrade the verdict to COMMENT (never APPROVE) so a human runs the checks.

Then always run the emoji gate last, diffing from the merge-base so only the PR's own
changes are scanned:

```bash
python3 - <<'PYEOF'
import subprocess, re, sys, os
EMOJI = re.compile(r'[\U0001F300-\U0001FAFF\U00002600-\U000027BF]')
base = subprocess.run(
    ['git', 'merge-base', 'HEAD', 'origin/main'],
    capture_output=True, text=True).stdout.strip()
if not base:
    base = subprocess.run(
        ['git', 'merge-base', 'HEAD', 'main'],
        capture_output=True, text=True).stdout.strip()
changed = subprocess.run(
    ['git', 'diff', base, 'HEAD', '--name-only'],
    capture_output=True, text=True).stdout.splitlines()
md_files = [f for f in changed if f.endswith(('.md', '.mdx')) and os.path.isfile(f)]
hits = []
for path in md_files:
    for n, line in enumerate(open(path, errors='ignore'), 1):
        if EMOJI.search(line):
            hits.append(f'{path}:{n}: {line.rstrip()[:100]}')
if hits:
    print('EMOJI CHECK FAIL:')
    [print(h) for h in hits[:25]]
    sys.exit(1)
print('EMOJI CHECK: OK')
sys.exit(0)
PYEOF
```

Record each check as PASS or FAIL with any relevant output lines. A check with
`gates: true` that fails is a **blocking finding** — it must appear in the verdict and
forces REQUEST_CHANGES regardless of AC status.

### Step 5 — Review the diff against the spec

**Get the diff:**
```bash
git diff <baseRefName>...HEAD --stat
git diff <baseRefName>...HEAD
```

**If `specFile` was located (Step 2):** read `planning/<blockSlug>/tasks.md` in full.
Note every Acceptance Criterion under `## Acceptance Criteria` (or equivalent heading).

For each criterion:
- State whether it is **MET**, **PARTIAL**, or **NOT MET** based on the diff.
- Cite evidence: file + function/line reference, or test name.
- A criterion is MET only when the diff contains code or a test that directly satisfies it.
  Do not mark MET based on plausibility alone.

**If no spec was located:** note "Block spec not found — AC review skipped. Base verdict
on gating results only." and continue.

### Step 6 — Determine verdict

- **APPROVE** — all gating checks pass, emoji gate passes, all located AC are MET.
- **REQUEST_CHANGES** — any gating check with `gates: true` fails, emoji gate fails,
  or any AC is NOT MET or PARTIAL.
- **COMMENT** — use only when informational (no spec found + all gating checks pass, **or** no gating
  suite could be located at all per Step 4); note what could not be verified so the human reviewer can
  fill the gap. Never APPROVE when the gating suite could not be located.

### Step 7 — Compose and post the review

Build the review body:

```
## PR Review — Block <blockSlug | PR title>

**Gating suite:** PASS | FAIL (<comma-separated list of failing check names>)
**Emoji gate:** PASS | FAIL
**Spec AC:** ALL MET | <N> PARTIAL | <N> NOT MET | not reviewed (spec not found)

### Acceptance Criteria

| # | Criterion | Status | Evidence |
|---|---|---|---|
| 1 | <criterion text, ~80 chars> | MET / PARTIAL / NOT MET | file:line or test name |

### Gating Results

| Check | Result | Notes |
|---|---|---|
| <check name> | PASS / FAIL | <first failing line, truncated> |
| emoji gate   | PASS / FAIL | <files with emoji, or "OK"> |

### Verdict

<One paragraph: what passed, what failed, what must be fixed before APPROVE.>
```

Post the review:
```bash
gh pr review <PR#> --approve --body "<body>"           # APPROVE
gh pr review <PR#> --request-changes --body "<body>"   # REQUEST_CHANGES
gh pr review <PR#> --comment --body "<body>"           # COMMENT
```

### Step 8 — Restore working branch

```bash
git checkout <base_branch or the branch recorded in Step 3>
```

Report to the user:
- Verdict posted (`APPROVE` / `REQUEST_CHANGES` / `COMMENT`).
- Gating summary (pass/fail counts).
- If REQUEST_CHANGES: list the specific blocking items that must be fixed.
- Next step:
  - If APPROVE: "`/merge-train [plan-slug]` once all PRs in the train are approved."
  - If REQUEST_CHANGES: "Fix the blocking items on branch `<headRefName>`, push, and
    re-run `/review-PR <PR#>` to re-review."

## Notes

- **Checkout side-effect.** This command checks out the PR branch in the current session.
  Run from the main repo root, not from inside a worktree. Step 8 restores the original
  branch automatically.
- **Emoji gate scope.** The gate diffs from the merge-base of HEAD and main/origin/main
  so it covers only the PR's own changes, not the full branch history.
- **Gating vs. non-gating checks.** Only checks with `gates: true` in `harness.json`
  are blocking. Non-gating failures are surfaced as informational findings in the review body.
- **Fat PRs.** In default `/sdlc-block` mode, Phase-N PRs include ancestor block work
  (the train branch is the common base). The AC review is still scoped to the target
  block's spec because `baseRefName` is the train branch from which this block forked.
- **`/merge-train`** reads the orchestration state and merges all approved PRs bottom-up
  in dependency order. Run it after every PR in the train is approved.
