# Document — Update docs to reflect a completed, reviewed implementation.

Gates on the review verdict being PASS. Scopes doc updates surgically from the real diff, scoped
to this task's branch/commit, cross-checked against the recorded file list.

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

4. **Derive the spec dir:** `planning/<spec-slug>/tasks.md` → `planning/<spec-slug>/sdlc/`.

5. Read `sdlc/state.json`'s top-level `review.verdict` (spec-wide) or `tasks["<N>"].status` (task-scoped).
   **If the verdict is not PASS, STOP immediately:**
   > "Cannot document: review verdict is [FAIL/PARTIAL]. Resolve all blocking issues and
   > re-run `/review-task [args]` until the verdict is PASS."
   If no state file / no review recorded, STOP with the same message, naming the missing state.

6. **Derive the changed-file list from the real diff, scoped to this task's branch/commit —
   not the whole working tree, which may hold unrelated concurrent-session noise in a shared
   index.** Run `git diff --name-only <base>...HEAD` (or, for an uncommitted worktree,
   `git diff --name-only HEAD`) against the branch or commit this implement/review pass produced,
   not a bare `git status` over the shared index. This diff is the authoritative list of changed
   source files.

   Then read `sdlc/state.json`'s `tasks["<N>"].files_changed` as a cross-check. If the two
   disagree — a file the state entry claims but the diff doesn't show, or vice versa — treat the
   mismatch itself as a finding: note it in the worklog entry's Docs Checked line rather than
   silently trusting either side, and prefer the diff's list for scoping the doc-update work in
   step 8.

7. Read every file in `docs/`. For each doc, check whether it references any of the changed
   source files (look for `**Source:**` annotations, code paths, class/function/component names that
   appear in the changed files). Build a map: `{ doc_path → [changed_source_files_it_covers] }`.
   If no docs reference any changed file, note "No docs affected" and skip to step 9.

8. For each affected doc + changed source pair:
   a. Read the full current doc and the full changed source file.
   b. Identify only the sections that describe the changed code.
   c. Rewrite those sections to match the source exactly. Leave all other sections untouched.
   d. If a change is too large for surgical patching (e.g. an entire module was replaced),
      flag it as `NEEDS_REVIEW` and skip — do not attempt a full rewrite.

9. Apply all surgical updates.

10. Record the result (see Record) and summarize to the user.

## Rules

- **Surgical only.** Never rewrite a doc section not covered by the changed source files.
- **Source is authoritative.** If the doc and source disagree, the source wins.
- **No invention in pipeline mode.** Do not add new sections or cover APIs not already in the doc. If `docs/` is empty or missing coverage, use `/update-docs --patch` (ad-hoc sweep) or `/update-docs --bootstrap` (full creation from scratch) to create docs first — then re-run `/document` to keep them current.
- **Never touch** `planning/`, `log.md`, `status.md`, or `CLAUDE.md`.
- **Flag** architecture-level docs as `NEEDS_REVIEW` if architecture-level source files changed (core libraries, routing/config, or other foundational modules the project treats as architecture). Never edit them automatically.
- **Gate strictly on PASS.** Never run doc updates if the review verdict is not PASS.

## Context / Files to Read

- `sdlc/state.json` — read first; gate on `review.verdict` (or task-scoped status) being PASS
- `git diff --name-only` output scoped to this task's branch/commit — the authoritative changed-file list
- `sdlc/state.json`'s `tasks["<N>"].files_changed` — cross-check against the diff
- `docs/` — all existing reference docs
- Changed source files identified from the diff

## Record (worklog + state, not a prose report)

No prose report file. Both `sdlc/worklog.md` and `sdlc/state.json` are **committed**,
exactly as the engines commit theirs. In a vaulted repo commit them through the REAL vault path
(`git -C <vault>/planning ...`), never through the `planning/` symlink face, which aborts the whole
`git add` with "beyond a symbolic link" (D46). Doc file
edits themselves (under `docs/`) are ordinary source changes and still get committed normally per
the spec's own instructions.

1. **Read `sdlc/state.json`** (else start from `{}`); preserve fields you don't touch.

2. **Update `sdlc/state.json`**: set `docs.changed` to the list of doc paths surgically updated
   this run (append, don't duplicate); set `docs.created` similarly if `/update-docs` created new
   files as a prerequisite this run (usually empty for `/document` itself). Bump `updated_at`;
   preserve `started_at`.

3. **Append to `sdlc/worklog.md`** (create with header `# Worklog — <spec-slug>` + a blank line
   first, if it doesn't exist yet):
   ```markdown
   ## Docs
   Patched: <comma-joined docs.changed, or "none">
   Checked, no changes needed: <comma-joined doc paths, or omit line if none>
   NEEDS_REVIEW: <comma-joined doc paths, or omit line if none>
   Mismatch: <diff vs. recorded files_changed note, or omit line if none>
   ```

Then summarize the updates to the user in the chat.

Then output the pipeline next step:
```
Next: /log-work [notes about what was completed]
```
