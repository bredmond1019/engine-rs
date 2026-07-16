# Blocked — Capture a new blocker on the fly

## Variables

$ARGUMENTS — what is blocked, by what, where, and why.

## Purpose

When getting ready to start work and you discover that a block or track is blocked by something new (e.g. an external manual step, a new chore, or a cross-repo dependency), use this command to update the dependency graph on the fly. This updates the `depends_on` array for the blocked item in `planning/state.json` and optionally helps capture the new work (e.g. via `/chore` or `/ticket`).

## Instructions

1. **Understand the blockage**: Analyze `$ARGUMENTS` to determine:
   - **Target**: Which block ID is being blocked (e.g., `MV.6.A`, `BL.1.D`).
   - **Blocker type**: Is the blocker an *external* step (e.g., creating a Cal.com account, moving a repo directory) or a *block* dependency (another ticket/chore in this or another repo)?
   
2. **Find the target `state.json`**: Locate the `planning/state.json` file in the repository that owns the target block.

3. **Update `state.json`**:
   Open the `planning/state.json`, locate the target block in `tracks[].blocks[]`, and append a new dependency object to its `depends_on` array:
   - For an external or manual step:
     `{ "type": "external", "what": "<concise description of the external blocker>" }`
   - For a block dependency:
     `{ "type": "block", "repo": "<repo-slug>", "id": "<block-id>" }`
     
   *Note: Ensure valid JSON.*

4. **Regenerate State**: Run `mev emit-state --write` from the brain root (or let it walk up to find `brain.toml`). This updates derived fields like `focus.blocked` and brain rollups so the system knows the block is now blocked.

5. **Follow-up (Optional)**: If the blocker is a significant new piece of code work that isn't tracked yet, suggest using `/chore` or `/ticket` to spec it out, so it can be tracked as a formal block dependency.

6. **Report**: Output a concise summary of what was updated, e.g.:
   ```
   Updated BL.1.D in bastiel/planning/state.json to be blocked by:
   - [External] Create a Cal.com account and links.

   State regenerated successfully.
   ```
