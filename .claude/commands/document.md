# Document — Update docs to reflect a completed, reviewed implementation.

Gates on the review verdict being PASS. Uses the implement report's file list to scope
doc updates surgically, without relying on git diff alone.

## Variables

$ARGUMENTS — path to the task spec with optional task number. Same format as `/implement`.

Examples:
- `planning/<spec-slug>/tasks.md` — document all tasks in the spec
- `planning/<spec-slug>/tasks.md 3` — document task 3 only

## Instructions

1. If `$ARGUMENTS` is not provided, stop:
   > "Usage: /document planning/<spec-slug>/tasks.md [N]"

2. Parse `$ARGUMENTS`: split on the last space. Trailing number = task N; remainder = spec path.

3. Run `/prime` to orient to the codebase before reading any files.

4. **Derive the review report path** (reports live in the spec's `reports/` sibling directory):
   - Plan only: `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/reports/review.md`
   - Plan + task N: `planning/<spec-slug>/tasks.md 3` → `planning/<spec-slug>/sdlc/reports/task3-review.md`

5. Read the review report. Check the **Overall verdict** line.
   **If the verdict is not PASS, STOP immediately:**
   > "Cannot document: review verdict is [FAIL/PARTIAL]. Resolve all blocking issues and
   > re-run `/review-task [args]` until the verdict is PASS."

6. **Derive the implement report path:**
   - Plan only: `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/reports/implement.md`
   - Plan + task N: `planning/<spec-slug>/tasks.md 3` → `planning/<spec-slug>/sdlc/reports/task3-implement.md`

7. Read the implement report. Extract the **Files Created or Modified** table. This is the
   authoritative list of changed source files — do not guess from git or the spec.

8. Read every file in `docs/`. For each doc, check whether it references any of the changed
   source files (look for `**Source:**` annotations, code paths, class/function/component names that
   appear in the changed files). Build a map: `{ doc_path → [changed_source_files_it_covers] }`.
   If no docs reference any changed file, note "No docs affected" and skip to step 10.

9. For each affected doc + changed source pair:
   a. Read the full current doc and the full changed source file.
   b. Identify only the sections that describe the changed code.
   c. Rewrite those sections to match the source exactly. Leave all other sections untouched.
   d. If a change is too large for surgical patching (e.g. an entire module was replaced),
      flag it as `NEEDS_REVIEW` and skip — do not attempt a full rewrite.

10. Apply all surgical updates.

11. Write the document report and summarize to the user (see Report).

## Rules

- **Surgical only.** Never rewrite a doc section not covered by the changed source files.
- **Source is authoritative.** If the doc and source disagree, the source wins.
- **No invention in pipeline mode.** Do not add new sections or cover APIs not already in the doc. If `docs/` is empty or missing coverage, use `/update-docs --patch` (ad-hoc sweep) or `/update-docs --bootstrap` (full creation from scratch) to create docs first — then re-run `/document` to keep them current.
- **Never touch** `planning/`, `log.md`, `status.md`, or `CLAUDE.md`.
- **Flag** architecture-level docs as `NEEDS_REVIEW` if architecture-level source files changed (core libraries, routing/config, or other foundational modules the project treats as architecture). Never edit them automatically.
- **Gate strictly on PASS.** Never run doc updates if the review verdict is not PASS.

## Context / Files to Read

- Review report (derived from $ARGUMENTS) — read first; gate on PASS
- Implement report (derived from $ARGUMENTS) — for the changed file list
- `docs/` — all existing reference docs
- Changed source files identified from the implement report

## Report

**Derive the document report file path** (reports live in the spec's `reports/` sibling directory):
- Plan only: `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/reports/document.md`
- Plan + task N: `planning/<spec-slug>/tasks.md 3` → `planning/<spec-slug>/sdlc/reports/task3-document.md`

Create `planning/<spec-slug>/sdlc/reports/` if it does not exist.

**Write the report file in this exact format:**

```markdown
# Document Report — <spec filename> [Task <N> | All Tasks]

**Date:** <YYYY-MM-DD>
**Plan:** <spec file path>
**Scope:** Task <N> | All tasks
**Review verdict confirmed:** PASS
**Implement report read:** <path>
**Source files scanned:** <N>
**Docs updated:** <N>

## Docs Updated

| Doc | Sections Updated |
|---|---|
| docs/OPERATIONS.md | "Content loader" subsection |

## Docs Checked — No Changes Needed

- docs/DEPLOYMENT.md — no references to changed source files

## NEEDS_REVIEW

- docs/architecture.md — an architecture-level source file was modified; manual review required

## Source Files Covered

| Source File | Action | Docs Updated |
|---|---|---|
| src/services/data-loader | modified | docs/OPERATIONS.md |
| src/routes/index | modified | (no docs reference this file) |

## Next Step

`/log-work [notes about what was completed]`
```

Then summarize the updates to the user in the chat.
