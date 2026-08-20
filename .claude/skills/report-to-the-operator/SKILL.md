---
name: report-to-the-operator
description: How to write the chat reply an operator actually reads when several agent sessions are running at once — the shape, the ceiling, the full cut-list, what always earns its space, and worked before/after rewrites. Use when authoring a command's Report or Output Format section, when closing out a run, when a reply is running past ten lines, and whenever you are about to explain your reasoning rather than state your result.
allowed-tools: Bash(grep:*) Bash(ls:*)
---

# Reporting to the operator

The operator runs **several agent sessions at once** and scans them. Your reply is not a document —
it is a status line they read in a few seconds before deciding whether to intervene. Everything
durable already goes to disk, because the commands require it. **The reply's job is to say what
happened and whether it needs them. Nothing else.**

The hard contract lives in `CLAUDE.md`. This skill is the reasoning, the full cut-list, and the
examples.

---

## The shape

1. **First line = the outcome.** What happened, and whether it needs them. No preamble, no
   restating the request.
2. **Then the specifics.** Bullets, one line each, max ~6. Facts, not narration.
3. **Last line = the ask**, if there is one. One question, answerable in a word.

**Ceiling: 10 lines for a normal turn, 20 for an end-of-run report.**

The ceiling is not a style preference. Ten lines x six sessions is a screenful; forty lines x six
sessions is a reading session the operator did not agree to.

---

## What to cut, in order of how much it costs

| Cut | Why |
|---|---|
| **Reasoning narration** — how you got there, what you considered, what you almost did | The transcript already holds the steps. Report conclusions. |
| **Restating a file you just wrote** | Link the path. The operator can open it. Two copies drift. |
| **Justifying decisions that worked** | Explain only what was non-obvious or what they might reverse. |
| **Unasked-for next steps, roadmaps, option menus** | They decide what is next. Offer one, if any. |
| **Self-assessment and stage direction** | "the finding that reframes everything", "worth your attention", praise, hedging, apology. |
| **Tables or headings under ~4 rows** | A sentence is faster to read than a table with two rows. |
| **Preamble** — "I'll start by...", "Let me..." | The tool calls already showed that. |

---

## What always earns its space

- **Failures and blocks — first, plainly, with the real error text.** Never soften, never bury
  below good news.
- **Anything that did not match what was asked.**
- **Assumptions the operator might reject**, and decisions that need their call.
- **Security, data-loss, or money implications.**
- **Exact identifiers where the identifier IS the content** — `sdlc-task.js:1481`, a commit SHA, an
  error code. Never a paragraph describing what a one-line reference would say.

---

## Worked rewrites

**A block closing.**

> Bad (14 lines): a paragraph on what the block changed, a paragraph on how it was verified, a
> table of the gates, a note about what the engine reported, and a closing thought about what this
> means for the next block.

> Good (3 lines):
> ```
> Block 4 closed — 7/7 tasks, 20/20 gates, state verified clean.
> - Six command defects fixed; detail in planning/orchestration-run/<slug>/notes.md
> Launching block 5.
> ```

**A failure.**

> Bad: "I want to flag something interesting I ran into. While working through task 3, I noticed
> that the check appeared to be failing, and after some investigation it turned out that..."

> Good:
> ```
> Block 3 BAILED at task 3 — engine-parse gate on a file the task deletes.
>   Error: Cannot find module '.../sdlc-block.js'
> Unpassable as specced. Rescoping files[] and re-running; nothing else is affected.
> ```

**Nothing to report.**

> Bad: three paragraphs explaining that everything is fine and why.

> Good: `Corpus clean: 0 errors, all four flags. No action needed.`

---

## Authoring a command's Report section

A command file is read seconds before the agent writes its reply, so it is the strongest place to
set output shape — stronger than `CLAUDE.md`, which is read once at session start and drifts.

Every command that produces operator-facing output should end with:

```md
## Report

<= N lines. First line: outcome + whether it needs the operator. Then <= 6 one-line bullets.
Link paths; never restate a file's contents. See the `report-to-the-operator` skill.

<a fenced example of the ideal reply for THIS command>
```

Two rules for writing one:

- **Give a fenced example, not a description.** An example is copied; a description is interpreted.
- **Separate disk from chat.** If a section specifies a file (frontmatter, a template), title it so
  that is obvious — `## Output Format — planning/<slug>/notes.md` — and keep the chat contract in a
  separate `## Report`. Conflating them is why some commands specify a 70-line "report" that was
  never meant to be spoken.

---

## Reporting a workflow result

`/sdlc-task` and `/sdlc-flow` finish inside subagents. Their JSON result comes back to the **calling
session**, which writes the operator-facing summary — so the ceiling applies to *you*, not the
engine. Never paste the returned JSON, the token roll-up, or the stage list. From a completed run
the operator needs four things:

```
<block-id>: <PASS|BAILED> — <n>/<m> tasks, <gates>/<total> gates[, PR #<n> merged]
- <the one thing that went wrong, if any, with its real error>
Next: <what you are doing now>
```
