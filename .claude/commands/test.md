# Application Validation Test Suite

Execute the project's validation suite, returning results in a standardized JSON format for
automated processing. The suite is **not hardcoded** — it is read from `planning/harness.json`
(the mechanism/policy split), so this command works for any stack.

## Variables

$ARGUMENTS — optional path to the task spec and optional task number. Same format as `/implement`.

Examples:
- (no args) — run full suite; output JSON to chat only; no worklog/state written
- `planning/<spec-slug>/tasks.md` — run full suite; append a worklog section + update state for the whole spec
- `planning/<spec-slug>/tasks.md 1` — run full suite; append a worklog section + update state scoped to task 1

The task number N does NOT change which checks run — all checks always run regardless. N only
scopes which `sdlc/state.json` task entry and worklog section this run's results are recorded
against.

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

- **Step 0 — Parse `$ARGUMENTS`:** If provided, split on the last space. Trailing number = task N; remainder = spec path. Derive the spec dir from the spec's parent directory:
  - No args: no worklog/state will be written.
  - Spec only: `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/` (record scoped to "all tasks")
  - Spec + task N: `planning/<spec-slug>/tasks.md 1` → `planning/<spec-slug>/sdlc/` (record scoped to task 1)
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

**Emoji prohibition** — hard FAIL if this work ADDS a line containing an emoji to any markdown
file. DIFF-SCOPED: only lines added by this work are judged, never a whole changed file — a legacy
file's pre-existing emoji does not fail a diff that never touched it:

```bash
python3 - <<'PYEOF'
import subprocess, re, sys
EMOJI = re.compile(r'[\U0001F300-\U0001FAFF\U00002600-\U000027BF]')
FOOTER = 'Generated with Claude Code'
diff = subprocess.run(['git','diff','-M','-U0','main..HEAD','--','*.md','*.mdx'], capture_output=True, text=True).stdout.splitlines()
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
    print('EMOJI CHECK FAIL: emoji added in changed markdown (violates the no-emoji harness rule):')
    for h in hits[:25]: print(h)
    sys.exit(1)
print('EMOJI CHECK: OK — no emoji added in changed markdown')
sys.exit(0)
PYEOF
```

Record this as one row: `test_name` `"emoji_check"`, `test_purpose` "Universal harness gate — no emoji added in changed markdown".

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
    "execution_command": "cargo clippy --all-targets -- -D warnings",
    "test_purpose": "Lint gate — end-of-flow review sees test/bench targets too",
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

## Record (worklog + state, not a prose report)

If `$ARGUMENTS` was provided, after returning the JSON array to chat, record the result the way
`/sdlc-flow` and `/sdlc-task` do (D31) — no prose report file. Both `sdlc/worklog.md` and
`sdlc/state.json` are **committed**, exactly as the engines commit theirs — in a vaulted repo
through the REAL vault path (`git -C <vault>/planning ...`), never through the `planning/` symlink
face, which aborts the whole `git add` with "beyond a symbolic link" (D46). Let M = total number of checks run, including the emoji gate,
and n = number passed.

1. **Derive the spec dir:** `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/`. Create it if it does not exist.

2. **Read `sdlc/state.json`** if it exists (else start from `{}`); preserve fields you don't touch.

3. **Update `sdlc/state.json`**: set `status` to `"test_pass"` (n == M) or `"test_fail"`
   (n < M); set/merge the task entry (or entries, for a full run) at `tasks["<N>"].validated` to
   `"PASS (<n>/<M>)"` or `"FAIL (<n>/<M>)"`, and append any failing check names to that entry's
   `issues` array (don't duplicate ones already present). Bump `updated_at` (NOW, UTC ISO8601);
   preserve `started_at`.

4. **Append to `sdlc/worklog.md`** (create with header `# Worklog — <spec-slug>` + a blank line
   first, if it doesn't exist yet):
   ```markdown
   ## Task <N> — TEST <PASS|FAIL> (<n>/<M> passed)
   Failing: <comma-joined failing check names, or omit line if none>
   ```
   (`<N>` is "All Tasks" when no task number was given.)

After recording, output one line to chat:
```
Next: /review-task planning/<spec-slug>/tasks.md [N]
```
