# Chore — Plan a maintenance or housekeeping task.

## Variables

$ARGUMENTS — description of the chore to plan.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the chore.
2. Research the codebase: read `CLAUDE.md`, then any files directly relevant to the chore.
3. Create a plan using the Plan Format below.
4. Choose a short descriptive slug for the chore (e.g. `remove-k8s-secret`, `fix-devin-typos`, `update-stale-handles`).
   Determine this chore's Block ID: find this repo's `prefix` in `brain.toml` at the brain root
   (e.g. `BA`), then `<BlockID> = <Prefix>.chore.<slug>` (chores don't have a phase number — the
   literal string `chore` fills that slot). This is the `<BlockID>` referenced in "Register the
   block in state.json" below.
5. Create the directory `planning/chore-{descriptive-name}/` if it does not exist, then save **both**
   `planning/chore-{descriptive-name}/tasks.md` (prose) and `tasks.json` (task list) using the Plan
   Format below. In the `tasks.md` frontmatter, **populate `related:` with ≥1 real `doc_id`** (the
   project `master-plan`, a governing decision, or a doc the chore touches) — never `related: []`, or
   the file is an isolated graph node (`mev`'s `W_GRAPH_ISOLATED_NODE`). Use genuine doc_ids only.
6. Return only the paths to the files created.

## Codebase Structure

- `CLAUDE.md` — standing rules, the SDLC pipeline, build/test/validate commands (start here)
- `planning/context.md` — why the project exists + audit findings; `planning/status.md` — progress
- `planning/harness.json` — the project's validation commands + UI-test config
- `planning/` — task specs and plan files (one concept folder per task)

Read `CLAUDE.md` for the project's actual stack, directory layout, and conventions — do not assume
any framework, language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` — internalize and enforce **the project's standing rules**.
CLAUDE.md is the authority; do not assume any stack, locale-parity, narrative, or content-layout rule
unless written there. Universal harness rules still apply: no fabricated metrics/quotes, no emoji,
every change ships with tests.

## Plan Format

`tasks.md` opens with OKF frontmatter (required on every new `.md` under `planning/`), then the prose:

```md
---
type: Plan
title: Chore — <chore name>
description: <one-line summary of the chore>
doc_id: chore-<slug>
layer: [<inferred layer>]
project: <repo slug>
status: active
keywords: [<3-5 terms>]
related: [<≥1 real doc_id>]   # required — never leave empty; else this file is an isolated graph node (mev W_GRAPH_ISOLATED_NODE)
---

# Chore: <chore name>

## Metadata
prompt: `{$ARGUMENTS}`

## Chore Description
<describe the chore in detail — what it is, why it matters, any known constraints>

## Relevant Files
<list files relevant to the chore with bullet points explaining why each is needed>

### New Files
<list any new files that will be created, if applicable>

## Step by Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Validation Commands
```
<the project's validation commands — see `planning/harness.json` (`validation.checks[]`) or CLAUDE.md; one command per line, in order>
```
<add any chore-specific checks above the standard project checks>

## Notes
<optional context, edge cases, or gotchas>
```

`planning/chore-{descriptive-name}/tasks.json` — a **bare array**, matching orchestrator's
`SDLCTask` schema (`core/orchestrator/app/schemas/sdlc_schema.py`) plus two additive fields
base-template's own tooling uses:
```json
[
  { "task_id": 1, "title": "<First Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [] },
  { "task_id": 2, "title": "<Second Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [1] },
  { "task_id": "N", "title": "Validate", "description": "Run the Validation Commands listed below and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
]
```

### Step X — Register the block in state.json
After writing `tasks.md` + `tasks.json`, also register this chore's block in `planning/state.json`
— a chore is a standalone block, not one already sitting in `master-plan.md`.
1. Open `planning/state.json`. Find or create a `tracks[]` entry titled `"Chores"` (reuse it if it
   already exists).
2. Add an entry to that track's `blocks[]` for this chore's `<BlockID>`, if it doesn't already exist:
   - `id`: the chore's Block ID
   - `title`: the chore name
   - `status`: `"open"`
   - `wave`: default to one past this repo's current highest wave (chores queue behind roadmap work
     unless the user says it's urgent — ask before assigning an earlier wave)
   - `depends_on`: `[]` unless the chore explicitly names a prerequisite block, in which case
     `{ "type": "block", "repo": "<this-repo-slug>", "id": "<ID>" }`
   - **Cross-repo-edge prompt.** Before defaulting to `[]` or a same-repo edge, ask explicitly: "Does
     this chore depend on work landing in another repo first?" If yes, resolve that repo's `slug` from
     `brain.toml` and add `{ "type": "block", "repo": "<other-repo-slug>", "id": "<their-ID>" }`; if the
     dependency is non-block (hardware, a paid-API budget, a manual step), use
     `{ "type": "external", "what": "<gloss>" }` instead.
3. Do **not** hand-author a `tasks` array on that block — `tracks[].blocks[].tasks` is a *derived*
   pointer + status summary (`{ file, generated, counts }`, see `core/planning/state-schema.md`),
   not a copy of the task list. `mev emit-state --write` (Step below) derives it from the
   `tasks.json` you just wrote. (Not implemented in `mev` yet — the step is a no-op until it ships.)
4. Save `planning/state.json` and validate it is still valid JSON:
   `python3 -c "import json;json.load(open('planning/state.json'))"`.

### State Refresh

Run `mev emit-state --write` to update the brain's focus derivation and state based on the new planning files.

## Report

Output the paths to the files created and the next step:
```
planning/chore-{name}/tasks.md + tasks.json

Next (implement + test loop):
  /sdlc-task chore-{name}
```
