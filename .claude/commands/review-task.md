# Review Task — Verify a completed task against its spec and acceptance criteria.

## Variables

$ARGUMENTS — path to the task spec, with an optional task number suffix (same format as `/implement`).

Examples:
- `planning/phase0-blockC/tasks.md` — review all tasks in the spec
- `planning/phase0-blockC/tasks.md 1` — review only Task 1
- `planning/phase0-blockC/tasks.md 3` — review only Task 3

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user for the task spec path.
2. Parse `$ARGUMENTS`: split on the last space. If the trailing token is a number, treat it as the **task number** to review; the remainder is the task spec path.
3. Run `/prime` to orient to the codebase before reading any files.
4. **Derive file paths for prior step outputs** (reports live in the spec's `reports/` sibling directory):

   Implement report (written by `/implement`):
   - Plan only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/implement.md`
   - Plan + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-implement.md`

   Test report (written by `/test`):
   - Plan only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/test.md`
   - Plan + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-test.md`

5. Read the task spec in full.
6. Read both prior step outputs as historical context:
   a. Read the implement report if it exists. If absent, note it and continue from source alone.
   b. Read the test report if it exists. Note the historical results, but do NOT treat them as
      authoritative — a fresh test run is required in step 8. If absent, note that `/test` was
      not run before this review; the fresh run below covers it.
7. Read every file listed in the implement report's **Files Created or Modified** table (or, if no
   report, read the files most likely touched by the task based on the spec). Verify the actual
   content — do not trust the report alone.
8. **Run a fresh test suite** as the authoritative verification. Do NOT rely on the historical
   test report — run the commands now and capture the results.
   - **Task-scoped:** run the spec's Validation Commands section exactly as written.
   - **Full block:** run the spec's complete Validation Commands block. If none exist, run the
     project's checks from `planning/harness.json` (`validation.checks[]`); if that is absent too,
     stop and ask the user for the validation commands.
   The checks marked `gates: true` in `planning/harness.json` are authoritative for the verdict
   (a typical project gates its test suite and build). Also enforce any project-specific gate
   written in `CLAUDE.md`. A failing gated check always prevents PASS, even if all acceptance
   criteria appear MET from reading the code.
9. **Check every Acceptance Criterion** in the spec against the actual code and fresh test output:
   - For each criterion: state whether it is **MET**, **PARTIAL**, or **NOT MET**, and cite the evidence (file + line, test name, command output).
   - A criterion is MET only when you can point to code or test output that directly satisfies it.
   - Do not mark anything MET based solely on the implementation report — verify in source.
10. Write the review report file and summarize to the user (see Report).

## Context / Files to Read

- `$ARGUMENTS` (the task spec)
- `planning/phase0-blockC/sdlc/reports/implement.md` (or task-scoped variant — the implementation report, if present)
- `planning/phase0-blockC/sdlc/reports/test.md` (or task-scoped variant — the test report, if present)
- `CLAUDE.md` (standing rules — check for violations)
- All files created or modified by the implementation

## Report

**Derive the review report file path** (reports live in the spec's `reports/` sibling directory):
- Plan only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/review.md`
- Plan + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-review.md`

**Write the review report file** in this exact format:

```markdown
# Review Report — <plan filename> [Task <N> | All Tasks]

**Date:** <YYYY-MM-DD>
**Plan:** <plan file path>
**Scope:** Task <N> | All tasks
**Implement report:** found / not found
**Test report:** found / not found
**Overall verdict:** PASS / FAIL / PARTIAL

## Acceptance Criteria

| # | Criterion | Status | Evidence |
|---|---|---|---|
| 1 | <criterion text, truncated to ~80 chars> | MET / PARTIAL / NOT MET | file:line or test name or command output |
| 2 | … | … | … |

## Fresh Test Run

**Commands run:**
\`\`\`
<exact commands executed>
\`\`\`

**Output:**
\`\`\`
<stdout/stderr, truncated to relevant lines>
\`\`\`
Result: PASS / FAIL

## CLAUDE.md Rule Violations

Check against **the project's standing rules** as written in `CLAUDE.md` and `planning/context.md`
(stack, locale-parity, narrative, content-layout — only what is actually written there). Plus the
universal harness rules:
- **No fabricated metrics/quotes** — every number verifiable; model ids / package names verified, not from memory.
- **No emoji** in docs or output.
- **Gated checks pass** — every `planning/harness.json` check with `gates: true` passes before the task is done.

- <any standing-rule violation found, or "None">

## Issues Found

- <concrete problem with file + line reference, or "None">

**Verdict rules:**
- **PASS** — all acceptance criteria MET AND fresh test run passed.
- **PARTIAL** — criteria MET but one or more fresh tests failed; or fresh tests pass but some criteria only partially met.
- **FAIL** — blocking acceptance criteria NOT MET, or fresh test run produced failures that invalidate the implementation.

## Verdict

<One paragraph: overall assessment, what passed, what failed, what must be fixed before this task is considered done. If PASS, state clearly. If FAIL or PARTIAL, list the blocking items.>
```

Then summarize the verdict and any blocking issues to the user in the chat.

Then output the pipeline next step:
```
If verdict is PASS:     Next: /document planning/phase0-blockC/tasks.md [N]
If verdict is not PASS: /fix planning/phase0-blockC/tasks.md [N]
                        then: /test planning/phase0-blockC/tasks.md [N]
                        then: /review-task planning/phase0-blockC/tasks.md [N]
```
