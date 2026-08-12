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

   **File operator work as a graph edge, never as prose.** Anything this session is leaving for
   the operator to decide, review, approve, or judge is filed as a `{"type":"operator", slug,
   exit, start, what?}` entry in `depends_on` on the block(s) it gates — **not** as a `carryover`
   entry and **not** as a note in this command's output. `slug` is kebab-case, prefixed
   `operator-`; `exit` names the artifact whose existence ends the gate (never a description of
   the work); `start` is a paste-ready command. Use `{"type":"approval", slug, what, digest}`
   instead when the decision is a single reducible yes/no on a fixed payload. **Why:** an operator
   (or approval) edge inherits the effective priority of everything it gates and surfaces in
   `/next` as the reason work cannot start; prose surfaces nowhere. Skip if the repo has no
   `state.json`.

2. Run `/log-work $ARGUMENTS` — this syncs status.md, appends the log entry, and syncs
   the company brain. Wait for it to complete before continuing.

3. Run `/commit` — stages and commits all remaining changes with a conventional message.

That's it. No handoff file, no context summary — just (drain →) log + commit.
