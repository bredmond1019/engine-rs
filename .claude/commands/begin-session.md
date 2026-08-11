---
type: Command
title: begin-session — Run one bounded operator session with an agent, and finish it
description: Open a named operator session, work it with the operator, and close it by producing the one artifact that ends it. A session is the unit for work that needs the human's judgement — defined entry, defined exit, dies when done. It is not a block with a person in it.
---
# Begin Session — one bounded piece of operator work, start to finish

`/orchestrate` drives work an agent can do alone. **`/begin-session` drives work it cannot** — the
decision, the credential, the judgement call, the thing only the operator can answer.

The failure this exists to prevent is measured, not theoretical. A Dev.to sweep whose dry-run was
**four identical one-line diffs** — fifteen seconds of review — sat unactioned for days *because the
decision lived in a markdown file nobody opens*. The tooling was finished. The gate was a document.

So a session is not a reminder and not a backlog row. It is a **sitting**, with:

- a **repo** you run it from (and, when relevant, a **machine**),
- an **exit artifact** — the thing that exists afterwards and did not before,
- and an ending. **A session that does not produce its artifact did not happen.**

## Variables

`$ARGUMENTS` — the session slug, optionally with flags.

```
Usage: /begin-session <session-slug> [--roadmap <path>] [--dry-run]
```

| Flag | Default | What it does |
|---|---|---|
| `<session-slug>` | **required** | e.g. `session-developer-offer`. Kebab-case. |
| `--roadmap <path>` | inferred | Where the session is defined, when it is not yet a graph edge. |
| `--dry-run` | off | Print the resolved session, its exit artifact and gated blocks; change nothing. |

Empty `$ARGUMENTS` → print usage, list every session you can resolve, and stop.

## Step 1 — Resolve the session

Look in this order and **stop at the first hit**:

1. **`planning/state.json` `depends_on` edges of type `session`** carrying this slug — the real home
   once `okf-core:OK.ticket.operator-edge-types` has landed. Collect **every** block gated by it,
   across every repo: one session commonly gates several, and the operator should see all of them.
2. **A roadmap's Wave 0 session table** (`--roadmap`, or search `planning/*/roadmap.md`) — where
   sessions live before the edge type exists.
3. **`planning/<slug>/notes.md`** — a `/capture` holding area.

If it resolves nowhere, **stop and say so.** Do not invent a session; a fabricated exit condition is
worse than no session, because it will be marked done.

Report before starting: the slug, the exit artifact, the repo, the machine if not this one, and
every block gated by it — **with their effective priority**. That last number is the point. A
session gating a P0 block *is* P0, and the operator is entitled to know that before deciding whether
to sit down.

## Step 2 — Check you are in the right place

- **Wrong repo** → say which one, give the `cd`, and stop. Do not work it from here.
- **Needs another machine** (the Mac Mini, a phone, a browser you are not driving) → say so up
  front, list every prerequisite in one block so the operator sets up once, and **group every step
  that needs that machine into this one sitting.** Making someone open an SSH session three times
  for three one-line changes is how sessions stop getting run.
- **Needs a credential or an access the agent does not have** → name it now, not at step 4.

## Step 3 — Work it

This is a conversation, not a checklist. The operator's judgement *is* the deliverable — your job is
to make it cheap to give.

- **Bring the decision to them fully prepared.** Read the sources, do the analysis, and open with
  the recommendation and its reasoning. "Here is what I would do and why — object or confirm" beats
  "here are five options" every time.
- **One decision at a time**, and never ask for a decision whose inputs you could have resolved
  yourself.
- **Write as you go**, not at the end. A session interrupted at 80% should leave 80% of its artifact
  on disk, not a transcript.
- **Record what was rejected and why.** The next agent re-proposes it otherwise — that is how a
  settled question gets re-litigated three roadmaps later.

## Step 4 — Close it

A session ends **only** when its exit artifact exists. Then, in order:

1. **Write the artifact**, at the path the session named. OKF frontmatter if it enters the corpus.
2. **Clear the gate.** Once the edge type exists: `mev close-session <slug> --exit-verified`. Until
   then, remove the session row from the roadmap's Wave 0 table and say what replaced it.
   `--exit-verified` is the operator asserting the artifact exists — **mev never infers it.**
3. **Commit with an explicit pathspec.** Every `planning/` is a symlink into one git index; a bare
   commit sweeps other sessions' staged work in.
4. **Report what unblocked.** Name the blocks that just became startable and the command that runs
   them. A session whose whole point was unblocking work should end by pointing at the work.

If the artifact is **not** produced, say so plainly and leave the session open. **Do not close a
session because the sitting ended.** A session marked done without its artifact is worse than one
never started: the gate is gone and the work is not.

## Files

- **Reads:** `planning/state.json`, the roadmap, `planning/<slug>/notes.md`, `brain.toml`
- **Writes:** the exit artifact, the gate clearing, and nothing else
- **Never writes:** another repo's `state.json` — report the change you want and let that repo apply
  it. One command writing state across repos is the contention pattern this fleet keeps getting bitten by.

## Traps

- **`timeout` does not exist on this macOS shell.**
- **A piped command's `$?` is the pipe's**, not the command's. Redirect, then check.
- **`rg`/`find` are symlink-blind** and every `planning/` is a symlink — pass `-L`, and `-uu` to
  reach gitignored sub-repos.
- **`state.json` round-trips** with `json.dump(..., indent=2, ensure_ascii=False)` plus a trailing
  newline. The default escapes every em dash and turns a three-field edit into ~130 lines of churn.
- **Do not let a session grow.** If it turns out to need work an agent could have done alone, file
  that as a block and keep the session to the decision. Sessions that absorb implementation stop
  being sittings and start being projects, and then they do not get run either.
