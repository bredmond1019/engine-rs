# Update Task — Record progress in a task spec.

## Variables

$ARGUMENTS — space-separated values in this order:
  1. Target spec identifier, e.g. `<spec-slug>` (optional — if omitted, auto-detects the
     current spec from `planning/status.md`).
  2. Step number to mark done, e.g. `3`. Pass `0` to append a note without marking a step.
  3. Note text (everything after the step number) to append to the spec's Amendment Log, in
     `planning/<spec-slug>/amendments.md` — a sibling file to `tasks.md`/`tasks.json`, never
     inside `tasks.md` itself.

Examples:
  `/update-task 3 Finished scaffolding`                                          ← auto-detect spec, mark step 3, log amendment
  `/update-task 0 Still investigating the failing edge case`                      ← auto-detect spec, amendment only
  `/update-task <spec-slug> 2 Fixed the retired model id in frontmatter`  ← explicit spec, mark step 2, log amendment
  `/update-task <spec-slug> 0 Investigating the failing edge case`     ← explicit spec, amendment only

## Execution Model

**Run entirely inline. Spawn no subagent.** This is a single-file mark-done-and-append-note edit —
a subagent round trip adds latency without adding value.

## Instructions

1. **Resolve the target spec.**
   - If the first token of `$ARGUMENTS` matches a spec identifier pattern (e.g. `<spec-slug>`,
     `<spec-slug>`), resolve to `planning/<name>/tasks.md` and verify the file
     exists. If it does not exist, stop:
     > "No spec found at planning/<name>/tasks.md — run `/generate-tasks <name>` to create it."
   - Otherwise (first token is a number or `$ARGUMENTS` is empty), read `planning/status.md` to
     identify the current spec and load it. If no spec exists, say so and stop.

2. Parse the remaining arguments:
   - Step number: first integer token after the (optional) spec identifier. `0` = note-only.
   - Note text: all remaining text after the step number. May be empty.

3. Read the task spec.

4. If a non-zero step number was given, mark that step heading done by prepending `[done]` to the
   matching `### <N>.` line. If the step is already marked done, report that and skip.

5. If note text was provided, append it as one dated line to the spec's Amendment Log at
   `planning/<name>/amendments.md` — a sibling file to `tasks.md`, never a section inside it.
   - If `amendments.md` does not exist yet, create it first with this seed. **The OKF frontmatter
     is not optional** — standing rule 5 requires it of every new `.md` under `planning/`, and a
     file created without it red-gates `validate-brain` on **all four** flags at once
     (`--graph`, `--state`, `--links`, `--structure` each report the same missing-fence error), so
     one omitted fence reads as a corpus-wide regression. That happened on 2026-08-19, when this
     command created the fleet's first `amendments.md` without frontmatter.
     ```
     ---
     type: Log
     title: "Amendment Log — <name>"
     description: Deviations recorded while <name> actually ran - fixes, scope adjustments, substitutions.
     doc_id: <repo-slug>-amendments-<short-slug>
     layer: [<repo's layer>]
     project: <repo-slug>
     status: active
     keywords: [amendment log, d18, <2-3 terms from the spec>]
     related: [<repo-slug>-status]
     ---

     # <name> — Amendment Log

     Append-only. Records deviations from the spec as it actually ran — a fix, a scope
     adjustment, a substitution. Do not rewrite history — only append.
     ```
     Keep `doc_id` unique and stable — it is the graph's handle on this file. Then add a row for
     the new file to the directory's `index.md` if that directory has one (standing rule 7).
   - Then append one line (step `0` omits the `[task N]` tag):
     ```
     **YYYY-MM-DD** [task N]: <note text>
     ```
     ```
     **YYYY-MM-DD**: <note text>
     ```
   Never write this entry into `tasks.md` — `tasks.md` is a generated view (rendered by
   `scripts/render_spec.py` from the block record) and a regeneration would not know to
   preserve a note left inside it. `amendments.md` is untouched by any render, so an amendment
   survives regeneration by construction.

6. Write the updated files back. Preserve all other content and formatting exactly — the step-
   marking edit (step 4) touches only `tasks.md`; the amendment edit (step 5) touches only
   `amendments.md`.

7. Report what changed (see Report).

## Context / Files to Read

- `planning/status.md` — only if no spec identifier was provided in $ARGUMENTS
- The target `planning/<name>/tasks.md` — step marking only
- The target `planning/<name>/amendments.md` — amendment log; created on first use if absent

## Report

- Which spec was updated (full relative path).
- Which step was marked done (if any), or "no step marked" if step was 0.
- The amendment appended to `amendments.md` (if any), or "no amendment added".
- One-line success or failure of the file write(s).
