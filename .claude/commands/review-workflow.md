# Review Workflow — Verify that a completed sdlc-run pipeline executed correctly.

## Variables

$ARGUMENTS — block ID with an optional task number suffix.

Examples:
- `phase0-blockC` — review the full-block workflow run
- `phase0-blockC 2` — review the task-2-scoped workflow run

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask for the block ID.

2. Parse `$ARGUMENTS`: split on the last space. If the trailing token is a number, treat it as the
   **task number**; the remainder is the block ID.

3. Run `/prime` to orient to the codebase before reading any files.

4. **Derive file paths** from the block ID:

   | Item | Path |
   |---|---|
   | Spec | `planning/<blockId>/tasks.md` |
   | Reports dir | `planning/<blockId>/sdlc/reports/` |
   | Task prefix | `task<N>-` if task number given, else empty |

   Report files (prefix applied to all):
   - Implement report: `planning/<blockId>/sdlc/reports/[taskN-]implement.md`
   - Test report:      `planning/<blockId>/sdlc/reports/[taskN-]test.md`
   - Review report:    `planning/<blockId>/sdlc/reports/[taskN-]review.md`
   - Document report:  `planning/<blockId>/sdlc/reports/[taskN-]document.md`
   - Workflow report:  `planning/<blockId>/sdlc/reports/[taskN-]workflow.md`
   - **Output (this command):** `planning/<blockId>/sdlc/reports/[taskN-]workflow-review.md`

5. **Check which report files exist** — run `ls` for each expected path. Record present/missing.

6. **Read all existing reports** in this order:
   a. Workflow report — primary source: pipeline metadata, stage results table, commit list
   b. Implement report — what was built; did validation pass?
   c. Test report — validation check results (one row per check + emoji gate); pass/fail counts
   d. Review report — verdict, acceptance criteria check, fresh test outcome
   e. Document report — which docs were patched (relevant only if verdict was PASS)

7. **Check git log** for commits made during this pipeline run:
   ```
   git log --oneline -20
   ```
   Look for commits matching the block/task stem. Expected commit pattern:
   - `feat: implement <stem>` (or `fix:` if validation failed)
   - `fix: fix pass N for <stem>` — one per fix pass (if fix cycles ran)
   - `docs: update docs for <stem>` — document agent commit (if verdict was PASS)
   - `chore: wrap up <stem>` — finalize agent commit

8. **Check log.md** — does an entry exist for this run? Does it accurately describe the outcome?
   ```
   head -60 log.md
   ```

9. **Check status.md** — is the block/task marked correctly?
   ```
   grep -A3 "<blockId>" planning/status.md
   ```
   Expected:
   - Full block run, verdict PASS → block status "Done"
   - Task-scoped run or verdict FAIL → block status "In progress"

10. **Evaluate each stage** using the checks in the Checks section below.

11. Write the workflow review report (see Report section) and summarize findings to the user.

## Checks

### Stage Completeness
For each expected report, check: is the file present? Does it contain the required sections?

| Stage | Required Sections |
|---|---|
| Implement | `## What Was Built`, `## Files Created or Modified`, `## Validation Output` |
| Test | `## Summary` table (one row per validation check + the emoji gate), pass/fail counts, `## Full Results (JSON)` |
| Review | `## Acceptance Criteria Check` table, `**Verdict:**` line |
| Document | `## Docs Patched` table, `## Docs Flagged NEEDS_REVIEW` (only if review PASS) |
| Workflow | `## Stage Results` table, `## Final Verdict`, `## Commits (this pipeline run)` |

The checks a passing Test stage records come from the project's `planning/harness.json`
(`validation.checks[]`, in order) plus the universal emoji gate — so the row count and commands
vary by project. There are no hardcoded stack commands.

### Verdict Chain
- What verdict did the review report? (PASS / FAIL / PARTIAL)
- How many review attempts occurred?
- If FAIL or PARTIAL: did the fix→test→review retry cycle run? Are fix pass commits present?
- If PASS: did the document stage run and produce a report?

### Commit Audit
Map expected commits to `git log` output. For each expected commit:
- Is it present? (search by prefix + stem keyword)
- Does the commit message follow the conventional commit format?


### Log Quality
- Is there an entry dated to this run?
- Does it cover: what was built, review outcome, any notable decisions, "Next:" pointer?
- Does the git log block in the entry show the pipeline commits?

### STATUS Accuracy
- Is the block's Status column in the Progress Table correct for the outcome?
- Is the "Current focus" line updated to the next task or block?
- Is "Last updated" current?

## Context / Files to Read

- All five pipeline report files (implement, test, review, document, workflow)
- `planning/status.md`
- `log.md`
- `git log --oneline -20`

Do NOT re-run the test suite — this command reviews the pipeline's execution record, not the implementation itself. Use `/review-task` if you need to re-verify the implementation.

## Report

Write the workflow review report to:
`planning/<blockId>/sdlc/reports/[taskN-]workflow-review.md`

Use EXACTLY this format:

```markdown
# Workflow Review — <blockId> [Task <N> | All Tasks]

**Date:** <YYYY-MM-DD>
**Block:** <blockId>
**Scope:** Task <N> | All tasks
**Pipeline verdict:** PASS / FAIL / PARTIAL (from review report)
**Review attempts:** N of 3 max
**Overall verdict:** PASS / PARTIAL / FAIL

## Report Completeness

| Stage | Report File | Present | Well-formed |
|---|---|---|---|
| Implement | [taskN-]implement.md | Y / N | Y / N / N/A |
| Test | [taskN-]test.md | Y / N | Y / N / N/A |
| Review | [taskN-]review.md | Y / N | Y / N / N/A |
| Document | [taskN-]document.md | Y / N | Y / N / N/A |
| Workflow | [taskN-]workflow.md | Y / N | Y / N / N/A |

## Pipeline Outcome

**Implementation verdict:** PASSED / FAILED (from Validation Output in implement report)
**Review verdict:** PASS / FAIL / PARTIAL
**Review attempts:** N
**Fix passes:** N
**Document stage:** ran / skipped — <reason if skipped>

## Commit Audit

| Expected Commit | Present | Hash | Notes |
|---|---|---|---|
| feat/fix: implement <stem> | Y / N | abc1234 | — |
| fix: fix pass 1 for <stem> | Y / N | — | only if fix cycle ran |
| docs: update docs for <stem> | Y / N | abc1234 | only if verdict PASS |
| chore: wrap up <stem> | Y / N | abc1234 | — |

## Log Check

**Entry present:** yes / no
**Describes outcome accurately:** yes / partially / no
**Git log block included:** yes / no
**Notes:** <any gaps or inaccuracies>

## status.md Check

**Block status:** Done / In progress / Not started
**Correct for this outcome:** yes / no
**Current focus updated:** yes / no
**Notes:** <any discrepancy>

## Issues Found

- <concrete problem with report/commit/Log reference, or "None">

## Verdict

<One paragraph: did the pipeline execute correctly end-to-end? Were all stages completed
and committed? Are records (Log, STATUS) accurate? If PASS, state clearly.
If PARTIAL or FAIL, list the specific blocking items that need manual follow-up.>
```

**Verdict rules:**
- **PASS** — all expected reports present and well-formed, commits match the expected conventional
  commit pattern, Log entry exists and describes the outcome, status.md is accurate.
- **PARTIAL** — minor gaps: a report section is thin, Log entry is sparse, a commit message
  is slightly off-format, or STATUS is correct but "Current focus" wasn't updated.
- **FAIL** — critical failures: required reports missing, expected commits absent, Log not
  updated, status.md still shows "Not started" or wrong state, or the pipeline review verdict
  was FAIL and no fix cycle was attempted.

Then summarize the verdict and any issues requiring manual follow-up to the user in the chat.
