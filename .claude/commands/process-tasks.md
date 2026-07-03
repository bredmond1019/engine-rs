# Process Tasks — Analyze the block sequence and report what is eligible to start.

## Instructions

1. Read `planning/status.md`.
2. Parse every block in the sequence with its current status.
3. Apply eligibility rules (status literals in status.md are title-case — tolerate the full set `Not started` · `In progress` · `Done` · `Blocked` · `Skipped`):
   - `Done` — already complete, not eligible.
   - `In progress` — already running, not eligible to start again.
   - `Skipped` — intentionally not done; treated as satisfied for downstream prerequisites.
   - `Blocked` — explicitly held; not eligible until unblocked.
   - `Not started` — eligible ONLY if every block above it in the sequence is `Done` (or `Skipped`). Otherwise it is blocked.
4. Return a structured report (see Output Format). Do not modify any file.

## Eligibility Rules

- A block is **ready** if it is `Not started` and all preceding blocks are `Done` (or `Skipped`).
- A block is **blocked** if it is `Not started` and any preceding block is not yet `Done`.
- A block is **in progress** if it is currently being worked on.
- A block is **done** if it is complete.

## Context / Files to Read

- `planning/status.md`

## Output Format

Return a structured list in this format:

```
## Block Eligibility Report

Current focus: <value from status.md>

| Block | Status | Eligible? | Reason |
|---|---|---|---|
| phase1-block1 | Done | — | complete |
| phase1-block2 | In progress | — | already running |
| phase1-block3 | Not started | [ready] | all prerequisites done |
| phase2-block1 | Not started | (blocked) | phase1-block3 not done |

## Next eligible block
<block identifier and one-sentence description of what it covers>
```

If no block is eligible (e.g. all are done or all remaining are blocked by in-progress work), say so explicitly.
