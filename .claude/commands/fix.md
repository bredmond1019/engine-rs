# Fix — Make targeted fixes for a FAIL or PARTIAL review verdict.

## Variables

$ARGUMENTS — path to the task spec, with an optional task number suffix. Same format as `/implement`.

Examples:
- `planning/phase0-blockC/tasks.md` — fix all tasks in the spec
- `planning/phase0-blockC/tasks.md 3` — fix task 3 only

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user for the task spec path.

2. Parse `$ARGUMENTS`: split on the last space. If the trailing token is a number, treat it as
   the **task number** to fix; the remainder is the spec path. If no number is present, operate
   on all tasks.

3. **Derive the review report path:**
   - Spec only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/review.md`
   - Spec + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-review.md`

4. Read the review report. **If it does not exist, stop immediately:**
   > Cannot fix: no review report found at `<path>`. Run `/review-task <spec> [N]` first.

5. Check the **Overall verdict** line in the review report.
   - **If PASS:** stop with: "Review verdict is PASS — no fix needed. Run `/document <spec> [N]`."
   - **If FAIL or PARTIAL:** continue.

6. Extract from the review report:
   a. Every row in the **Acceptance Criteria** table where Status is NOT MET or PARTIAL —
      collect criterion text and evidence.
   b. Every item listed under **Issues Found** — collect file:line references and descriptions.
   c. The full **Verdict** paragraph — understand the blocking rationale.

7. Run `/prime` to orient to the codebase.

8. **Derive the implement report path:**
   - Spec only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/implement.md`
   - Spec + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-implement.md`

9. Read the implement report if it exists:
   a. Extract the **Files Created or Modified** table — this is the baseline file list.
   b. Check the report title: if it begins with "Fix Pass" read the `**Fix pass:**` field and set
      the new pass number to that value + 1. If the title begins with "Implementation Report"
      (not a fix pass), this is Fix Pass 1. If no implement report exists at all, this is Fix
      Pass 1 — note its absence in the report.

10. Read the task spec in full: acceptance criteria and Validation Commands section.

11. Read every source file from the **Files Created or Modified** table (or, if that table was
    absent, the files most likely touched based on the spec and review report's Issues Found).
    Understand current code state.

12. THINK HARD: for each failing criterion (step 6a) and each issue found (step 6b), identify
    the minimal targeted change needed. Do not re-implement work that already passed.

13. **Make only the targeted fixes.** Address every item from steps 6a and 6b. Do not touch
    code paths that the review confirmed as passing.

14. **Compile the complete file list for the report:** start with every file from the prior
    implement report's table. Add any new files touched by this fix. Remove nothing — even
    files untouched by the fix pass must remain listed if they were created or modified by a
    prior implementation step. Run `git diff --stat` to verify completeness. This list is the
    authoritative input for `/document`.

15. **Run the validation commands** from the spec's Validation Commands section exactly as
    written. If the spec has none, run the project's checks from `planning/harness.json`
    (`validation.checks[]`); if that is absent too, stop and ask the user for the validation
    commands. Capture the exact output.

16. If validation still fails: record the remaining failures clearly in the report —
    do NOT loop or attempt further changes. The subsequent `/test` and `/review-task` cycle is
    the authoritative gate. A second fix pass may be needed.

17. Write the Fix Pass report to the implement report path, overwriting any prior content
    (see Report).

18. Summarize the fixes to the user in the chat: what was wrong, what was changed, and whether
    validation passed.

19. Output the pipeline next steps:
    ```
    Next: /test <spec> [N]
    then: /review-task <spec> [N]
    ```

## Context / Files to Read

- Review report (derived from $ARGUMENTS) — read first; gate on non-PASS verdict
- Implement report (derived from $ARGUMENTS) — for prior file list and fix pass count
- `$ARGUMENTS` (the task spec) — for acceptance criteria and validation commands
- `CLAUDE.md` (standing rules)
- All source files from the implement report's Files Created or Modified table

## Report

**Write to the implement report path** (overwrites previous implement or fix report):
- Spec only: `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/reports/implement.md`
- Spec + task N: `planning/phase0-blockC/tasks.md 3` → `planning/phase0-blockC/sdlc/reports/task3-implement.md`

**Write the report file in this exact format:**

```markdown
# Fix Pass <N> — <plan filename> [Task <N> | All Tasks]

**Date:** <YYYY-MM-DD>
**Plan:** <plan file path>
**Scope:** Task <N> | All tasks
**Fix pass:** <N>
**Review report consumed:** <path to review report>
**Prior verdict:** FAIL | PARTIAL

## Failures Addressed

| # | Failing Criterion / Issue | Fix Applied |
|---|---|---|
| 1 | <criterion or issue text, ~80 chars> | <what was done to fix it> |
| 2 | … | … |

## Files Created or Modified

| File | Action |
|---|---|
| path/to/file.ts | created / modified |

## Validation Output

**Commands run:**
\`\`\`
<exact commands executed>
\`\`\`

**Results:**
\`\`\`
<stdout/stderr output, truncated to relevant lines>
\`\`\`
Status: PASSED / FAILED

## Changes Made

- <bullet per logical change with file:line reference>

## Decisions and Trade-offs

- <any non-obvious choice made during the fix, or "None">

## git diff --stat

\`\`\`
<output>
\`\`\`
```

Then summarize the fixes to the user in the chat.

Then output:
```
Next: /test <spec> [N]
then: /review-task <spec> [N]
```
