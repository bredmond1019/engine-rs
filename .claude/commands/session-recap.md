# Session Recap — Summarize recent work and current standing before starting a session.

Reads the most recent Log entries and status.md to produce a tight briefing: what was
completed recently, where work left off, and the exact next command to run. Use this at the
start of a coding session before doing anything else.

## Instructions

0. Check for an active handoff: `ls planning/handoff.md 2>/dev/null`.
   - If the file exists, read it and **lead the briefing** with an Active Handoff section:
     ```
     ## Active Handoff — <title from handoff.md>
     <What's in flight and why.>
     Remaining: <bullet list from "Remaining work">
     First command: `<command from "First command after /prime">`
     > Delete `planning/handoff.md` once this session has consumed it.
     ```
   - If absent, skip silently and proceed to step 1.

1. Read `log.md`. Focus on the three most recent entries (identified by `## YYYY-MM-DD`
   headings). Extract: what was built or changed, any decisions made, and any explicit
   "left off at" or "next step" notes the author wrote.

2. Read `planning/status.md`. Extract:
   - Current focus block
   - Which blocks are `In progress`, `Done`, and `Not started` (status is title-case in
     status.md; tolerate lowercase variants if present)
   - Last updated date

3. If a task spec exists for the current focus block at
   `planning/<name>/tasks.md`, read it. Identify:
   - Which steps are marked done (have `[done]` prefix)
   - Which steps remain
   - Any notes in the `## Notes` section

4. Check `planning/<name>/sdlc/reports/` for any existing pipeline reports for the current
   block (e.g. `implement.md`, `task3-test.md`). Note which pipeline steps have been completed
   (have a report file) and which haven't.

5. Read `planning/state.json` if present. Extract any active `carryover[]` entries (those whose
   `clears_when` is unresolved) — durable constraints, known-issues, env caveats, and deferred
   follow-ons. Skip silently if the file or array is absent.

6. Output the briefing in this exact format — keep it under 300 words:

---

## Recent Work
<2–4 bullet points from the latest Log entries — what was built, changed, or decided.
Use the author's own language where possible.>

## Where We Left Off
<One paragraph. State: current block, last completed pipeline step (based on report files
present), last completed spec task (based on `[done]` markers), and anything explicitly noted
as "in flight" or "next" in the Log.>

## Remaining Spec Tasks
<Numbered list of un-checked (no `[done]` prefix) steps from the current task spec.
If the spec is missing, say so. If all steps are done, say "All steps marked complete.">

## Carryover
<One line per active `carryover[]` entry: `slug` (`kind`) — gist. Omit this section entirely if
there are none. Flag any `kind: env` caveat that gates the next step (e.g. "rebuild binary first").>

## Next Pipeline Step
<Single line: the exact command to run next, with the full argument.>
Example: `/test planning/<spec-slug>/tasks.md 2`
If the block is complete: `Run /log-work to wrap up, then /start-block for the next block.`

---

Do not read any source code files. Do not run any commands. This is read-only.

## Context / Files to Read

- `planning/handoff.md` (if present — check with ls first)
- `log.md` (last 3 entries)
- `planning/status.md`
- `planning/state.json` (the `carryover[]` array, if present)
- `planning/<name>/tasks.md` (current block's spec, if it exists)
- `planning/<name>/sdlc/reports/` (directory listing to check which step reports exist)
