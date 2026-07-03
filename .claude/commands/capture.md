# Capture — Scaffold a pre-plan notes file and add a backlog pointer

Captures rich conversation content that is too detailed for a `/backlog-ticket` but not
ready to be a `/plan`. Creates `planning/<slug>/notes.md` in the current repo as a
structured holding area, then appends a pointer ticket to the brain's
`planning/backlog.md` (found via `brain.toml` walk-up).

## Variables

$ARGUMENTS — title or free-form description of what to capture.
             A slug is derived from this automatically.
             Example: "Work email setup with instructions"
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
   - **If no `brain.toml` is found**, this is a standalone repo. Skip the backlog step
     (Step 4) and note that no brain pointer was created. Still create the local notes file.
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

5. Create `planning/<slug>/notes.md` using the Output Format below. Leave the body sections
   as scaffolded prompts — **do not invent content** the user hasn't provided. The user will
   fill in the detail (paste conversation content) after the file exists.
   - **Populate `related:` with ≥1 real `doc_id`** — the project's `master-plan` doc_id, a
     governing decision, or the parent `index`. Never ship `related: []`: a doc_id-bearing file
     with zero outbound edges is an isolated graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`). Use
     genuine doc_ids that exist in the corpus — do not invent one to satisfy the rule.

### Step 4 — Brain backlog pointer (skip if standalone)

6. Append a pointer ticket to `<BRAIN_ROOT>/planning/backlog.md` in the `## Active` section
   (before `## Promoted`) using this format:

   ```
   ### [YYYY-MM-DD] <title>
   `repo:<repo-slug>` `type:<type>` `status:idea`
   **notes:** `<repo-path>/planning/<slug>/notes.md`

   <One sentence: what this capture is and why it matters.>

   ---
   ```

   Use today's date. `<repo-path>` is this repo's path relative to `BRAIN_ROOT`
   (from the `[[repos]]` `repo_path` field). The `**notes:**` line is the link back to
   the detail — the backlog entry stays a one-liner pointer.

### Step 5 — Report

7. Shell out to `mev emit-state --write` to update the brain's focus derivation and state.

8. Confirm: output the local path created and (if applicable) the brain backlog entry.
   Tell the user to open `planning/<slug>/notes.md` and paste their conversation content
   into the relevant sections.

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
> **Backlog entry:** `<BRAIN_ROOT>/planning/backlog.md`

## What & Why

<!-- What is this? Why does it matter? This becomes the "Goal" section in a plan. -->

## Context & Background

<!-- Constraints, related decisions, prior work, anything the next reader needs to understand
     the space before reading the instructions. -->

## Key Information / Instructions

<!-- Paste the detailed content from your conversation here: instructions, research,
     decisions made, links, commands, config snippets, etc. -->

## Open Questions

<!-- Things not yet resolved that need answers before this can become a plan.
     Delete this section if there are none. -->

## Rough Scope

<!-- Optional early sizing: what building this likely involves. Not tasks — just a
     directional sense so /plan knows what it's walking into. Delete if not needed. -->
```

## Notes

- This is a scaffold + pointer only — do not generate content for the body sections.
- The notes file is the primary input when you later run `/plan`, `/chore`, or
  `/generate-master-plan` — those commands will read it as context for the feature.
- If the user says the idea is ALSO a content piece, suggest they also run `/add-idea`
  from the brain session.
