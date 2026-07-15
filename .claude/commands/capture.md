# Capture — Scaffold a pre-plan notes file and optionally add a backlog ticket

Captures rich conversation content from a planning or research session. Creates `planning/<slug>/notes.md` in the current repo as a structured holding area. If the `--backlog` flag is provided, it calls the `/backlog-ticket` command to create a backlog item pointing to these notes.

## Variables

$ARGUMENTS — title or free-form description of what to capture, optionally including the `--backlog` flag.
             A slug is derived from this automatically.
             Example: "--backlog Work email setup with instructions"
             Example: "notion-dashboard Notion API read-only dashboard concept"

## Execution Model

Spawn a subagent (Agent tool) to execute all steps below. Pass the resolved `$ARGUMENTS`
and this whole Instructions section in the subagent prompt. Return the subagent's result
to the user.

## Instructions

### Step 0 — Resolve the brain root (same walk-up as /log-work)

1. From the current working directory, walk **up** parent by parent looking for a
   `brain.toml` file (its first line begins `# brain.toml`). The directory containing it
   is `BRAIN_ROOT`.
   - **If no `brain.toml` is found**, this is a standalone repo. If `--backlog` is provided, `/backlog-ticket` will handle standalone logic. Still create the local notes file.
2. The brain backlog path is `<BRAIN_ROOT>/planning/backlog.md`.

### Step 1 — Parse arguments

3. From $ARGUMENTS derive:
   - **slug** — kebab-case, 2–4 words (e.g. `work-email`, `notion-dashboard`)
   - **title** — human-readable (e.g. `Work Email Setup`, `Notion Dashboard`)
   - **description** — one-line summary for the frontmatter
   - **repo** — the current repo's slug (read from `brain.toml` `[[repos]]` entry matching
     this repo's path; fall back to the directory name if standalone)
   - **type** — `feature` · `improvement` · `research` · `content` · `business` · `planning-session`
   - **keywords** — 3–5 topic terms

### Step 2 — Guard

4. Check whether `planning/<slug>/` already exists. If it does and `notes.md` is present,
   stop and tell the user — do not overwrite existing content.

### Step 3 — Create the notes file

5. Create `planning/<slug>/notes.md` using the Output Format below. You must populate the body sections with all the important details discussed during the session, but **do not invent content the user or agent hasn't provided/visually seen**. Ensure you capture file paths, class/struct names, functions, important snippets of code, and any additional content that will make it EXTREMELY easy for the next agent or the user to go dig into this note and know exactly what was discussed, how you got to this conclusion or initial research, where to go look to review/investigate further, etc.
   - **Populate `related:` with ≥1 real `doc_id`** — the project's `master-plan` doc_id, a
     governing decision, or the parent `index`. Never ship `related: []`: a doc_id-bearing file
     with zero outbound edges is an isolated graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`). Use
     genuine doc_ids that exist in the corpus — do not invent one to satisfy the rule.

### Step 4 — Backlog ticket (only if --backlog flag is present)

6. If the `--backlog` flag is provided in `$ARGUMENTS`, call the `/backlog-ticket` command, passing the title and referencing the newly created `planning/<slug>/notes.md` file. If the flag is not provided, skip this step.

### Step 5 — Report

7. Shell out to `mev emit-state --write` to update the brain's focus derivation and state.

8. Confirm: output the local path created and (if applicable) confirm the backlog ticket was created.

## Output Format — `planning/<slug>/notes.md`

```markdown
---
type: Note
title: <Title>
description: <one-line summary>
doc_id: <slug>
layer: [<inferred layer>]
project: <repo slug>
status: draft
keywords: [<3-5 terms>]
related: [<≥1 real doc_id>]   # required — never leave empty; else this file is an isolated graph node (mev W_GRAPH_ISOLATED_NODE)
---

# <Title>

> **Status:** draft — pre-plan holding area.
> **Promote with:** `/plan "<title>"` · `/chore "<title>"` · `/generate-master-plan "<title>"`

## What & Why

<!-- What is this? Why does it matter? This becomes the "Goal" section in a plan. -->

## Context & Background

<!-- Constraints, related decisions, prior work, anything the next reader needs to understand
     the space before reading the instructions. -->

## Key Information / Instructions

<!-- Detailed content captured from the session. MUST include:
     - File paths
     - Class/struct names, functions
     - Important snippets of code
     - Any additional content that makes it EXTREMELY easy for the next agent or user to dig into this note, know exactly what was discussed, how the conclusion or initial research was reached, and where to go look to review/investigate further. -->

## Open Questions

<!-- Things not yet resolved that need answers before this can become a plan.
     Delete this section if there are none. -->

## Rough Scope

<!-- Optional early sizing: what building this likely involves. Not tasks — just a
     directional sense so /plan knows what it's walking into. Delete if not needed. -->
```

## Notes

- Populate the body sections based on the conversation context, but do not invent new content the user or agent hasn't provided/visually seen.
- The notes file is the primary input when you later run `/plan`, `/chore`, or
  `/generate-master-plan` — those commands will read it as context for the feature.
- If the user says the idea is ALSO a content piece, suggest they also run `/add-idea`
  from the brain session.
