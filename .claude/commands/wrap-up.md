# Wrap Up — Log work and commit at the end of a session.

Runs `/log-work` then `/commit` in sequence. Use this after finishing a piece of work to
sync status.md, append a log entry, and commit everything cleanly. For a full end-of-context
hand-off to a fresh agent, use `/handoff` instead.

## Variables

$ARGUMENTS — optional free-text note about what was done (passed straight through to
             `/log-work` as its narrative). May be brief ("shipped D17 --from flag") or
             detailed. If omitted, `/log-work` derives the narrative from git history.

## Instructions

1. **Drain any durable caveat first.** If this session surfaced something the next agent must not
   lose — a constraint, a known-issue/don't-re-investigate fact, an environmental gotcha, or a
   not-yet-ticketed deferred follow-on — append it to `planning/state.json` `carryover[]` (field shape
   in `docs/state/state-schema.md`). `/wrap-up` writes no handoff file, so `carryover[]` is the only
   place this kind of note survives. Skip if the session produced none or the repo has no `state.json`.
   **Cross-Repo Constraints Rule:** If a completed block spawns follow-up work in a different repo, **DO NOT** record it as a local `carryover`. You must actively open the downstream repo's `planning/state.json`, inject the new block into its `tracks` and `focus` arrays, and wire it into the `depends_on` DAG immediately.

2. Run `/log-work $ARGUMENTS` — this syncs status.md, appends the log entry, and syncs
   the company brain. Wait for it to complete before continuing.

3. Run `/commit` — stages and commits all remaining changes with a conventional message.

That's it. No handoff file, no context summary — just (drain →) log + commit.
