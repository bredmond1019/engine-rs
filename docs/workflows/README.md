---
type: Reference
title: What engine-rs can do
description: The capability catalogue — every workflow this engine can run, what each one does in plain English, and how to trigger it.
doc_id: workflows-readme
layer: [engine]
project: engine-rs
status: active
keywords: [workflows, capabilities, catalogue, triggering, policy, profiles]
related: [docs-index, architecture, workflows-index]
---

# What engine-rs can do

A **workflow** is a named graph of steps the engine can run — you POST it a payload, it walks the
graph, and it writes a result. This page lists every workflow that exists, says what each one is
for, and shows how to start one.

For how the engine is *built* (crates, types, seams), see [`../architecture.md`](../architecture.md).
For what is planned but does not exist yet, see [`../coming-soon.md`](../coming-soon.md).

## Quickstart

First point `$ENGINE` at your engine. **There is no single correct port**: the in-code local-dev
placeholder is `8080` (`DEFAULT_EVENTS_URL` in `crates/engine-serve/src/workflows.rs`), while the
Mac Mini deployment is assumed to be `8090` — and [`../deployment-launchd.md`](../deployment-launchd.md)
warns that assumption is unverified against the installed plist. Check yours before trusting either.

```bash
export ENGINE=http://localhost:8080     # or :8090 — confirm against your own plist
```

Now ask the running engine what it can do. This is the authoritative answer and is always current,
unlike the table below:

```bash
curl -s $ENGINE/workflows | jq                      # every registered workflow_type
curl -s $ENGINE/workflows/SDLC_TASK/graph | jq      # one workflow's declared node graph
```

Start a run. Every workflow is triggered the same way — one endpoint, `workflow_type` picks which:

```bash
curl -X POST $ENGINE/events/ \
  -H "X-API-Key: $ENGINE_EVENTS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"workflow_type":"DIAGNOSTIC_INTAKE","data":{ ... }}'
# -> 202 {"run_id":"...","event_id":"..."}
```

Then watch or inspect it:

```bash
curl -s $ENGINE/events/$EVENT_ID | jq              # final state
curl -N  $ENGINE/events/$EVENT_ID/stream           # live progress (SSE)
curl -X POST $ENGINE/events/$RUN_ID/abort -H "X-API-Key: $ENGINE_EVENTS_API_KEY"
```

| Must exist first | Why | If missing |
|---|---|---|
| `bastion serve` running with the engine mounted | The engine has no binary of its own — it embeds in `bastion serve` | See [`../deployment-launchd.md`](../deployment-launchd.md) |
| `ENGINE_EVENTS_API_KEY` | `POST /events/` returns `401` without a matching `X-API-Key` header | Same doc |
| `ENGINE_BRAIN_ROOT` | Any workflow that writes a Brain document needs it; absent, writes fail | Same doc |

## The workflows

Seventeen registered types, grouped by what you'd use them for. The registration list in
`crates/engine-serve/src/workflows.rs` (`register_builtin_workflows`) is the source of truth; this
table is a reader's copy.

### Building software

| Workflow | What it does | Detail |
|---|---|---|
| `SDLC_FLOW` | Runs a whole spec end to end on one branch: implement each task, test, fix, one consolidated review, a docs pass, then opens a PR. The heavyweight option. | [sdlc-flow.md](sdlc-flow.md) |
| `SDLC_TASK` | The lean version: implement → test → fix, and stop. No review, no docs pass, no PR. For one small unit of work. | [sdlc-task.md](sdlc-task.md) |
| `ORCHESTRATION` | Runs an ordered *chain* of the two above, across repos, checking dependencies and merging each block before the next starts. | [orchestration.md](orchestration.md) |
| `DEBRIEF` | Renders a morning brief from one campaign's journal — every step, every bail named with its reason — readable on a phone. | [debrief.md](debrief.md) |

### Winning and serving work

| Workflow | What it does | Detail |
|---|---|---|
| `RESEARCH_AGENT` | Researches a company (or prospects for new ones) and writes an opportunity document into the Brain. | [research-agent.md](research-agent.md) |
| `DIAGNOSTIC_INTAKE` | Turns raw notes or a transcript from a diagnostic call into structured fields. | [diagnostic-intake.md](diagnostic-intake.md) |
| `PROPOSAL_GENERATOR` | Scores and ranks automation opportunities, drafts a roadmap, reviews it, and prices it from a locale-correct rate card. | [proposal-generator.md](proposal-generator.md) |
| `DELIVERABLE_RENDER` | Turns that roadmap into a client-ready markdown deliverable plus a `typst`-rendered PDF. | [deliverable-render.md](deliverable-render.md) |
| `LEAD_INGEST` | Takes an inbound lead payload and writes it straight to a durable opportunity document, so a lead can't be lost to "the email was the only record". | [lead-ingest.md](lead-ingest.md) |
| `OPPORTUNITY_SET_STAGE` · `OPPORTUNITY_ADD_ACTION` | Two tiny edits to an existing opportunity document: move its pipeline stage, or append an action. | [opportunity-edit.md](opportunity-edit.md) |

### Content

| Workflow | What it does | Detail |
|---|---|---|
| `CONTENT_PIPELINE` | Channel-agnostic content run: fetch → summarize → self-critique loop → optional translate → render → publish. | [content-pipeline.md](content-pipeline.md) |
| `LINKEDIN_POST` | Drafts post candidates from real fleet work (commits, logs, decisions), then critiques them against the brand rubric. | [linkedin-post.md](linkedin-post.md) |

### Operator control

| Workflow | What it does | Detail |
|---|---|---|
| `HARVEST_APPROVE` | Presents a queued Brain write for a human yes/no before it lands. | [`../harvest-gate.md`](../harvest-gate.md) |
| `APPROVE_AND_RUN` | Takes an approved decision and actually executes the thing it authorized, recording one ledger row. | [approve-and-run.md](approve-and-run.md) |
| `TERMINAL_PROBE` | Opens (or reattaches to) a tmux session and reads its pane back. A diagnostic for the terminal stack, not business work. | [terminal-probe.md](terminal-probe.md) |

## Tuning a workflow: profiles and local models

Most workflows expose a **policy** — the knobs that trade cost, speed and quality (which model tier
each stage uses, how verbose prompts are, retry bounds, whether optional steps run). You do not
edit code to change these.

Two things you can do without touching Rust:

- **Pick a named profile** — `{"workflow_type":"...","data":{"profile":"cheap-fast", ...}}`.
- **Run a stage on a local model** instead of a cloud one, via `ModelTier::Local` and an
  OpenAI-compatible endpoint (defaults to Ollama at `http://localhost:11434`).

Both, plus the four-layer precedence that decides which setting actually wins, are in
[policy-and-profiles.md](policy-and-profiles.md). SDLC_FLOW's own knobs are in
[sdlc-flow-policy.md](sdlc-flow-policy.md).

## See also

- [index.md](index.md) — navigation table for this directory.
- [`../architecture.md`](../architecture.md) — crates, core types, data flow.
- [`../cli.md`](../cli.md) — the command-line surface.
- [`../coming-soon.md`](../coming-soon.md) — what is planned and does not exist yet.
- [`../suspend-resume.md`](../suspend-resume.md) — pausing and resuming any run.
- [`../orphan-recovery.md`](../orphan-recovery.md) — what happens to runs after a crash.
