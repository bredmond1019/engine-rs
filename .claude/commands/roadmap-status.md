---
type: Command
title: roadmap-status — One mid-run view of a roadmap's lanes
description: Read-only projection of one roadmap's live state across every repo — what needs the operator, what is running or recently finished, what stopped and why — joined from artifacts already on disk.
---
# Roadmap Status — one mid-run view of a roadmap's lanes

A roadmap runs as several concurrent lanes, one per repo, each in its own agent session. Asking
"where is this roadmap" today means reading every one of those sessions. The information is
already on disk, scattered across four artifact families that nothing joins. This command joins
them, for one roadmap, and reports.

**This command derives nothing.** Ranking, dedup and staleness scoring belong to `mev`
(`rank_carryover`, `cluster_by_finding_id`, effective-priority propagation); this command projects
what is already on disk, exactly as `/consolidate-run` does for post-run findings — per the fleet
discipline "mev owns the derivation; bastion projects it"
(`core/bastion/src/serve/handlers/attention.rs:140-141`). It implements no ranking, dedup, or
staleness logic of its own.

**This command writes nothing — anywhere.** No `state.json`, no record file, no lock, no cache.
Several lanes may be running concurrently against the same roadmap while this runs; a status tool
that mutates shared state during a concurrent run is the contention pattern this fleet has
repeatedly been bitten by (CLAUDE.md standing rule 10).

## Not these

- **Not `/attention`** — that triages stale items fleet-wide against `brain.toml` thresholds. This
  command is scoped to one roadmap's live lanes, not staleness triage.
- **Not `/next`** — that recommends what to work on next. This command reports what already
  happened and is happening; it makes no recommendation.
- **Not `/consolidate-run`** — that correlates findings across finished lane runs into proposed
  `carryover[]` entries after the fact. This command is a live, mid-run read; it proposes nothing
  and writes nothing.

## Variables

`$ARGUMENTS`:

```
Usage: /roadmap-status --roadmap <slug>
```

| Flag | Required | What it does |
|---|---|---|
| `--roadmap <slug>` | no | The roadmap to report on. Omit to list candidate roadmaps instead. |

**`--roadmap` omitted** → list the candidate roadmaps (from
`scripts/roadmap_status_discovery.py` with no `--roadmap`) with their last activity, and stop.
**Never guess** which roadmap was meant — a status read against the wrong roadmap is worse than no
read.

## Step 1 — Run discovery

Runs from the brain root or any tier directory; it must report every lane of the named roadmap,
across every repo, regardless of where it was invoked from. Resolve `BRAIN_ROOT` by walking up
from cwd for `brain.toml`, then invoke:

```
python3 <path-to-base-template>/scripts/roadmap_status_discovery.py --roadmap <slug> --root <BRAIN_ROOT>
```

The script owns discovery, join and normalization — realpath dedup, spec-slug resolution, liveness
from `updated_at`, operator-edge matching by type. **This command owns rendering and
interpretation only; it never reimplements the join in prose.** If the script errors (ambiguous
roadmap present in both `planning/roadmaps/<slug>/` and legacy `planning/<slug>/`, or the roadmap
not found anywhere), report the error verbatim and stop.

## Step 2 — Render, in D57 section 6's order

Render the JSON result as three sections, in this fixed order, one line per item, verdict before
reasoning, numbers rather than adjectives, target under ~30 lines total:

**1. What needs the operator now.** From `operator_coverage_total` and each lane's
`operator_gates.gates` — every `operator`/`approval` edge found in the graph, plus any block whose
lane log shows a bail awaiting a decision. State the coverage count explicitly:
`operator_coverage_total` gates found across `<N>` lanes, and always append the standing caveat
from the result's `coverage_caveat` field — gates recorded only in a roadmap's prose table (e.g.
`planning/operator-surface/roadmap.md`) are invisible to this graph-only read. If
`operator_coverage_total` is `0`, say so explicitly — do not omit the section.

**2. What is running or recently finished.** One row per lane: repo, current/last block, spec
slug, its `sdlc_state`'s liveness. Liveness is whatever `roadmap_status_discovery.py` computed from
`updated_at` against its named staleness threshold — a `running` state older than the threshold
renders as **stale**, with its age, never as live. Report the `status` field verbatim even when it
is a value outside the known vocabulary (`done`, `blocked`, `docs`, `running`, `passed`,
`completed`) — never silently remap an unknown value.

**3. What stopped and why.** Bails, `HELD` items, and any `OPEN` item surfaced in a lane's
`run_record` (`notes.md`), plus `repos_with_run_record_only` (a lane that wrote a run record but
logged no block — report it as a process finding, not a silent omission) and any lane present in
`repos_in_lane_log` with no matching run record.

**Every section renders even when empty.** An empty section is stated as empty
(`none found`), never omitted — an omitted section is indistinguishable from a bug (D57 section 6).

## Example

```
/roadmap-status
/roadmap-status --roadmap demand-ready
/roadmap-status --roadmap operator-surface
```

## Files

- Reads: `scripts/roadmap_status_discovery.py`'s output only (the roadmap's `lane-log.jsonl`,
  `planning/orchestration-run/<roadmap>/{notes.md,review.md}` per repo, each lane's
  `planning/<spec>/sdlc/sdlc-*state.json`, each repo's `planning/state.json`).
- Writes: nothing.

## Report

**<= 10 lines.** First line: outcome + whether it needs the operator. Then <= 6 one-line
bullets. Link paths; never restate a file. See the `report-to-the-operator` skill.

```
<roadmap>: <n> lanes — <running> running, <done> done, <stopped> stopped
- Needs you: <lane/block + why>, or "nothing"
- Stopped: <lane + the real bail reason>
```
Read-only: say so if nothing changed. Never dump per-lane detail the operator did not ask for.
