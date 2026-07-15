# Ticket — Plan a small behavior-change with observable Acceptance Criteria.

## Variables

$ARGUMENTS — description of the bug fix, enhancement, or small behavior-change to implement.

## Purpose

Plan one small, well-scoped behavior-change — a bug fix or targeted enhancement that requires
new or modified tests. The output is a single-block `tasks.md` (prose) + `tasks.json` (the task
list), explicit Acceptance Criteria, and a Testing Strategy, feeding directly into lean
`/sdlc-task`.

> **Distinct from `/chore`:** chores are maintenance (no behavior change, tests incidental).
> Tickets are behavior-changing (tests required, AC is non-negotiable).
> For multi-block work, use `/plan` → `/sdlc-block` instead.

## Instructions

1. If `$ARGUMENTS` is not provided, stop and ask the user to describe the bug or change.
2. **Plan-quality floor — clarify, don't fabricate.** If filling a load-bearing element (which
   files to change, what the observable correct behavior is, or an Acceptance Criterion) would
   require *inventing* a fact you cannot ground in `$ARGUMENTS`, `CLAUDE.md`, `planning/context.md`,
   or the repo — **stop and ask the user a targeted question** rather than write a plausible-looking
   guess. An honest "I need X to write the AC" beats a confident invention.
3. Research the codebase: read `CLAUDE.md`, `planning/context.md`, then the files directly
   relevant to the change.
4. THINK HARD about scope before writing:
   - A ticket is a **single coherent unit** — one logical change, one set of tests.
   - **SDLC workflow?** — `Yes (patch/task/run/flow)` or `No — <reason>`.
   - **Model** — `Sonnet` | `Gemini Pro` | `Gemini Flash` | `Either`. Rule of thumb: Opus = reasoning/breakdown only; Sonnet = high-risk/complex; Gemini Pro = intermediate; Gemini Flash = simple.
   - **Workflow & Model Rationale** — prose explaining the choices.
   - If the fix touches more than 3–4 files or needs its own sub-phases, it belongs in `/plan`.
   - Every task in `tasks.json` must name ≥1 concrete file in its `files[]` (the Validate task is
     exempt).
5. Choose a short descriptive slug (e.g. `fix-null-deref`, `add-rate-limit`, `patch-auth-refresh`).
   This is the `<slug>` referenced in "Register the block in state.json" below.
6. Create `planning/ticket-{slug}/` if it does not exist, then write **both**
   `planning/ticket-{slug}/tasks.md` (prose) and `planning/ticket-{slug}/tasks.json` (task list)
   using the Plan Format below.
7. Register this ticket's block in `planning/state.json` — see "Register the block in state.json"
   below.
8. **Property self-check.** Before reporting, re-read the spec and **revise in place** until every
   property holds, then re-check:
   - **`tasks.json` parses as valid JSON** and is a non-empty bare array (not wrapped in an object).
   - **Every task names ≥1 concrete file** in its `files[]` (Validate is exempt).
   - **Acceptance Criteria are non-empty and observable** — each can be judged true/false.
   - **Testing Strategy is non-empty** — names the test file(s) and what each must cover.
   - **Validation Commands are present** (or `planning/harness.json` → `validation.checks[]`
     supplies them as the fallback).
   - **No leftover template sentinels** — no `{{TOKEN}}`, unfilled `<placeholder>`-style angle
     stubs, or empty bullets. Legitimate `<...>` in code/prose is fine.
9. Report the path and next step.

## Codebase Structure

- `CLAUDE.md` — standing rules, stack, build/test/validate commands (start here)
- `planning/context.md` — why the project exists; `planning/status.md` — current state
- `planning/harness.json` — validation commands + UI-test config
- `planning/` — task specs (one concept folder per task)

Read `CLAUDE.md` for the project's actual stack and conventions — do not assume any framework,
language, or directory structure that isn't written there.

## Standing rules to respect

Read `CLAUDE.md` and `planning/context.md` and enforce **the project's standing rules**. CLAUDE.md
is the authority. Universal harness rules apply: no fabricated metrics/quotes, no emoji, every
ticket ships with tests.

## Plan Format

```md
# Ticket: <change name>

## Metadata
prompt: `{$ARGUMENTS}`
status: Not started
last-run: never
sdlc_workflow: <none | patch | task | run | flow>
model: <sonnet | gemini-pro | gemini-flash | either>
rationale: <prose explaining the model and workflow choices>

## Description
<what is broken or missing and what the correct behavior should be; one concise paragraph>

## Relevant Files
<files to change, with a one-line note on why each is needed>

### New Files
<new files to create, if any — test files go here when they don't exist yet>

## Step by Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Testing Strategy
<which test file(s) cover this change; what behavior each test must assert; any edge cases>

## Acceptance Criteria
<list specific, observable conditions that must be true for this ticket to be done>

## Validation Commands
<the project's validation commands — see `planning/harness.json` or CLAUDE.md; one per line>

## Notes
<optional: constraints, follow-ups, known edge cases not covered by this ticket>

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the plan. -->
_No amendments yet._
```

`planning/ticket-{slug}/tasks.json` — a **bare array**, matching orchestrator's `SDLCTask` schema
(`core/orchestrator/app/schemas/sdlc_schema.py`) plus two additive fields base-template's own
tooling uses:
```json
[
  { "task_id": 1, "title": "<First Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<file this task touches>"], "dependsOn": [] },
  { "task_id": 2, "title": "<Second Task Name>", "description": "<specific action>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<file this task touches>"], "dependsOn": [1] },
  { "task_id": "N", "title": "Validate", "description": "Run the Validation Commands listed below and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2] }
]
```

### Register the block in state.json

If this repo has a `planning/state.json`, also register this ticket's block — a ticket is a
standalone block, not one already sitting in `master-plan.md`.
1. Determine this ticket's Block ID: find this repo's `prefix` in `brain.toml` at the brain root
   (e.g. `BA`), then `<BlockID> = <Prefix>.ticket.<slug>` (tickets don't have a phase number — the
   literal string `ticket` fills that slot, mirroring `/chore`'s convention).
2. Open `planning/state.json`. Find or create a `tracks[]` entry titled `"Tickets"` (reuse it if it
   already exists).
3. Add an entry to that track's `blocks[]` for this ticket's `<BlockID>`, if it doesn't already exist:
   - `id`: the ticket's Block ID
   - `title`: the ticket name
   - `status`: `"open"`
   - `sdlc_workflow`: the block's chosen workflow (`none` | `patch` | `task` | `run` | `flow`)
   - `model`: the block's chosen model (`sonnet` | `gemini-pro` | `gemini-flash` | `either`)
   - `wave`: default to one past this repo's current highest wave (tickets queue behind roadmap work
     unless the user says it's urgent — ask before assigning an earlier wave)
   - `depends_on`: `[]` unless the ticket explicitly names a prerequisite block, in which case
     `{ "type": "block", "repo": "<this-repo-slug>", "id": "<ID>" }`
   - **Cross-repo-edge prompt.** Before defaulting to `[]` or a same-repo edge, ask explicitly: "Does
     this ticket depend on work landing in another repo first?" If yes, resolve that repo's `slug`
     from `brain.toml` and add `{ "type": "block", "repo": "<other-repo-slug>", "id": "<their-ID>" }`;
     if the dependency is non-block (hardware, a paid-API budget, a manual step), use
     `{ "type": "external", "what": "<gloss>" }` instead.
4. Do **not** hand-author a `tasks` array on that block — `tracks[].blocks[].tasks` is a *derived*
   pointer + status summary (`{ file, generated, counts }`, see `docs/state/state-schema.md`),
   not a copy of the task list. `mev emit-state --write` (next step) derives it from the `tasks.json`
   you just wrote. (Not implemented in `mev` yet — the step is a no-op until it ships.)
5. Save `planning/state.json` and validate it is still valid JSON:
   `python3 -c "import json;json.load(open('planning/state.json'))"`.

### State refresh (do not hand-author `state.json`'s `tasks` field)

If this repo has a `planning/state.json`, run `mev emit-state --write` after committing — it derives
`tracks[].blocks[].tasks` (a `{ file, generated, counts }` pointer + status summary, **not** a copy
of the task list — see `docs/state/state-schema.md`) from the `tasks.json` you just wrote. Do not
hand-edit a `tasks` array into `state.json` yourself; that field is derived, same as `focus`. (This
derivation isn't implemented in `mev` yet — running the command is a no-op until it ships; it's
listed here so the step is already in place when it does.)

## Report

Output the path and next step:
```
planning/ticket-{slug}/tasks.md + tasks.json

Next (implement + test loop):
  /sdlc-task ticket-{slug}
```
