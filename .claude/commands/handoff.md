# Handoff — Hand off an in-flight session cleanly to a fresh agent.

Use this when the current session has grown large enough that continuing in a new context is
better than pressing on. It writes `planning/handoff.md` so the next agent can orient instantly
after `/prime`, logs the work, and commits.

## Variables

$ARGUMENTS — optional free-text note to include in the handoff (e.g. "focus on the parser
             next, the renderer is blocked on the tokenizer fix"). If omitted, derive context
             from git history and status.md.

## Execution Model

**Run entirely inline. Spawn no subagents, and do NOT invoke `/log-work`, `/commit`, or
`/backlog-ticket` as skills — their steps are inlined below.**

This is deliberate and is the whole point of the command. `/handoff` exists to *serialize the
current session's context to disk*, and the main agent is the only thing that holds that
context. A subagent cold-starts and has to reconstruct the session narrative from `git log` —
slow, lossy, and exactly the reconstruction the handoff is meant to prevent. Delegating also
fires at the end of a long session, when re-serializing instructions into a subagent prompt
costs the most.

Budget: ~3 file reads, ~3 file writes, 2 shell calls. If you find yourself spawning an agent
or reading more than five files, stop — you are re-deriving something you already know.

## Instructions

### Step 1 — Gather (3 reads + 1 shell call)

Read:
- `planning/status.md` — current focus, block statuses
- `planning/state.json` — `tracks[].blocks[]` statuses and the existing `carryover[]`
- `planning/handoff.md` — only if it exists (you are updating, not blindly replacing)

Run once: `git log --oneline -10 && git status --short`

Do **not** re-read `log.md`, the task spec, or `docs/state/state-schema.md` — you have the
session context those would reconstruct. Read them only if you genuinely lack something.

### Step 2 — Flip closed blocks, then drain durable context

**2a — Flip any block this session closed** to `status: "closed"` in `planning/state.json`
`tracks[].blocks[]`. Do this **before** Step 4's `emit-state`: that authored field is the
*input* the derivation reads, and `emit-state` never infers completion from `status.md` (the
sync is one-way by design). Skipping this leaves `focus` and every generated surface stale
until someone reconciles by hand — the `engine-rs` `state-json-block-status-stale` incident,
2026-07-03.

**2b — Drain anything that must outlive this handoff** into `planning/state.json`'s
`carryover[]`, so the next handoff can't overwrite it away. One quick pass, not a routing
ceremony — append an entry with `slug`, `scope`, `kind`, `text`, `created` as the required
core, plus these optional fields — `docs/state/state-schema.md` is the authoritative field
table; this restates only what an agent needs inline while appending:

| kind | for |
|---|---|
| `constraint` | a rule the next agent must honor |
| `known_issue` | a don't-re-investigate fact |
| `env` | a transient environmental caveat ("installed binary is stale, rebuild first") |
| `deferred` | a real follow-on you haven't ticketed yet |

- `priority` (int, `0..=3`) — value if resolved, on the same rubric as `tracks[].blocks[]`.
  Omit when the entry carries no value judgement.
- `blocks` (array) — edges to the work this entry blocks, same forms as `depends_on`
  (`{type:"block",repo,id}` / `{type:"external",what}`); feeds the same reverse-topological
  `min`-propagation that derives `effective_priority`. Omit (don't write `[]`) when it blocks
  nothing.
- `finding_id` (string) — free-form join key so `mev carryover` can correlate the same finding
  filed in multiple repos.
- `related`, `reviewed`, `snoozed_until` — as documented in `docs/state/state-schema.md`.
- `clears_when` — either the legacy human-readable string (for genuinely subjective
  conditions), or a **typed predicate** object mev can evaluate itself: `block_closed`
  (`repo`, `id`), `file_exists` (`path`), `file_contains` (`path`, `pattern`), or
  `command_exits_zero` (`command`) — each takes an optional `note`. Prefer the typed form
  whenever the condition is checkable.

**Only entries with a typed `clears_when` predicate are machine-evaluable by `mev carryover`**
— a prose `clears_when` (or none) lands the entry in its not-evaluable lane. `priority` and
`finding_id` are what make an entry rankable and cross-repo-correlatable; an entry with none
of these three still counts, but sits inert until someone triages it by hand.

Append; don't duplicate an existing slug. **Delete** any entry whose `clears_when` resolved
this session. Skip entirely if this repo has no `planning/state.json`.

Sequenced work with real dependencies belongs in `tracks[].blocks[]`, and free-floating ideas
belong in the HQ `backlog[]` — but **do not invoke `/update-state` or `/backlog-ticket` from
here**. Note them in the handoff prose and let the next session file them properly.

Never hand-edit a block's `tasks` field — it's a derived pointer, not somewhere to inject
entries.

**2c — File operator work as a graph edge, never as prose.** Anything this session is leaving
for the operator to decide, review, approve, or judge — a call only they can make, a credential
only they hold, a thing they must look at — is filed as a `{"type":"operator", slug, exit,
start, what?}` entry in `depends_on` on the block(s) it gates, **not** written into the handoff
prose, a `note` field, or an `## Open questions` bullet. `slug` is kebab-case, prefixed
`operator-`; `exit` names the artifact whose existence ends the gate (never a description of the
work — e.g. `planning/decision-rate-card.md exists`, not "decide on pricing"); `start` is a
paste-ready command the operator runs to begin. If the decision reduces to a single yes/no on a
fixed payload, use `{"type":"approval", slug, what, digest}` instead. **Why:** an operator (or
approval) edge inherits the effective priority of everything it gates and surfaces in `/next` as
the reason work cannot start; prose in a handoff file surfaces nowhere and is exactly how these
get left for days. Skip entirely if this repo has no `planning/state.json` — say so explicitly in
the handoff instead (Step 3) and name who is expected to file it once one exists.

### Step 3 — Write `planning/handoff.md`

The next agent has zero session memory. Be specific and honest. If `$ARGUMENTS` was provided,
weave it in as the primary focus note.

```markdown
---
type: Handoff
created: YYYY-MM-DD
---

# Handoff — <5–10 word title: what's in flight>

> **For the next agent:** Read this immediately after `/prime`. Delete this file once consumed.

## What we're doing and why
<One paragraph: the goal, why it matters now, and any non-obvious background the next agent
would otherwise re-derive. Cite file paths and decision numbers.>

## Completed this session
<Concrete things done — commits, files changed, decisions reached. "bumped harness-config
loader to sonnet in all 3 engines (sdlc-block.js:473, sdlc-task.js:455, sdlc-run.js:326)",
not "fixed engine".>

## Remaining work
<What's left, in priority order. Mark blockers explicitly.>

## Durable State Updates
<`carryover[]` slugs added or deleted, and any block whose status you flipped. Slug / block ID
only — the next agent can look them up.>

## Open questions / choices
<Name the `operator-`/`approval` slugs already filed in `depends_on` (Step 2c) and what each
gates — this section points at the graph, it does not substitute for it. If truly nothing
needs the operator: "None — clear to proceed.">

## First command after `/prime`
`<exact command to run first>`
```

**Omit a section rather than padding it.** An absent "Open questions" reads as "none"; a
section full of filler wastes the next agent's attention, which is the one thing this file
exists to protect.

### Step 4 — Log, regenerate, commit (2 shell calls, no delegation)

**4a — Append to `log.md`** (repo root, `type: Log`; create with OKF frontmatter if missing).
Add a `### <title>` sub-entry under today's `## [YYYY-MM-DD]` section, creating that date
section at the top just below `# Log` if absent:

```markdown
### <Short title of the session>
- **What:** <what was built / changed / decided>
- **Why:** <what prompted it — the problem, request, or insight>
- **Refs:** <driving plan / decision / tasks — omit if none>
```

**Why** is required. If `$ARGUMENTS` doesn't make the reason clear, ask one brief question.
Bump `log.md`'s frontmatter `timestamp` to the current ISO-8601 time.

**4b — Bump `planning/status.md`'s frontmatter `timestamp`** to the same time, and update any
hand-maintained prose (`## Momentum`, narrative callouts) that this session changed. Do **not**
hand-write the focus line — Step 4c derives it. Never edit `master-plan.md` from this command.

**4c — Run `mev emit-state --write`.** It walks up to find `brain.toml` itself; no `cd` needed.
This regenerates every derived surface from the state you authored in Step 2a: leaf `state.json`
focus fields, the brain rollup, the per-project cache doc + `synced_from` watermark, tier
rollups, the HQ Operating Board, and `master-plan.md`'s wave tables.

Do not reimplement any of that by hand. If the run reports `W_EMIT_NO_SENTINEL` against a
target this repo feeds, report it rather than inventing the missing sentinel pair.

- **No `brain.toml` found** (standalone repo) → skip 4c and say so in the report.
- **`_root` repos only** (`brain`, `learn-ai`, `base-template`) → additionally update *this*
  repo's `###` subsection in `BRAIN_ROOT/README.md`'s `## Quick Status` by hand; `emit-state`
  doesn't generate it (no `generated:` sentinel). Verify the heading matches before writing —
  never touch another project's subsection.

**4d — Commit.** Stage `planning/handoff.md`, the `state.json` edits, `log.md`, `status.md`, and
any other uncommitted work. Write a conventional-commit message. Show the staged file list and
get confirmation before committing. Branch first if on the default branch. Do not push unless
asked.

### Step 5 — Report

- `planning/handoff.md` written (or updated)
- Blocks flipped to `closed`; `carryover[]` slugs added or cleared
- Any `operator`/`approval` edges filed this session, and what they gate
- The `emit-state --write` summary, or that it was skipped (standalone)
- What was committed
- Next session: open a fresh session here → `/prime` (it surfaces the handoff automatically) →
  run the first command listed in the handoff

If a settled architectural decision came out of this session, say so and suggest `/log-decision`
— do not author one inline.

## Context / Files to Read

- `planning/status.md`
- `planning/state.json` (`tracks[].blocks[]` + `carryover[]`)
- `planning/handoff.md` (only if it already exists)
