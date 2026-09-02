# Update State — Safely edit a repo's `planning/state.json` per the canonical schema.

## Purpose

`planning/state.json` is the **authoritative work-block dependency graph** for a repo — not a status
report. Almost every planning command (`/generate-master-plan`, `/plan`, `/chore`, `/ticket`,
`/start-block`, `/handoff`, `/wrap-up`, `/backlog-ticket`, a manual fix after `mev validate-brain
--state` flags something) ends up editing part of it. This command is the one place that spells out
**how** to do that safely, so every agent edits the same file the same way instead of re-deriving the
rules from scratch (or worse, guessing).

**The schema itself lives in one place — do not duplicate its field shapes here or anywhere else:**
[`docs/state/state-schema.md`](../../../docs/state/state-schema.md) (path relative to the brain
root; walk up from wherever you are until you find `brain.toml` if you're in a leaf repo, then resolve
`docs/state/state-schema.md` from there). Read it before making any non-trivial edit. This command
is the *workflow*; `state-schema.md` is the *ground truth*.

## When to use this

- You're about to hand-edit any `planning/state.json` (adding/closing a block, appending a
  `carryover[]` entry, promoting a backlog item, fixing a validator warning).
- You're renaming or restructuring block IDs (e.g. adopting the `<Prefix>.<Phase>.<Letter>`
  convention) and need to know what else has to move in lockstep.
- A planning command's instructions say "update `state.json`" without repeating the mechanics —
  come here first.

## The one rule that matters most: Authored vs Derived

**Load the `edit-state-json` skill before any non-trivial edit** — its Step 2 owns the
authored-vs-derived field split, the four `depends_on` edge shapes, the `status` authored-values
trap, and the `scope` exactly-one-of rule (the fleet's single most-repeated `state.json` error).
This command does not restate that content; it covers what the skill doesn't (below).

An `epics[]` entry's `plan` is a path, not a slug — resolve a roadmap's directory via
`/begin-orchestration`'s Step 1C rule (`planning/roadmaps/<slug>/`, else legacy `planning/<slug>/`)
before hand-editing it, rather than assuming either location.

## `kind` — which template applies

Every `state.json` declares a `kind` that must match its `brain.toml` `tier`:

| `kind` | When | Carries | Never carries |
|---|---|---|---|
| `project` | Default leaf repo (`core`/`side`/`client`/`_root` tier) | `tracks[]` (its roadmap) | `repos[]`, `cross_repo[]` |
| `brain` | HQ root or a tier sub-brain | `repos[]`, `cross_repo[]`, HQ-only `backlog[]`/`tiers[]` | Its own `tracks[]` |
| `portfolio` | `brain.toml` `tier == "portfolio"` — published to GitHub, no further planning state | A non-empty `note` (e.g. `"Completed — live on GitHub."`) | `tracks[]`, `focus` entries, a sibling `master-plan.md` |

Getting `kind` wrong is the #1 cause of permanent `mev validate-brain --state` warnings — a `project`
kind with empty `tracks[]` warns forever if the repo is actually a `portfolio`-tier terminal repo (see
`core/mev/planning/decisions/D8-portfolio-kind-terminal-repos.md`).

## Block-ID convention (non-negotiable)

Every block `id` is `<Prefix>.<PhaseNumber>.<BlockLetter>` (e.g. `BA.0.A`), and every task under it is
`<Prefix>.<PhaseNumber>.<BlockLetter>.<TaskNumber>` (e.g. `BA.0.A.3`) — **never** bare prose like
"Block A" or a phase-only id like `1.B`. The `Prefix` is this repo's `prefix` field in the brain root's
`brain.toml` — it must be globally unique across every repo (check `brain.toml` before inventing one;
`/new-project` derives and registers it automatically). This is what makes an id like `BP.1.A.3`
unambiguous no matter which repo's context you're reading it in.

**If you rename an id, you must update every reference in the same pass** — this is the single most
error-prone part of editing `state.json` by hand:
- The block's own `tracks[].blocks[].id`.
- Every `depends_on[]` entry (in this file and any sibling block that names it) whose `id` matches.
- Every `focus.now/next/blocked[].id` that matches (or just regenerate `focus` afterward — simpler).
- The matching heading in `planning/master-plan.md` (`### <id> — <name>`) and any prose reference to
  it there.
- `planning/status.md`'s Progress Table row and Current focus line, if present.
- A brain's cached `repos[]` rollup, if this repo feeds one (regenerate, don't hand-edit — Procedure step 3).

## Procedure

1. **Read `state-schema.md`** for the exact field shape you're about to touch — don't reconstruct it
   from memory or from another file's example; shapes have changed across schema versions (currently
   v2, D36).
2. **Make the authored edit**, following `edit-state-json`'s Step 2 rules (edge shapes, `status`
   values, `origin`/`clears_when`/`scope` shapes). Never invent a block ID prefix — resolve it from
   `brain.toml`; see the rename checklist above if you're renaming one.
3. **Round-trip, validate, and regenerate exactly as `edit-state-json`'s Steps 5–7 describe** — JSON
   well-formedness, `mev validate-brain --state`, `mev emit-state --write`, and the concurrent-lane
   commit-promptly rule. Not restated here.

## Common footguns

See `edit-state-json`'s own "Before you commit" checklist for the general list (scope exactly-one-of,
`origin` shape, `clears_when` already-satisfied trap, etc.). Specific to this command:

- Treating a `portfolio`-kind repo as `project` (or vice versa) — see the `kind` table above.
- Renaming a block id in `master-plan.md` but not in `state.json` (or vice versa), or skipping any
  step of the rename checklist above.

## Report

State what changed (which file, which fields, authored vs regenerated), the `mev validate-brain
--state` result, and whether `emit-state --write` produced any downstream file changes (list them).
