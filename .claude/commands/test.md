# Application Validation Test Suite

Execute the project's validation suite, returning results in a standardized JSON format for
automated processing. The suite is **not hardcoded** — it is read from `planning/harness.json`
(the mechanism/policy split), so this command works for any stack.

## Variables

$ARGUMENTS — optional path to the task spec and optional task number. Same format as `/implement`.

Examples:
- (no args) — run full suite; output JSON to chat only; no file written
- `planning/<spec-slug>/tasks.md` — run full suite; write report to `planning/<spec-slug>/sdlc/reports/test.md`
- `planning/<spec-slug>/tasks.md 1` — run full suite; write report to `planning/<spec-slug>/sdlc/reports/task1-test.md`

The task number N does NOT change which checks run — all checks always run regardless. N only
determines the output file name so the snapshot is scoped to the right pipeline stage.

## Purpose

Proactively identify and fix issues before they impact the project or downstream work. By running
this suite you can:
- Detect lint / format / type errors before they reach the build
- Catch broken tests or regressions
- Verify the project builds/compiles cleanly
- Enforce the universal harness rule: no emoji in changed markdown

## Constants

TEST_COMMAND_TIMEOUT: 5 minutes

## Instructions

- **Step 0 — Parse `$ARGUMENTS`:** If provided, split on the last space. Trailing number = task N; remainder = spec path. Derive the report file path from the spec's parent directory:
  - No args: no file will be written.
  - Spec only: `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/reports/test.md`
  - Spec + task N: `planning/<spec-slug>/tasks.md 1` → `planning/<spec-slug>/sdlc/reports/task1-test.md`
- Run `/prime` to orient to the codebase before executing any checks.
- **Step 1 — Load the validation suite:** Read `planning/harness.json`.
  - If present and valid JSON: the checks are `validation.checks[]`, run **in order, top to bottom**.
    Each entry has `name`, `command`, `purpose`, and `gates` (whether its failure blocks the
    review verdict). The check whose `purpose` names it authoritative for the verdict is the one
    that always prevents PASS when it fails.
  - If absent or invalid: fall back to the spec's optional `## Validation Commands` section — run
    each command there, in order. If there is no spec or no such section, run no project checks
    and record a single informational row (`test_name` `"no_validation_suite"`, `passed` true).
  - The engine ships **no stack defaults** — never invent lint/test/build commands; they come only
    from `harness.json` or the spec.
- Run each check with the Bash tool. Capture the result (passed/failed) and any error messages.
- IMPORTANT: Return ONLY the JSON array with check results
  - IMPORTANT: Do not include any additional text, explanations, or markdown formatting
  - We'll immediately run JSON.parse() on the output, so make sure it's valid JSON
- If a check passes, omit the error field
- If a check fails, include the error message in the error field
- Execute all checks even if some fail
- Error Handling:
  - If a command returns a non-zero exit code, mark as failed
  - Capture stderr output for the error field
  - Timeout commands after `TEST_COMMAND_TIMEOUT`
- Execution order is the order in `validation.checks[]` (or the spec) — earlier gates (format/lint/type) before later ones (test/build)
- All commands are run from the repo root unless the command itself changes directory
- Always run `pwd` before each check to confirm you are in the repo root

## Test Execution Sequence

### Project validation checks (from `planning/harness.json`)

For each entry in `validation.checks[]`, run `command` and record:
- `test_name`: the check's `name`
- `execution_command`: the exact `command`
- `test_purpose`: the check's `purpose` (note whether it is gating)
- `passed`: true iff exit code 0
- `error`: stderr/output snippet on failure; omit on pass

(If `harness.json` is absent, the rows come from the spec's `## Validation Commands` instead.)

### Universal harness gate (always runs last, regardless of config)

**Emoji prohibition** — hard FAIL if any markdown file changed by this work introduces an emoji:

```bash
python3 - <<'PYEOF'
import subprocess, re, sys, os
EMOJI = re.compile(r'[\U0001F300-\U0001FAFF\U00002600-\U000027BF]')
changed = subprocess.run(['git','diff','main..HEAD','--name-only'], capture_output=True, text=True).stdout.splitlines()
md_files = [f for f in changed if f.endswith(('.md','.mdx')) and os.path.isfile(f)]
hits = []
for path in md_files:
    for n, line in enumerate(open(path, errors='ignore'), 1):
        if EMOJI.search(line):
            hits.append(f'{path}:{n}: {line.rstrip()[:100]}')
if hits:
    print('EMOJI CHECK FAIL: emoji in modified files (violates the no-emoji harness rule):')
    for h in hits[:25]: print(h)
    sys.exit(1)
print('EMOJI CHECK: OK — no emoji in modified files')
sys.exit(0)
PYEOF
```

Record this as one row: `test_name` `"emoji_check"`, `test_purpose` "Universal harness gate — no emoji in changed markdown".

## Report

- IMPORTANT: Return results exclusively as a JSON array based on the `Output Structure` section below.
- Sort the JSON array with failed checks (passed: false) at the top
- Include all checks in the output, both passed and failed
- The execution_command field should contain the exact command that can be run to reproduce the check
- This allows subsequent agents to quickly identify and resolve errors

### Output Structure

```json
[
  {
    "test_name": "string",
    "passed": boolean,
    "execution_command": "string",
    "test_purpose": "string",
    "error": "optional string"
  }
]
```

### Example Output

```json
[
  {
    "test_name": "clippy",
    "passed": false,
    "execution_command": "cargo clippy -- -D warnings",
    "test_purpose": "Lint gate — denies warnings",
    "error": "error: unused variable `parsed` ... -D unused-variables"
  },
  {
    "test_name": "test",
    "passed": true,
    "execution_command": "cargo test",
    "test_purpose": "Test suite — authoritative for the review verdict; a failure here always prevents PASS."
  }
]
```

## File Output

If `$ARGUMENTS` was provided, after returning the JSON array to chat, write a report file to the
derived path. Create `planning/<name>/sdlc/reports/` if it does not exist.

**Write the report file in this exact format** (let M = total number of checks run, including the
emoji gate):

```markdown
# Test Report — <spec filename> [Task <N> | All Tasks]

**Date:** <YYYY-MM-DD>
**Plan:** <spec file path, or "ad-hoc">
**Scope:** Task <N> | All tasks
**Overall result:** PASS (<n>/<M> passed) | FAIL (<n>/<M> passed)

## Summary

| Test | Result | Error |
|---|---|---|
| <check name> | PASS / FAIL | <error snippet or blank> |
| ... (one row per check, in order) | | |
| emoji_check | PASS / FAIL | |

## Full Results (JSON)

\`\`\`json
<the full JSON array, verbatim>
\`\`\`

## Next Step

`/review-task <spec file path> [N]`
```

After writing the file, output one line to chat:
```
Next: /review-task planning/<spec-slug>/tasks.md [N]
```
