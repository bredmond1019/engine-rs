---
type: Command
title: consolidate-run — Gather orchestration-run records across the fleet and propose carryover entries
description: Discovers per-(repo x roadmap) orchestration-run records for one roadmap, cross-checks them against lane-log.jsonl, selects on D57's two-axis origin_roadmap rule, and proposes carryover[] entries with finding_id for mev's cross-repo correlation. Writes no state.json.
---
# Consolidate Run — gather findings across the fleet for one roadmap

Lane runs leave findings in their own repos (`planning/orchestration-run/<roadmap-slug>/notes.md` and
`review.md`, per [D57](../../planning/decisions/D57-orchestration-run-artifact-contract.md)) and
nothing gathers them. A defect observed once in four separate repos is four notes nobody correlates.
This command implements **D57 section 5** — it gathers, correlates by `finding_id`, and proposes; it
does not decide, and it does not write state.

**Related:** the record contract itself (layout, frontmatter, `origin_roadmap`, `lifecycle`) is
`BT.ticket.orchestration-run-record-contract` — this command depends on it and does not restate it.
`/generate-roadmap --from <consolidated>` is the disposal path for what this command proposes.

## Variables

`$ARGUMENTS` — a roadmap slug, plus optional flags.

```
Usage: /consolidate-run <roadmap-slug> [--repo <slug>]
```

| Flag | Required | Default | What it does |
|---|---|---|---|
| `<roadmap-slug>` | **yes** | — | The roadmap whose records to consolidate. Matches the `roadmap:` frontmatter field on run records and resolves to `<roadmap-dir>` under `planning/roadmaps/` (or legacy `planning/`) per Step 1's resolution rule. |
| `--repo <slug>` | no | none (fleet-wide) | Scope discovery to one repo instead of the whole fleet. |

**Both scopes required by D57 section 5:**

- **Run at the brain root with no `--repo`** — consolidates the whole roadmap across every repo in
  the fleet. This is the normal case: the point of consolidation is cross-repo correlation.
- **Run inside one repo (or at the brain root with `--repo <slug>`)** — consolidates that repo's
  records for the roadmap alone. Useful when a single lane needs its own findings reviewed before the
  fleet-wide run, or when only one repo's records exist yet.

Empty `$ARGUMENTS` → print usage and stop.

---

## Step 1 — Resolve scope

Walk up from cwd for `brain.toml` to find `BRAIN_ROOT`. Resolve `<roadmap-slug>` to `roadmap_dir` via
`/begin-orchestration`'s Step 1C rule (`planning/roadmaps/<roadmap-slug>/` first, then legacy
`planning/<roadmap-slug>/`; both existing is an error) — cited here, not restated. If neither
location exists, stop — there is nothing to consolidate into.

If `--repo <slug>` is given, or the command is invoked from inside a sub-repo (cwd is not
`BRAIN_ROOT`), scope every step below to that one repo. Otherwise scope to the whole fleet.

## Step 2 — Participants: `lane-log.jsonl`, cross-checked against the filesystem

**Participants come from `<roadmap-dir>/lane-log.jsonl`** (append-only, one line per integrated
block, each line carrying its `repo` — see `.claude/commands/generate-roadmap.md` and
`begin-orchestration.md`'s `--log` variable, default `<roadmap-dir>/lane-log.jsonl`). Read every
line; the set of distinct `repo` values is the participant list.

**The filesystem sweep (Step 3) is a cross-check, never a silent union.** Compare the participant
list against the set of repos actually holding a discovered run record for this roadmap, in both
directions:

- **A repo wrote a run record but logged no block in `lane-log.jsonl`.** Report it — the lane ran
  outside `/begin-orchestration`'s bookkeeping, or the log write was missed. Do not silently add it
  to the participant list.
- **A repo logged an integrated block but wrote no run record.** Report it — the record write was
  missed, or the lane never reached the notes step. Do not silently drop it from the participant
  list.

Both mismatches go into the consolidated output as findings about the *process*, distinct from the
findings the run records themselves contain.

## Step 3 — Discovery: realpath dedup, `-L` and `-uu`

Sweep `**/orchestration-run/<roadmap-slug>/*.md` from `BRAIN_ROOT` (or from the single repo root when
`--repo` scopes the run). The sweep must pass **both**:

- **`-L`** (follow symlinks) — every repo's `planning/` is a symlink into the vault, including
  inside worktrees. Without `-L` the sweep silently returns nothing for every repo.
- **`-uu`** (search hidden/gitignored paths) — at the brain root, every sub-repo is gitignored from
  the brain's own perspective. `-L` alone silently misses whole repos.

**Dedup by `realpath`, before reading.** The same physical file is reachable through more than one
symlink chain: a repo's own `planning/` symlink, and — inside a worktree — a *second* symlink chain
that canonicalizes onto the same vault file, e.g.
`core/engine-rs/trees/sdlc/mev/planning/orchestration-run/notes.md` canonicalizes onto
`core/mev/planning/orchestration-run/notes.md`. Deduping by path string instead of `realpath` **triple-
counts the same finding and, worse, can read a stale worktree copy** instead of the vault original —
a worktree branch that has since been abandoned or superseded still shows up as if it were live. Take
the realpath as the retained path in every case; discard the worktree-routed alias.

## Step 4 — Selection: D57 section 3's two-axis rule

Select records for consolidation on **both** axes, per
[D57 section 3](../../planning/decisions/D57-orchestration-run-artifact-contract.md#3--per-block-origin_roadmap)
(cited, not restated):

1. Records whose own `roadmap:` frontmatter equals `<roadmap-slug>` **and** `lifecycle != consolidated`.
2. Items in *any other* record whose ledger row or finding carries `origin_roadmap: <roadmap-slug>` —
   a block adopted into a different roadmap's lane still attributes its findings back to the roadmap
   that owns it.

A record can therefore contribute to a consolidation run even though its own `roadmap:` field points
elsewhere. Skip nothing on axis 2 solely because axis 1 already selected the record's home roadmap —
the two axes are additive, not a fallback pair.

No dedup, similarity matching, priority ranking, or staleness logic belongs in this half of the
command. `mev` already builds cross-repo dedup and ranking; this command consumes it rather than
shipping a second implementation.

## Step 5 — Propose: `finding_id`, the new `carryover[]` shape, and `mev`'s ranking

Emit `<roadmap-dir>/consolidated-review.md`. Shape it as a flat list of one item per proposed
finding — one row per item — so `/generate-roadmap --from <consolidated-review.md>`'s coverage
crosswalk (required whenever `--from` includes an action register, per
`.claude/commands/generate-roadmap.md`) can map each row to where it lands without re-deriving
structure this command already produced. `--from` already accepts an arbitrary source document
(`generate-roadmap.md`); a `consolidated-review.md` is exactly such a source.

**Every proposed entry carries a `finding_id`.** It is a free-form shared string, no registry,
many-to-one by construction — set it identically on every record contributing the same underlying
claim. This is the command's actual contribution: it is what lets `mev`'s `cluster_by_finding_id`
correlate the same claim across repos. Without it there is nothing for `mev` to group on.

**Proposed entries match the NEW `carryover[]` shape** (per D57 Correction 1 — cited, not restated):

- **`priority`** — `0..3`, lower hotter, default P2 when absent. **Cite
  [D43](../../../docs/decisions/D43-cross-domain-priority-graph.md) for the rubric; never paraphrase
  it.** Restating D43's rubric in a command file is exactly how `close-the-loop/orchestrate-prompt.md`
  came to assert the contradictory "P0 = blocks other work" — a divergence nothing caught because
  nothing linked that file back to D43. That competing reading is not a rival scale: D43 names it
  *cross-DAG priority inheritance*, and it ships as `MV.7.A` (`mev/src/brain/emit.rs:607`) —
  **derived, never authored.** If a proposal needs the rubric applied, link to D43 rather than
  quoting its buckets.
- **`blocks[]`** — the edge array. There is no `blocking: bool` field anywhere in a proposed entry — a
  hand-maintained boolean nothing validates is the defect class D57 exists to remove.
- **`clears_when`** — a **typed predicate**, e.g. `{type: block_closed, repo: base-template, id:
  <block>}`, never free text. Free text may surround an item as context; it may never stand in for
  the predicate itself.
- **`finding_id`** — as above.

**Scope boundary — the point of this command.** It implements **no** dedup, similarity matching,
priority ranking, or staleness logic of its own. `mev` is already building all three:
`MV.ticket.carryover-dedup-clusters` (groups by `finding_id`, suggests links for ungrouped entries,
reports per-repo priority divergence) and `MV.ticket.carryover-triage-ranking` (the public ranking
API, re-cutting lanes to BLOCKING / HOT / AGING / STANDING). This command calls `mev carryover` and
consumes its `clusters` / `suggestions` / `single_repo_finding_ids` sections and its ranking output —
it never recomputes them. This follows the fleet's standing discipline, **"mev owns the derivation;
bastion projects it,"** with the precedent enforced in a doc comment at
`core/bastion/src/serve/handlers/attention.rs:140-141`: *"this function never reimplements that
predicate."* This command is a third projector, not a second deriver.

**Never auto-merges.** The matcher `mev` runs only *suggests* links between findings; this command
never merges two proposed entries into one on its own authority. A false merge destroys durable
knowledge the same way a false `cleared` does.

**Per-repo priority divergence is preserved.** The same claim can be genuinely P0 in one repo (it
blocks revenue there now) and P2 in another (it is routine there). Dedup merges the *claim* — via
shared `finding_id` — and keeps each repo's own `priority` value; it never collapses them to one
number.

## Step 6 — Write boundary: no `state.json`, anywhere

This command **writes no `state.json` in any repo.** One command writing state across nine repos is
the exact contention pattern that has already cost real runs in this fleet (CLAUDE.md standing rule
10) — write authority stays narrow by design (D57 OD-4). It proposes; `/generate-roadmap --from`
disposes, reusing the existing authoring path instead of creating a second one.

The **only** write this command makes is stamping `lifecycle: consolidated` on the run records it
consumed, so a re-run does not re-propose the same findings. It does not touch `carryover[]`,
`tracks[]`, or any other `state.json` field in any repo — those writes belong to whatever consumes
`consolidated-review.md`.
