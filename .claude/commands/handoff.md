# Handoff — Hand off an in-flight session cleanly to a fresh agent.

Use this when the current session has grown large enough that continuing in a new context is
better than pressing on. It creates or updates `planning/handoff.md` so the next agent can
orient instantly after running `/prime`, then wraps up the session by logging work and
committing lingering changes.

## Variables

$ARGUMENTS — optional free-text note to include in the handoff (e.g. "focus on the parser
             next, the renderer is blocked on the tokenizer fix"). If omitted, the agent
             derives context from git history and status.md.

## Execution Model

Run inline — do NOT spawn a subagent. `/log-work` and `/commit` are invoked as Skill tool
calls from the main agent context; they have their own confirmation gates that require the
main loop.

## Instructions

### Step 1 — Gather current state

Read these files (all relative to the project root):

- `planning/status.md` — current focus, block statuses, last updated date
- `log.md` — the three most recent entries (for narrative context)
- Any in-flight task spec visible from status.md (e.g. `planning/<current-block>/tasks.md`)
- `planning/handoff.md` — if it exists, read it (you are updating it, not replacing blindly)
- `planning/state.json` — the existing `carryover[]` (you are appending to it, not duplicating)
- `docs/state/state-schema.md` — the `carryover[]` section, for the field shape

Run:
- `git log --oneline -10` — recent commits
- `git diff --stat` — uncommitted changes
- `git status` — untracked files and staged state

### Step 2 — Drain durable context into `state.json` `carryover[]`

**This is what keeps `handoff.md` disposable.** Before writing the handoff, route anything that must
outlive *this* handoff (so the next one can't overwrite it away) into the right durable home — do **not**
leave it living only in the prose below:

- **Committed, sequenced work** with real dependencies → a `tracks[].blocks[]` block in `planning/state.json`.
- **Free-floating ideas/chores** not on this repo's critical path → the company brain's HQ `backlog[]`
  (via `/backlog-ticket` at the brain).
- **Durable caveats, known-issues, environmental notes, and not-yet-ticketed deferred follow-ons** →
  append a `carryover[]` entry to `planning/state.json`. This is the in-between lane the other two miss:
  - `kind: constraint` — a rule the next agent must honor.
  - `kind: known_issue` — a don't-re-investigate fact.
  - `kind: env` — a transient environmental caveat (e.g. "installed binary is stale, rebuild first").
  - `kind: deferred` — a real follow-on you haven't ticketed yet; promote it to a block/backlog when ready.

  Follow the `carryover[]` field shape in `docs/state/state-schema.md` (`slug`, `scope`, `kind`, `text`,
  optional `related` + `clears_when`, `created`). Keep it valid JSON; append, don't duplicate an existing
  slug. **Delete** any existing `carryover[]` entry whose `clears_when` resolved this session. If this repo
  has no `planning/state.json` yet, skip this step.

After making any changes to `planning/state.json`'s `carryover[]`, or after writing/updating a
block's `tasks.json`, run `mev emit-state --write` to ensure the new state is tracked across
brains. Never hand-edit a block's `tasks` field yourself — it's a derived pointer + status summary
(see `docs/state/state-schema.md`), not something you inject entries into.

The handoff prose in Step 3 then *points at* these slugs instead of being their only home.

### Step 3 — Write `planning/handoff.md`

Create or overwrite `planning/handoff.md` using the template below. Be specific and honest:
the next agent has zero session memory and will rely entirely on this file + `/prime` to
orient. If $ARGUMENTS was provided, weave it in as the primary focus note.

```markdown
---
type: Handoff
created: YYYY-MM-DD
---

# Handoff — <5–10 word title: what's in flight>

> **For the next agent:** Read this immediately after `/prime`. Delete this file once consumed.

## What we're doing and why
<One paragraph. State the goal, why it matters right now, and any non-obvious background
the next agent needs to avoid re-deriving it. Reference specific file paths or decision
numbers where helpful.>

## Completed this session
<Bulleted list of concrete things done — commits made, files changed, decisions reached.
Pull from git log and $ARGUMENTS. Be specific: "bumped harness-config loader to sonnet in
all 3 engines (sdlc-block.js:473, sdlc-task.js:455, sdlc-run.js:326)" not "fixed engine".>

## Remaining work
<Bulleted list of what's left, in priority order. Mark blockers explicitly.
If work is blocked on an answer to an open question, say so.>

## Durable State Updates
<List any items you added to `state.json`'s `carryover[]`, and any block whose `tasks.json` you
created or changed this session. Note their slug / block ID so the next agent can find them easily
without having to hunt.>

## Open questions / choices
<Bulleted list of unresolved decisions or things to verify before proceeding. If none, write
"None — clear to proceed.">

## Context the next agent needs
<Only ephemeral, this-session framing the next agent needs to read the above. Durable constraints,
known-issues, and env caveats belong in `state.json` `carryover[]` (Step 2) — reference their slugs
here, don't restate them. Omit this section if Step 2 captured everything.>

## First command after `/prime`
`<exact command to run first>`
```

Fill every section. Do not leave placeholder text. If a section is genuinely empty, say why
(e.g. "No open questions — the approach is settled per D14.").

### Step 4 — Invoke `/log-work`

Invoke the `/log-work` skill. Pass $ARGUMENTS as the argument if it was provided, so the
log entry gets the same narrative. `/log-work` will ask for confirmation before writing — let
that flow normally.

### Step 5 — Invoke `/commit`

After `/log-work` completes, invoke the `/commit` skill. This will pick up `planning/handoff.md`
(just written), the `state.json` `carryover[]` edits, plus any other uncommitted changes. `/commit`
will show staged files and ask for confirmation — let that flow normally.

### Step 6 — Report

Tell the user:
- `planning/handoff.md` was written (or updated)
- Log entry was appended
- What was committed
- The exact command sequence to start the next session:
  1. Open a fresh Claude Code session in this directory
  2. Run `/prime` — it will surface the handoff automatically
  3. Run the first command listed in the handoff

## Context / Files to Read

- `planning/status.md`
- `log.md` (last 3 entries)
- The current in-flight task spec (path from status.md, if any)
- `planning/handoff.md` (if it already exists)
- `planning/state.json` (existing `carryover[]`) + `docs/state/state-schema.md` (`carryover[]` field shape)
