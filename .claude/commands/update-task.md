# Update Task — Record progress in a task spec.

## Variables

$ARGUMENTS — space-separated values in this order:
  1. Target spec identifier, e.g. `<spec-slug>` (optional — if omitted, auto-detects the
     current spec from `planning/status.md`).
  2. Step number to mark done, e.g. `3`. Pass `0` to append a note without marking a step.
  3. Note text (everything after the step number) to append to the spec's `## Notes` section.

Examples:
  `/update-task 3 Finished scaffolding`                                          ← auto-detect spec, mark step 3, append note
  `/update-task 0 Still investigating the failing edge case`                      ← auto-detect spec, note only
  `/update-task <spec-slug> 2 Fixed the retired model id in frontmatter`  ← explicit spec, mark step 2, append note
  `/update-task <spec-slug> 0 Investigating the failing edge case`     ← explicit spec, note only

## Execution Model

Spawn a Haiku subagent (Agent tool, `model: "haiku"`) to execute all steps below.
Pass the resolved `$ARGUMENTS` value and the complete Instructions section in the subagent prompt.
Return the subagent's result to the user.

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

5. If note text was provided, append it to the `## Notes` section of the spec, prefixed with
   today's date:
   ```
   **YYYY-MM-DD**: <note text>
   ```

6. Write the updated spec back. Preserve all other content and formatting exactly.

7. Report what changed (see Report).

## Context / Files to Read

- `planning/status.md` — only if no spec identifier was provided in $ARGUMENTS
- The target `planning/<name>/tasks.md`

## Report

- Which spec was updated (full relative path).
- Which step was marked done (if any), or "no step marked" if step was 0.
- The note appended (if any), or "no note added".
- One-line success or failure of the file write.
