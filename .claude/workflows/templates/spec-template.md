---
type: Spec
title: <Concept Name> — Task Spec
description: One-line summary of what this spec delivers.
---

# <Concept Name>

> Format reference for SDLC task specs. A real spec lives at `planning/<concept>/tasks.md` +
> a companion `planning/<concept>/tasks.json`. Replace every `<...>` placeholder; keep the
> section headings — the pipeline reads them.

**Status:** Not started · **Last run:** _never_

## Goal

<What this spec delivers, stated as an outcome. One short paragraph. What does "done" look
like?>

## Context Pointers

- `planning/context.md` — project orientation and standing rules
- `planning/master-plan.md` — where this concept sits in the sequence
- <path/to/relevant/source — the code this spec touches>
- <link to any prior decision in planning/decisions/ that constrains this work>

## Step-by-Step Tasks

The task list is **not** written here — it lives in the companion `tasks.json` (same directory,
same basename). This file just points at it; the pipeline reads `tasks.json` directly, never a
markdown heading. The shape matches orchestrator's `SDLCTask` schema
(`core/orchestrator/app/schemas/sdlc_schema.py`) exactly, so a spec either pipeline can consume —
**a bare array, not wrapped in an object.** See `planning/<concept>/tasks.json`:

```json
[
  { "task_id": 1, "title": "<Foundational step>", "description": "<what to build and where>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [] },
  { "task_id": 2, "title": "<Next step>", "description": "<what to build and where>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [1] },
  { "task_id": 3, "title": "<Next step>", "description": "<what to build and where>", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": ["<path/to/file>"], "dependsOn": [1] },
  { "task_id": 4, "title": "Validate", "description": "Run the Validation Commands listed below and confirm all pass.", "acceptance_criteria": [], "validation_commands": [], "max_attempts": 3, "files": [], "dependsOn": [1, 2, 3] }
]
```

`task_id` — 1-indexed, dependency-ordered, no gaps. `title` — short, matches what a heading used to
say. `description` — the "what to build and where" prose (bulleted lines in one string are fine).
`acceptance_criteria` / `validation_commands` — usually left `[]`; the spec-level `## Acceptance
Criteria` / `## Validation Commands` sections below stay authoritative, these per-task arrays exist
because orchestrator's schema has them, not because base-template's engines populate them.
`max_attempts` — defaults to 3; only set it to override for one task. `files` and `dependsOn` are
**not** part of orchestrator's schema — orchestrator ignores them (Pydantic default), base-template
uses them for the disjoint-file-ownership self-check and dependency ordering. `files`: every task
except the final Validate task must name ≥1 concrete file it creates or modifies. `dependsOn`: ids
this task requires to run first; the final Validate task depends on every other id.

## Acceptance Criteria

- <Observable, checkable condition — not "works well" but "endpoint returns 200 for X">
- <Each criterion maps to something a reviewer can verify against the diff>
- All tasks ship with tests covering their core functionality.

## Validation Commands

Optional. If `planning/harness.json` exists, the pipeline runs its `validation.checks[]` and
this section is a human reference. If `harness.json` is absent, the pipeline runs the commands
listed here instead.

```bash
# <format check>
# <lint check>
# <test suite — authoritative for the verdict>
# <build>
```

## Notes

<Anything that doesn't fit above: known risks, out-of-scope items, follow-ups, links.>

## Amendment Log

Append-only. When a pipeline stage deviates from this spec (a fix, a scope adjustment, a
substitution), it records one dated line here so the spec stays a living record of how it actually
ran. Do not rewrite history — only append.

_No amendments yet._
