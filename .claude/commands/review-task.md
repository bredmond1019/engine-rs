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
4. **Derive the spec dir:** `planning/phase0-blockC/tasks.md` → `planning/phase0-blockC/sdlc/`.

5. Read the task spec in full.
6. Read prior step history as context, from `sdlc/state.json` and `sdlc/worklog.md`:
   a. Read `sdlc/state.json`'s `tasks["<N>"]` entry, if present — `summary`, `files_changed`,
      `validated`. If absent, note it and continue from source alone.
   b. Read `sdlc/worklog.md`'s `## Task <N> — TEST ...` section(s), if present, for the historical
      test result. Do NOT treat it as authoritative — a fresh test run is required in step 8. If
      absent, note that `/test` was not run before this review; the fresh run below covers it.
7. Read every file listed in `tasks["<N>"].files_changed` (or, if absent, read the files most
   likely touched by the task based on the spec). Verify the actual content — do not trust the
   recorded summary alone.
8. **Run a fresh test suite** as the authoritative verification. Do NOT rely on the historical
   test worklog entry — run the commands now and capture the results.
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
   - Do not mark anything MET based solely on the recorded `summary` — verify in source.
10. Record the verdict (see Record) and summarize to the user.

## Context / Files to Read

- `$ARGUMENTS` (the task spec)
- `sdlc/state.json`'s `tasks["<N>"]` entry, and `sdlc/worklog.md`'s prior `## Task <N> — ...` sections, if present
- `CLAUDE.md` (standing rules — check for violations)
- All files created or modified by the implementation

## Record (worklog + state, not a prose report)

No prose report file. Record this review the way `/sdlc-flow` and `/sdlc-task` do (D31) — a
worklog section plus a state update. Both `sdlc/worklog.md` and `sdlc/state.json` are
**committed**, exactly as the engines commit theirs — in a vaulted repo through the REAL vault path
(`git -C <vault>/planning ...`), never through the `planning/` symlink face, which aborts the whole
`git add` with "beyond a symbolic link" (D46).

**Evidence format:** name the symbol first — function, struct, type, or test name — not a bare
line number. A line number moves the moment the file is next edited; a symbol can still be
grepped weeks later when this worklog entry is read. A line number may follow as a secondary hint.

**Verdict rules** (unchanged):
- **PASS** — all acceptance criteria MET AND fresh test run passed.
- **PARTIAL** — criteria MET but one or more fresh tests failed; or fresh tests pass but some criteria only partially met.
- **FAIL** — blocking acceptance criteria NOT MET, or fresh test run produced failures that invalidate the implementation.

1. **Read `sdlc/state.json`** (else start from `{}`); preserve fields you don't touch.

2. **Update `sdlc/state.json`**: set top-level `review.verdict` (spec-wide) or, for a task-scoped
   review, `tasks["<N>"].status` to `"reviewed_<verdict lowercased>"`; set/append
   `review.findings` with any NOT MET/PARTIAL criteria and issues found (short strings); increment
   `review.attempts`. Bump `updated_at`; preserve `started_at`.

3. **Append to `sdlc/worklog.md`** (create with header `# Worklog — <spec-slug>` + a blank line
   first, if it doesn't exist yet):
   ```markdown
   ## Task <N> — REVIEW <PASS|FAIL|PARTIAL>
   Criteria: <n> MET, <n> PARTIAL, <n> NOT MET
   Issues: <symbol/test-name references, semicolon-joined; omit line if none>
   CLAUDE.md violations: <summary, or omit line if none>
   Verdict: <one-sentence rationale>
   ```
   (`<N>` is "All Tasks" when no task number was given.)

Then summarize the verdict and any blocking issues to the user in the chat.

Then output the pipeline next step:
```
If verdict is PASS:     Next: /document planning/phase0-blockC/tasks.md [N]
If verdict is not PASS: /fix planning/phase0-blockC/tasks.md [N]
                        then: /test planning/phase0-blockC/tasks.md [N]
                        then: /review-task planning/phase0-blockC/tasks.md [N]
```
