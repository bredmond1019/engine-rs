---
type: Command
title: next — Show what's up next, what's blocked, and recommend the next action
description: Tight briefing on what's next, what's blocked, and a recommended next action based on local status and HQ/business/core goals.
---
# Next — What's up next, what's blocked, and recommendation

Show what's next, why, what's blocked and by what, and recommend the next action based on local context (like handoff.md, status.md, and state.json) and overall company goals in HQ, Business, and Core.

## Instructions

1. **Locate Brain Root (`BRAIN_ROOT`)**: Walk up from the current directory looking for `brain.toml`.
   - If not found, run as a standalone repository (degrade gracefully: skip HQ, Business, and Core goals sections and base the recommendation solely on local files).
   - If found, mark that directory as `BRAIN_ROOT`.

2. **Read Local planning files**:
   - Check if `planning/handoff.md` exists. If so, read it.
   - Read `planning/status.md` (current focus and momentum).
   - Read `planning/state.json` (focus and carryover array).

3. **Read Brain / Sibling goals (if `brain.toml` is found)**:
   - Read `planning/status.md` in `BRAIN_ROOT` (HQ Company Brain goals and Operating Board).
   - Read `business/planning/status.md` in `BRAIN_ROOT` (Business sub-brain goals).
   - Read `core/planning/status.md` in `BRAIN_ROOT` (Core sub-brain goals).

4. **Synthesize the State**:
   - **Up Next**: Identify items that are ready to be worked on. This includes:
     - The first command and remaining tasks in `planning/handoff.md` (if present).
     - Focus "now" or "next" items in local `planning/status.md` or `planning/state.json`.
     - Carryover items in local `planning/state.json` that are active.
   - **Blocked**: Identify items that are blocked, what they are blocked by, and why (from local `planning/status.md` or `planning/state.json`).
   - **HQ/Business/Core context**: Summarize the active company/sub-brain focus in 1-2 sentences each.

5. **Generate the Recommendation**:
   - Recommend a single next command/action.
   - Provide a clear, logical explanation for *why* this is recommended, showing how it unblocks local carryovers or tasks, and how it aligns with the overall goals of HQ, Business, or Core.
   - If there are multiple items that could be next, briefly present each option (with a one-sentence overview) and justify why the recommended one is preferred.

6. **Format Output**: Produce a short, clean, and sweet briefing (similar in length and style to `/session-recap`, under 300 words).

---

## Output Format

### ⏭ Up Next
- **<Item/Task Title or Slug>** - <Brief overview/gist of the work.>
...

### 🚫 Blocked
- **<Blocked Item>** - Blocked by: `<Blocker>` (Why: <concise explanation>).
- (Or: "None.")

### 💡 Recommendation
- **Recommended Action:** `<exact command or next step>`
- **Rationale:** <Why this is the best next step, linking it to local handoff/state and overall company/core/business goals.>

---

## Context / Files to Read

- `planning/handoff.md` (if present)
- `planning/status.md`
- `planning/state.json`
- `[BRAIN_ROOT]/planning/status.md` (if brain.toml found)
- `[BRAIN_ROOT]/business/planning/status.md` (if brain.toml found)
- `[BRAIN_ROOT]/core/planning/status.md` (if brain.toml found)
