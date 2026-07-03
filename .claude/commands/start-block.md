# Start Block — Mark a block as in-progress in status.md.

## Variables

$ARGUMENTS — optional block identifier to start (e.g. `<spec-slug>`). If omitted, defaults to the first block that is not `Done` in status.md.

## Execution Model

Spawn a Haiku subagent (Agent tool, `model: "haiku"`) to execute all steps below.
Pass the resolved `$ARGUMENTS` value and the complete Instructions section in the subagent prompt.
Return the subagent's result to the user.

## Instructions

1. Read `planning/status.md`.
2. Identify the target block:
   - If `$ARGUMENTS` is provided, find that block by identifier. If not found, say so and stop.
   - If `$ARGUMENTS` is omitted, find the first block that is not `Done`.
3. Check preconditions:
   - If the block is already `In progress`, report that and stop.
   - If the block is `Done`, report that and stop.
   - If any block that must precede this one (all blocks above it in the sequence) is not `Done`, report which ones are incomplete and stop — do not skip the sequence.
4. Update the block's status to `In progress` in status.md. Preserve all other content and formatting.
5. Update the **Current focus** line to reflect this block.
6. Bump the **Last updated** date.
7. Write the updated status.md.

## Context / Files to Read

- `planning/status.md`

## Report

- Which block was marked in-progress.
- The updated Current focus line.
- Success or failure of the file write.
