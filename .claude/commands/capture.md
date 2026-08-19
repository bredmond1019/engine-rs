# Capture — Scaffold a pre-plan notes file and optionally add a backlog ticket

Captures rich conversation content from a planning or research session. Creates `planning/<slug>/notes.md` in the current repo as a structured holding area. If the `--backlog` flag is provided, it calls the `/backlog-ticket` command to create a backlog item pointing to these notes.

## Variables

$ARGUMENTS — title or free-form description of what to capture, optionally including the `--backlog` flag.
             A slug is derived from this automatically.
             Example: "--backlog Work email setup with instructions"
             Example: "notion-dashboard Notion API read-only dashboard concept"

## Execution Model

**Run entirely inline. Spawn no subagent.** The notes file must be populated from what was
actually discussed in *this* session — a subagent cold-starts with no memory of that conversation
and can only reconstruct it from `git log`, which defeats the point of a capture. The main agent
already holds the context.

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

   **Mark every claim's standing. This is the single most important rule in this command.** A
   captured note is read weeks later, by someone with none of this session's context, and read as
   *fact* unless it says otherwise. Prefix or tag each substantive claim:

   | Tag | Means |
   |---|---|
   | **VERIFIED** | Read in source or observed running, this session. Name the file and symbol |
   | **ASSUMED** | Believed, not checked. Say what would check it |
   | **SAID** | The user or another agent stated it; not independently confirmed |

   An untagged capture is indistinguishable from an assessment, and it will be planned on as if it
   were one. Tagging costs a word and is the difference between a useful note and a confident one.

   **Name symbols, not line numbers.** Line numbers move between the capture and the read; a
   function name can be grepped. Where a line number genuinely helps, keep it *and* the symbol.

   **Pin the moment.** Record today's date and `git rev-parse --short HEAD` for each repo the note
   makes claims about, in the `## Provenance` section. A reader who knows the SHA can tell in one
   command whether the note is still describing the system that exists.
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

> **Status:** draft — pre-plan holding area. **Claims are tagged VERIFIED / ASSUMED / SAID;
> anything untagged is unconfirmed.** Line numbers move — grep the symbol.
> **Promote with:** `/ticket` or `/chore` (one small unit) · `/plan` (one repo, several blocks) ·
> `/assess <topic>` (the shape of the work is still unclear — see Open Questions)

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
     For each, say which of these it is — it decides what the next session does:
       READ   answerable by reading source or docs
       SPIKE  cheaper to settle by writing ~30 lines or running the thing once
       ASK    only the operator can answer (a decision, a preference, a credential)
       ASSESS too many unknowns to answer one at a time — this area needs /assess
     Delete this section if there are none. -->

## Rough Scope

<!-- Optional early sizing: what building this likely involves. Not tasks — just a
     directional sense so the next command knows what it's walking into.

     If you can answer either of these cheaply, do — they are what a later planning session
     would otherwise have to rediscover, and the second one is the expensive miss:
       - What does this CALL that it does not build, and is that thing wired in production
         or does it merely exist in source? Name a call site, or write "unknown".
       - What already exists that this duplicates, and what should be deleted first?
     "Unknown" is a fine answer here and a useful one — it tells the next session where to look.
     Delete this section if not needed. -->

## Provenance

<!-- Captured <DATE>. Repos and SHAs this note makes claims about:
       <repo> @ <short sha>
     A reader who knows the SHA can tell in one command whether this note still describes the
     system that exists. Without it, a six-week-old note is indistinguishable from a current one. -->
```

## Notes

- Populate the body sections based on the conversation context, but do not invent new content the user or agent hasn't provided/visually seen.
- The notes file is the primary input when you later run `/ticket`, `/chore`, `/plan`, or
  `/assess` — those commands read it as context. Where the note goes next depends on what its
  **Open Questions** say: mostly READ/ASK means it is ready to promote; an ASSESS entry, or an
  unanswerable "what does this call that it does not build", means the next step is `/assess`,
  not a plan.
- **A capture is not an assessment and must not be promoted as one.** It records what a session
  saw and thought, at one moment, mostly unverified by design — that is why it is cheap. The
  VERIFIED/ASSUMED/SAID tags exist so a later reader can tell the difference, and the Provenance
  SHA exists so they can tell whether it is still true. Neither costs anything at capture time
  and both are unrecoverable afterwards.
- If the user says the idea is ALSO a content piece, suggest they also run `/add-idea`
  from the brain session.
