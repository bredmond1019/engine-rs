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
8. **Flip the matching block's status in `planning/state.json`, if the repo has one.** `status.md` and
   `state.json` both name the same block — a `/start-block` that touches only `status.md` leaves the
   authoritative graph stale. Resolve the target block's canonical ID (the `<BlockID>` in `status.md`'s
   row, or the id it maps to in `state.json`'s `tracks[].blocks[]` if `status.md` only carries a bare
   letter) and:
   - Find that block in `tracks[].blocks[]` (search every track). If it does not exist, report that and
     stop — do not fabricate a new block entry here (that's `/generate-master-plan`/`/plan`/`/chore`/
     `/ticket`'s job).
   - Set its `status` to `"in_progress"` (an authored value — never hand-set `"blocked"`, see
     `core/planning/state-schema.md`).
   - Save `planning/state.json` and validate it is still valid JSON:
     `python3 -c "import json;json.load(open('planning/state.json'))"`.
   - Run `mev emit-state --write` to refresh `focus.now`/`focus.next` from the new status.

## Context / Files to Read

- `planning/status.md`
- `planning/state.json` (if present)

## Report

- Which block was marked in-progress.
- The updated Current focus line.
- Whether `planning/state.json` was updated (and to what status), or that no `state.json` exists.
- Success or failure of the file write(s).
