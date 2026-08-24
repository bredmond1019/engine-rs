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
| Run journal | `EN.12.D` | **Ready** | Durable decision rows, a D57 renderer, and a read route — the substrate the two below render from. |
| `WORKFLOW_DISPATCH` | `EN.12.E` | Waiting on `EN.12.D` | Lets a chain step run any registered workflow, not just an SDLC engine. |
| `CONDUCTOR` | `EN.12.F` | Waiting on `mev:MV.14.B`, `EN.12.D`, `EN.12.E`, `OP.first-weekly-objective` | The run picks tonight's chain itself from a weekly objective, instead of being handed a block list. |
| `DEBRIEF` | `EN.12.G` | Waiting on `EN.12.D`, `EN.12.F`, `OP.first-weekly-objective` | A morning brief rendered from the journal, readable on a phone. |
| Research → action items | `EN.12.H` | Waiting on `EN.12.E`, `bastion:BA.21.D` | A scheduled research chain files into the operator queue. |
| Research → demo | `EN.12.I` | Waiting on `EN.12.E`, `EN.12.F` | An overnight branded demo generated from a company name. |

Two of these wait on `OP.first-weekly-objective` — an operator gate. The engine cannot pick a
chain from an objective that nobody has written yet, and it may not invent one.

## Brain integration

The engine writes to the Brain today ([`materialize-doc-node.md`](materialize-doc-node.md),
[`harvest-gate.md`](harvest-gate.md)). Reading back from it is ruled but unbuilt.

| Capability | Block | State | What it will do |
|---|---|---|---|
| Brain read client | `EN.6.K` | **Ready** | `HttpGet` seam, `BrainConfig`, a `RecallNode` over `GET /recall`, plus ingest-client hardening. |
| Recall consumer | `EN.12.L` | Waiting on `orchestrator:OR.3.B`, `EN.12.E` | The engine half of the read seam, consuming a contract-pinned recall result mid-chain. |
| `CLAIM_REAFFIRM` | `EN.6.L` | Waiting on `EN.6.K`, `mev:MV.ticket.distill-freshness-lane` | Distilled-claim reaffirmation via queue-drain. |
| Content-pipeline ingest fix | `EN.12.K` | Waiting on `orchestrator:OR.3.A` | Real route, auth header, and the chosen payload mapping. |

The read direction was gated on an operator ruling until 2026-08-23; it is now settled by
`planning/decisions/D23-brain-read-seam.md` — the engine **may** read back from Synapse as a typed
consumer that never re-ranks. The blocks above are the implementation of that ruling.

## Artifacts and deliverables

| Capability | Block | State | What it will do |
|---|---|---|---|
| `CONTENT_DRAFT` | `EN.5.G` | **Ready** | Draft posts from shipped work, review-gated. |
| `EXTERNAL_INTEL` | `EN.5.C` | Waiting on `orchestrator:OR.Q` | Ecosystem sweep and ranked digest — the engine half of a Synapse pairing. |
| Regression history + blind judge | `EN.5.B2` | **Ready** | Keep-if-better / revert-if-worse change gate. |

## Run integrity and identity

| Capability | Block | State | What it will do |
|---|---|---|---|
| Artifact identity | `EN.11.A` | Waiting on `brain:HQ.5.A` | Stamp build, writer, `run_id` and host onto produced artifacts. |
| Chains compose | `EN.11.C` | **Ready** | Guarantee block N+1's tree contains block N's work. |
| Permission-profile enforcement | `EN.12.C` | Waiting on `bastion:BA.21.C`, `brain:HQ.5.B` | Stamp the profile in force into every run record and gate graded actions on it. |
| `GuardedSender` adoption | `EN.12.B` | **Ready** | Adopt it at both terminal send call sites. |
| `routine.sh` behind an approval gate | `EN.9.H` | Waiting on `brain:HQ.ticket.restart-services-drain-guard` | Excluding its self-restart. |

## Known defects, filed and not yet fixed

These are shortcomings in shipped code rather than absent features. Listed here because a reader
hitting one should find it named rather than assume they are holding it wrong.

| Defect | Block | What is wrong today |
|---|---|---|
| The test gate can wedge | `EN.ticket.test-gate-must-terminate-a-hang-not-wedge` | No `nextest.toml` exists, so the default profile never terminates a hung test — a genuine hang blocks the gate forever with no verdict. |
| `CloseBlockNode` leaves derived output uncommitted | `EN.ticket.close-block-node-leaves-derived-output-uncommitted` | It must stage and commit the derived fallout of its own state write. |
| Vault-dependent tests fail instead of skipping | `EN.ticket.vault-dependent-tests-must-skip-not-fail` | A test needing the brain vault hard-fails on any checkout without one, including CI. |
| The test suite mutates a tracked fixture | `EN.ticket.diagnostic-intake-fixture-tempdir` | `diagnostic-intake-state.json` is rewritten in place by running the suite. |
| No env-var policy source | `EN.ticket.local-policy-harness-file` | `PolicyConfigSource::HarnessFile` for the three API-shaped workflows. Interim — superseded by `EN.3.K`. |

## Keeping this page honest

This page is a snapshot and will drift. Two habits keep it useful:

- **When a block closes, delete its row here.** A "coming soon" list that still advertises shipped
  work is worse than no list, because it teaches readers to distrust the page.
- **Do not add a row for a block that has no record.** If it is not in `planning/state.json`, it does
  not exist — including as a plan. File the block first.
