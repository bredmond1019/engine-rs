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

3. **Derive the spec dir:** `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/`.

4. Read `sdlc/state.json` (if absent, and `sdlc/worklog.md` also has no `## Review` section for
   this scope, **stop immediately:**
   > Cannot fix: no review result found for `<spec> [N]`. Run `/review-task <spec> [N]` first.
   ). Find the most recent `review` verdict for this scope: `state.json`'s top-level `review.verdict`
   field (spec-wide runs) or, for a task-scoped fix, the relevant `tasks["<N>"]` entry plus the
   most recent `## Task <N> — REVIEW ...` section in `sdlc/worklog.md` (worklog sections are the
   ordered history; state.json only holds the latest value per key).

5. Check the verdict.
   - **If PASS:** stop with: "Review verdict is PASS — no fix needed. Run `/document <spec> [N]`."
   - **If FAIL or PARTIAL:** continue.

6. Extract from the most recent `## Task <N> — REVIEW ...` worklog section (and `state.json`'s
   `review.findings`, if populated):
   a. Every acceptance criterion recorded as NOT MET or PARTIAL — criterion text and evidence.
   b. Every issue listed — collect the symbol reference (function, struct, type, or test name; a
      line number only as a secondary hint) and description.
   c. The blocking rationale.

7. Run `/prime` to orient to the codebase.

8. **Determine the fix pass number:** read `sdlc/state.json`'s `tasks["<N>"].attempts` (or, for a
   full run, the highest `attempts` across all task entries touched by this spec). This fix pass
   is `attempts + 1`. If no prior state exists, this is Fix Pass 1 — note its absence.

9. **Determine the baseline file list:** read `sdlc/state.json`'s `tasks["<N>"].files_changed`
   (the implement/prior-fix-pass record). If absent, use the files most likely touched based on
   the spec and the review worklog section's issues.

10. Read the task spec in full: acceptance criteria and Validation Commands section.

11. Read every source file from the baseline file list (step 9). Understand current code state.

12. THINK HARD: for each failing criterion (step 6a) and each issue found (step 6b), identify
    the minimal targeted change needed. Do not re-implement work that already passed.

13. **Make only the targeted fixes.** Address every item from steps 6a and 6b. Do not touch
    code paths that the review confirmed as passing.

14. **Compile the complete file list:** start with the baseline list (step 9). Add any new files
    touched by this fix. Remove nothing — even files untouched by this fix pass must remain
    listed if a prior implementation step created or modified them. Run `git diff --stat` to
    verify completeness. This list is the authoritative input for `/document`.

15. **Run the validation commands** from the spec's Validation Commands section exactly as
    written. If the spec has none, run the project's checks from `planning/harness.json`
    (`validation.checks[]`); if that is absent too, stop and ask the user for the validation
    commands. Capture the exact output.

16. If validation still fails: record the remaining failures clearly in the worklog entry (see
    Record) — do NOT loop or attempt further changes. The subsequent `/test` and `/review-task`
    cycle is the authoritative gate. A second fix pass may be needed.

17. Record this fix pass (see Record).

18. Summarize the fixes to the user in the chat: what was wrong, what was changed, and whether
    validation passed.

19. Output the pipeline next steps:
    ```
    Next: /test <spec> [N]
    then: /review-task <spec> [N]
    ```

## Context / Files to Read

- `sdlc/state.json` and `sdlc/worklog.md`'s most recent `## Task <N> — REVIEW ...` section — read
  first; gate on non-PASS verdict
- `$ARGUMENTS` (the task spec) — for acceptance criteria and validation commands
- `CLAUDE.md` (standing rules)
- All source files from the baseline file list (step 9)

## Record (worklog + state — the fix pass appends, it does not overwrite)

No prose report file, and **no overwrite.** The old prose-report version of this command wrote
"the current state of Phase 2 work" by overwriting the implement report slot on every fix pass —
that behavior does not carry over. In the worklog model every fix pass **appends** its own
section like every other step; git history (each fix pass's own commit) already gives you what
the overwrite was for, so nothing is lost by appending instead.

1. **Read `sdlc/state.json`** (else start from `{}`); preserve fields you don't touch.

2. **Update `sdlc/state.json`**: set `tasks["<N>"].status` to `"fix_passed"` or `"fix_failed"`;
   increment `tasks["<N>"].attempts`; append this pass's fix descriptions to `tasks["<N>"].fixes`;
   set `tasks["<N>"].files_changed` to the complete list from step 14; set `tasks["<N>"].commit` to
   this pass's short commit hash; set `tasks["<N>"].validated` to the step-15 result. Bump
   `updated_at`; preserve `started_at`.

3. **Append to `sdlc/worklog.md`** (create with header `# Worklog — <spec-slug>` + a blank line
   first, if it doesn't exist yet):
   ```markdown
   ## Task <N> — FIX PASS <k> — <PASSED|FAILED>
   Addressed: <criterion/issue summaries, semicolon-joined>
   Files: <comma-joined file list>
   Commit: <short hash>
   Validated: <PASS|FAIL, from step 15>
   ```
   (`<N>` is "All Tasks" when no task number was given; `<k>` is the fix pass number from step 8.)

Then summarize the fixes to the user in the chat.

Then output:
```
Next: /test <spec> [N]
then: /review-task <spec> [N]
```
