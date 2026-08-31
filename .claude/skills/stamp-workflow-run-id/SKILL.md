---
name: stamp-workflow-run-id
description: How to record the Workflow tool's run id into an SDLC engine's on-disk state file — why the engine script can never do this itself, why the calling agent must, the exact patch recipe, and the null-is-normal contract downstream telemetry consumers rely on. Use immediately after any `Workflow({name:'sdlc-task'|'sdlc-flow', ...})` call, and before claiming a run's cost/token data can be joined to its Claude Code session transcript.
allowed-tools: Bash(python3:*) Bash(cat:*) Bash(ls:*)
---

# Stamping `workflow_run_id` into an engine's state file

Claude Code writes real, measured per-turn token usage — `input_tokens`, `output_tokens`,
`cache_creation_input_tokens`, `cache_read_input_tokens`, exact model id — into
`~/.claude/projects/<project-slug>/<session-id>/subagents/workflows/wf_*/agent-*.jsonl`. That is a
far better cost signal than either engine's own `tokens` roll-up (which models no cache channel and
has been measured low by ~100x on a real run). Joining a run-state file to its transcript exactly,
instead of inferring the join from a `started_at`/`updated_at` timestamp window, is what
`workflow_run_id` is for. Full contract: `docs/data-contract.md#workflow_run_id--optional-caller-stamped`.

## Why the engine can't do this itself

`.claude/workflows/sdlc-task.js` and `sdlc-flow.js` are Workflow-tool scripts. The Workflow script
API (`agent`/`pipeline`/`parallel`/`log`/`phase`/`args`/`budget`/`workflow`) exposes **no `runId`
global**, and scripts have **no filesystem or Node.js API access** — so a script cannot read the
run id back out of its own on-disk snapshot path
(`~/.claude/projects/<proj>/<session>/workflows/scripts/sdlc-<engine>-wf_<runid>.js`) either. There
is no code change inside either engine that fixes this. Do not go looking for one.

## Why the calling agent can

`Workflow({name: 'sdlc-task'|'sdlc-flow', args})` runs in the background and **returns immediately**
with the run id in its tool result — before the engine has done anything. Whichever agent turn made
that call already has the run id, full Bash/Write access, and knows the spec slug it passed in
`args`. That agent is the only party in the whole flow positioned to write the id anywhere.

## The recipe

1. Call `Workflow({name: 'sdlc-task', args: '<spec-slug> ...'})` (or `sdlc-flow`) as normal.
2. Note the run id from the tool result (the `wf_...`-shaped id).
3. Wait until `planning/<spec-slug>/sdlc/sdlc-task-state.json` (or `sdlc-flow-state.json`) exists —
   it's written at the end of the engine's first state-write stage, not at launch. Don't create it
   yourself if it's missing; that means the engine hasn't reached its first write yet.
4. Patch the field in with a small, idempotent JSON rewrite — do **not** hand-edit the file with a
   text editor, and do not touch any other key:

```bash
python3 -c "
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d['workflow_run_id'] = sys.argv[2]
json.dump(d, open(p, 'w'), indent=2)
" planning/<spec-slug>/sdlc/sdlc-task-state.json '<runId>'
```

5. If the file never appears (the run bailed before any state write, or you're driving a
   shell-less/manual-replication agent that never actually called the Workflow tool — see the
   `.agents/skills/sdlc-task/SKILL.md` guide, which has no `runId` concept at all), do nothing. Do
   not synthesize a substitute id.

## The null-is-normal contract

`workflow_run_id` is optional and nullable on both `sdlc-task-state.json` and
`sdlc-flow-state.json`. **Absence is the common case, not a defect** — plenty of legitimate runs
(manual replication, an interrupted session, a caller that skipped step 4) will never carry it. Any
code or agent reading this field — a dashboard, `mev`, a telemetry consumer like jynx — must treat
`null`/missing as "fall back to the timestamp-window join," never as something to flag, backfill, or
block on.

## When this applies

- Every `/sdlc-task` or `/sdlc-flow` invocation made through the real Workflow tool.
- Not the `.agents/skills/sdlc-task` or `sdlc-flow` manual-replication guides — those exist for a
  shell-less agent that cannot call the Workflow tool at all, so there is no run id to capture.
- Not `/patch` or the retired one-off stage commands — this only exists on the two Workflow-backed
  engines.
