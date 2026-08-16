# Generate Master Plan — superseded by `/plan --founding` (D65)

## This command no longer authors anything

`master-plan.md` is **generated** from the block graph (`planning/blocks/*.json` +
`planning/state.json`), not authored by hand. Writing one by hand is how it became sediment: 15
live copies across the fleet, three over 2400 lines, edited mostly by bookkeeping rather than by
anyone planning work.

**Use instead:**

| You want to | Run |
|---|---|
| Plan a new project's founding roadmap | `/plan --founding` |
| Plan an initiative in one repo (any number of blocks) | `/plan` |
| Plan work spanning several repos | `/generate-roadmap` |
| Plan a single behavior change | `/ticket` |
| Plan a single maintenance task | `/chore` |

## Why this changed

Governed by [D65](../../planning/decisions/D65-block-record-is-the-planning-unit.md).

The block record at `planning/blocks/<BlockID>.json` is now the authored definition of a block —
its what, why, files, out-of-scope, interfaces, and acceptance criteria. `/generate-tasks` reads
that record. The markdown roadmap is a *view* over the block graph, and a view can be regenerated
rather than maintained.

What the narrative layer still holds — the goal, the destination, the architecture framing, the
sequencing rationale, the cut list — is genuinely about the *set* rather than any one block, and
lives in an initiative's `planning/<slug>/plan.md`, authored by `/plan`.

`/plan --founding` is the same code path as `/plan`, with the founding framing sections and a
`planning/founding/` destination. There is no separate founding format.

## If you are looking for the old behavior

- **The block skeleton** (What / Why / SDLC workflow / Model / Files / Interfaces / Out of scope /
  Depends on / Acceptance criteria) is now the block record's field contract:
  `.claude/workflows/block.schema.json`.
- **The registration steps** (block ID, `state.json` creation and registration, the cross-repo
  edge prompt, `mev emit-state --write`) are `.claude/workflows/block-registration.md` — one copy,
  shared by every producer. Four drifted copies existed before D65; only one of them created a
  missing `state.json`, which is how at least one repo's block graph went dark.
- **The wave-table sentinel** is unchanged: `mev emit-state --write` fills
  `<!-- BEGIN generated:wave-table -->`. A file missing the sentinel pair is silently skipped
  (`W_EMIT_NO_SENTINEL`).

## Migration status

The generated `master-plan.md` render is **not yet implemented in `mev`**. Until it ships, the 15
existing `master-plan.md` files stay where they are and are **frozen** — do not add new blocks to
them by hand. New work goes through `/plan`, whose block records are what the generator will read.
