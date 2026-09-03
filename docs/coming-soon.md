---
type: Reference
title: engine-rs — Not Yet Built
description: Capabilities on the engine-rs roadmap that do not exist yet, what each will do, and what it is waiting on — so a reader can tell "documented and shipped" from "documented and planned".
doc_id: engine-rs-coming-soon
layer: [engine]
project: engine-rs
status: active
keywords: [roadmap, coming soon, not built, planned, blocked, capabilities]
related: [docs-index, architecture, orchestration-workflow]
---

# engine-rs — Not Yet Built

**Everything on this page is planned and does not exist yet.** Every other doc in
[`docs/`](index.md) describes code you can run today. This one exists so the two are never
confused: a reader who searches the docs for `CONDUCTOR` should find out in one step that it is a
roadmap item, not a workflow they have mis-invoked.

## Quickstart — is this thing built or not?

```bash
# The authoritative answer for any block, from the repo root:
python3 -c "
import json
d = json.load(open('planning/state.json'))
for t in d['tracks']:
    for b in t['blocks']:
        if b['id'] == 'EN.12.F':          # <- the block you care about
            print(b['id'], '->', b['status'])
"
```

`status: closed` means shipped. Anything else means not yet. **`planning/state.json` is the
authority; this page is a hand-maintained snapshot and will lag it.** If the two disagree, the graph
wins — see the brain's `CLAUDE.md` on why prose gates nothing.

## How to read the tables

- **Waiting on** names what has to land first. A `repo:BLOCK` entry is another repo's work; an
  `OP.<slug>` entry is an operator gate — a decision or credential only a human can supply, which
  blocks the work until the named artifact exists.
- **Ready** means every declared dependency is met and the block could be started now. It does not
  mean it is next in any lane's order.
- Block IDs (`EN.12.D`) are this repo's roadmap identifiers. Their authored definitions live in
  `planning/blocks/<ID>.json`; the narrative lives in `planning/master-plan.md`.

## Autonomy — the overnight loop

The largest unbuilt cluster. Today a chain runs the blocks it is handed and reports what happened;
none of the pieces below exist, so an operator still chooses the work and reads the results.

| Capability | Block | State | What it will do |
|---|---|---|---|
| `CONDUCTOR` | `EN.12.F` | Waiting on `OP.first-weekly-objective` | The run picks tonight's chain itself from a weekly objective, instead of being handed a block list. |
| Research → action items | `EN.12.H` | **Ready** | A scheduled research chain files into the operator queue. |
| Research → demo | `EN.12.I` | Waiting on `EN.12.F` | An overnight branded demo generated from a company name. |

`CONDUCTOR` waits directly on `OP.first-weekly-objective` — an operator gate — and `Research →
demo` waits on `CONDUCTOR`, so it is gated on the same objective transitively. The engine cannot
pick a chain from an objective that nobody has written yet, and it may not invent one.

## Brain integration

The engine writes to the Brain today ([`materialize-doc-node.md`](materialize-doc-node.md),
[`harvest-gate.md`](harvest-gate.md)). Reading back from it is ruled but unbuilt.

| Capability | Block | State | What it will do |
|---|---|---|---|
| `CLAIM_REAFFIRM` | `EN.6.L` | **Ready** | Distilled-claim reaffirmation via queue-drain. |

The read direction was gated on an operator ruling until 2026-08-23; it is now settled by
`planning/decisions/D23-brain-read-seam.md` — the engine **may** read back from Synapse as a typed
consumer that never re-ranks. The blocks above are the implementation of that ruling.

## Artifacts and deliverables

| Capability | Block | State | What it will do |
|---|---|---|---|
| `EXTERNAL_INTEL` | `EN.5.C` | **Deferred** (dependency-clear — parked, not blocked) | Ecosystem sweep and ranked digest — the engine half of a Synapse pairing. |
| Regression history + blind judge | `EN.5.B2` | **Deferred** (dependency-clear — parked, not blocked) | Keep-if-better / revert-if-worse change gate. |

## Run integrity and identity

| Capability | Block | State | What it will do |
|---|---|---|---|
| `routine.sh` behind an approval gate | `EN.9.H` | **Ready** | Excluding its self-restart. |

## Known defects, filed and not yet fixed

These are shortcomings in shipped code rather than absent features. Listed here because a reader
hitting one should find it named rather than assume they are holding it wrong.

**None currently filed.** The five defects previously listed here (`EN.ticket.test-gate-must-
terminate-a-hang-not-wedge`, `EN.ticket.close-block-node-leaves-derived-output-uncommitted`,
`EN.ticket.vault-dependent-tests-must-skip-not-fail`, `EN.ticket.diagnostic-intake-fixture-
tempdir`, `EN.ticket.local-policy-harness-file`) are resolved — the first four `closed`, the last
`wontfix` and superseded by `EN.3.K` (also `closed`) — and their rows have been removed per the
rule below.

## Keeping this page honest

This page is a snapshot and will drift. Two habits keep it useful:

- **When a block closes, delete its row here.** A "coming soon" list that still advertises shipped
  work is worse than no list, because it teaches readers to distrust the page.
- **Do not add a row for a block that has no record.** If it is not in `planning/state.json`, it does
  not exist — including as a plan. File the block first.
