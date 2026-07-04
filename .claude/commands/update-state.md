# Update State — Safely edit a repo's `planning/state.json` per the canonical schema.

## Purpose

`planning/state.json` is the **authoritative work-block dependency graph** for a repo — not a status
report. Almost every planning command (`/generate-master-plan`, `/plan`, `/chore`, `/ticket`,
`/start-block`, `/handoff`, `/wrap-up`, `/backlog-ticket`, a manual fix after `mev validate-brain
--state` flags something) ends up editing part of it. This command is the one place that spells out
**how** to do that safely, so every agent edits the same file the same way instead of re-deriving the
rules from scratch (or worse, guessing).

**The schema itself lives in one place — do not duplicate its field shapes here or anywhere else:**
[`core/planning/state-schema.md`](../../../core/planning/state-schema.md) (path relative to the brain
root; walk up from wherever you are until you find `brain.toml` if you're in a leaf repo, then resolve
`core/planning/state-schema.md` from there). Read it before making any non-trivial edit. This command
is the *workflow*; `state-schema.md` is the *ground truth*.

## When to use this

- You're about to hand-edit any `planning/state.json` (adding/closing a block, appending a
  `carryover[]` entry, promoting a backlog item, fixing a validator warning).
- You're renaming or restructuring block IDs (e.g. adopting the `<Prefix>.<Phase>.<Letter>`
  convention) and need to know what else has to move in lockstep.
- A planning command's instructions say "update `state.json`" without repeating the mechanics —
  come here first.

## The one rule that matters most: Authored vs Derived

`state-schema.md`'s "Authored vs derived" table is the load-bearing distinction. Before touching any
field, know which bucket it's in:

| Bucket | Fields | You may hand-edit these |
|---|---|---|
| **Authored** (source of truth) | `tracks[].blocks[]` (`id`, `title`, `status`, `depends_on`, `wave`, `note?`, `origin?`, `tasks?`), `backlog[]` (HQ only), `carryover[]`, HQ `tiers[]`, `note` (portfolio kind) | **Yes** |
| **Derived** (a regenerated cache) | `focus`, brain `repos[]`, brain `cross_repo[]`, master-plan wave tables | **No — never hand-edit.** Run `mev emit-state --write` instead |

If you find yourself about to type a value into `focus.now[]`, `repos[]`, or `cross_repo[]` directly,
stop — edit the authored `tracks[].blocks[]` (or the child repo's own state.json) instead, then
regenerate (see Step 4 below).

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
- A brain's cached `repos[]` rollup, if this repo feeds one (regenerate, don't hand-edit — Step 4).

## Procedure

1. **Read `state-schema.md`** for the exact field shape you're about to touch (`tracks[].blocks[]`
   shape, `carryover[]` shape, `backlog[]` shape, `depends_on` entry forms) — don't reconstruct it
   from memory or from another file's example; shapes have changed across schema versions (currently
   v2, D36).
2. **Make the authored edit only.** Never hand-set `status: "blocked"` — it's derived from unmet
   `depends_on`, not an authored value (`open` / `in_progress` / `closed` are the only authored
   statuses). Never invent a block ID prefix — resolve it from `brain.toml`.
3. **Validate the JSON is well-formed** before doing anything else:
   `python3 -c "import json;json.load(open('planning/state.json'))"`.
4. **Regenerate derived views** — run `mev emit-state --write` (from anywhere under the brain root; it
   walks up to find `brain.toml`). This recomputes `focus`, brain `repos[]`/`cross_repo[]`, and any
   `master-plan.md` wave tables with `wave-table` sentinels. Never hand-patch these fields to "match" —
   let the derivation do it, then diff the result to sanity-check it.
5. **Run `mev validate-brain --state`** and confirm no new `E_STATE_*` errors and no unexpected new
   `W_STATE_*` warnings. A `W_STATE_ROLLUP_DRIFT` after step 4 means step 4 didn't actually run against
   the file you just edited (check you're pointing `emit-state` at the right root).
6. **Check for concurrent in-flight work before committing.** If this repo has an active `/sdlc-flow`
   or `/sdlc-block` running (look for `planning/<slug>/trees/*` worktrees, or a fresh unexpected commit
   on `main` since you started), your uncommitted edit can be silently discarded by that process's own
   git operations. Commit your `state.json` edit **promptly** once it's correct, or coordinate with
   whatever is running before editing further.

## Common footguns (from real incidents)

- Editing `focus`/`repos[]`/`cross_repo[]` by hand instead of running `emit-state --write` — the next
  regeneration will silently overwrite your manual fix, and in the meantime nothing else agrees with
  it.
- Renaming a block id in `master-plan.md` but not in `state.json` (or vice versa) — `mev
  validate-brain --state` will flag the resulting `W_STATE_ROLLUP_DRIFT` / dangling reference, but only
  *after* something reads the mismatch.
- Treating a `portfolio`-kind repo as `project` (or vice versa) — see the `kind` table above.
- Leaving an edit uncommitted in a repo with a concurrent SDLC pipeline running — see Step 6.
- Hand-authoring `status: "blocked"` — always derived, never authored (`state-schema.md`'s
  "Derivation rules" section).

## Report

State what changed (which file, which fields, authored vs regenerated), the `mev validate-brain
--state` result, and whether `emit-state --write` produced any downstream file changes (list them).
