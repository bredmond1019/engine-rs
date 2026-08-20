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
   lose, append it to `planning/state.json` `carryover[]` (field shape in
   `docs/state/state-schema.md`) with one of these `kind` values:

   | kind | for |
   |---|---|
   | `defect` | a real unticketed bug with a fix — not yet filed as its own block |
   | `deferred` | a real follow-on you haven't ticketed yet |
   | `drift` | a doc, comment, block title or generated surface that has fallen out of step with the code or the graph |
   | `env` | a transient environmental caveat ("installed binary is stale, rebuild first") |

   `constraint` and `known_issue` are **retired** (HQ D72) — okf-core preserves them only through its
   `Unknown(String)` fallback so legacy entries still round-trip. Do not mint new entries with either.

   `/wrap-up` writes no handoff file, so `carryover[]` is the only place this kind of note
   survives. Skip if the session produced none or the repo has no `state.json`.

   **Route at write time — three destinations, not two.** Ask both questions before appending:

   1. **Can only a human do this?** A decision only the operator can make, a credential only they
      hold, a judgement call, a thing they must look at — that is **not** a `carryover[]` entry. File
      it as a `{"type":"operator", slug, exit, start, what?}` edge on the block it gates, per the
      operator-work rule below. **Why it matters here and not only there:** a carryover entry gates
      nothing, so operator work parked in it is never forced; an operator edge blocks the work standing
      behind it, which is what gets it done. Measured 2026-08-19 — **30 of the fleet's 202 `carryover[]`
      entries are operator work misfiled this way**, filed as `defect` or `deferred` because the table
      above offers no row meaning "not an agent's to do."
   2. **Is it permanently true?** A gotcha still true next month, a deliberate non-fix nobody intends to
      reverse, a load-bearing measured number someone will need again — that belongs in `reference[]`.
      A fact with no `clears_when` because nothing will ever make it stop being true is the signal.
      See `docs/state/reference-container-schema.md` for its field table and kind vocabulary.

   Only what survives both questions is a `carryover[]` entry: work-class findings that eventually
   clear — an unticketed defect, a deferred follow-on, a drifted surface, a transient env caveat.

   **Run `mev validate-state planning/state.json` immediately after this step's write — this is
   a mandatory step, not a suggestion to consider.** Treat a nonzero exit as blocking: read the
   reported error, fix the entry, and re-run until it passes. Skip only if the repo has no
   `planning/state.json`.

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

## Report

**<= 10 lines.** First line: outcome + whether it needs the operator. Then <= 6 one-line
bullets. Link paths; never restate a file. See the `report-to-the-operator` skill.

```
Logged and committed — <n> file(s), <commit sha>
- <carryover/operator edges filed this session, or "none">
- <anything left unfinished>
```
