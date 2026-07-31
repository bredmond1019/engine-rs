---
type: Guide
title: SDLC_FLOW End-to-End Smoke Run
description: How to trigger and watch a real SDLC_FLOW run from bastion-web through to a terminal state, the six undocumented operational prerequisites, and cleanup.
doc_id: sdlc-flow-smoke
layer: [engine]
project: engine-rs
status: active
keywords: [sdlc-flow, smoke-test, bastion-web, worktree, e2e, prerequisites, health-check]
related: [sdlc-flow-workflow, sdlc-flow-policy, architecture]
---

# SDLC_FLOW End-to-End Smoke Run

This guide documents how to prove the Rust `SDLC_FLOW` workflow works end to end — triggered
from bastion-web, walking the real graph with real agentic write permission, reaching a terminal
state on disk — using the minimal smoke spec at `planning/smoke-sdlc-flow/` and the harness script
at `scripts/sdlc_smoke.sh`.

## Prerequisites

None of the following are documented anywhere else, and each produces a confusing failure when
unmet.

1. **`bastion serve`'s working directory MUST be `core/engine-rs`.** This is the single most
   important operational fact about the whole system. There is no `repo` field on the `SDLC_FLOW`
   event; every path `SetupWorktreeNode` touches is relative to the serve process's cwd —
   including the `planning` symlink resolution and the `git checkout -B ... origin/main` fallback,
   both run against `Path::new(".")` (`crates/engine-core/src/workflows/sdlc_flow/setup.rs:211-228`).
   Starting `bastion serve` from any other directory makes every worktree/branch operation resolve
   against the wrong tree.
2. **`DATABASE_URL` set with Postgres reachable AND `BASTION_ENGINE_API_KEY` non-empty.**
   `decide_engine_mount` (`core/bastion/src/serve/mod.rs:103-133`, called at `mod.rs:257-259`)
   mounts the engine routes only when both hold; otherwise it returns `Skip` and `POST /events/`
   simply 404s with no boot-time error explaining why.
3. **The same API key configured in bastion-web's BFF** — `BASTION_SERVE_URL` +
   `BASTION_ENGINE_API_KEY` (`core/bastion-web/lib/env.ts:79-93`). Note the engine uses
   `X-API-Key`, a scheme entirely separate from the `BASTION_SERVE_TOKEN` bearer used by the rest
   of `bastion serve`.
4. **`claude` CLI on the serve process's PATH and authenticated** — the agentic nodes shell out to
   it.
5. **`git fetch origin` first** — `SetupWorktreeNode`'s `git worktree add ... origin/main` fails
   outright against a stale `origin/main` ref.
6. **`trees/sdlc/smoke-sdlc-flow` must not already exist** — run `scripts/sdlc_smoke.sh --clean`
   first.

## The event body and why every flag is load-bearing

```json
{ "workflow_type": "SDLC_FLOW",
  "data": { "spec_slug": "smoke-sdlc-flow", "use_worktree": true, "auto_pr": false, "profile": "cheap-fast" } }
```

- **`use_worktree: true` is not optional.** The field defaults to `false`
  (`crates/engine-core/src/workflows/sdlc_flow/schema.rs:133`), and on that path
  `SetupWorktreeNode` runs `git checkout -B sdlc/smoke-sdlc-flow origin/main` in the **live
  checkout** (`crates/engine-core/src/workflows/sdlc_flow/setup.rs:220-228`) — moving HEAD out from
  under you while an agentic node with real write permission edits the real tree. Omitting this
  flag is the most damaging mistake available when running this smoke.
- **`auto_pr` defaults to `true`** (`default_auto_pr`, `crates/engine-core/src/workflows/sdlc_flow/schema.rs:101-103`;
  the field at `schema.rs:118-119`), so it must be set `false` explicitly. With it false,
  `PullRequestNode` short-circuits cleanly to `{ pr_url: null, skipped: true }`
  (`crates/engine-core/src/workflows/sdlc_flow/pr.rs:91-99`) and `gh` is never invoked — the smoke
  needs no GitHub auth and opens no PR.
- **`profile: "cheap-fast"`** keeps the run at the cost/latency floor and additionally exercises
  EN.3.D's `test_depth: Fast` resolution, so the smoke covers the check-selection path that block
  depends on.

## You cannot watch the run from bastion-web

QuickLaunch (`core/bastion-web/components/trigger/quick-launch.tsx:51-52, 125, 161-167`) polls
`GET /api/events/{id}` exactly six times at 1s intervals (`POLL_INTERVAL_MS = 1000`,
`POLL_MAX_TICKS = 6`). That is a fail-fast "did the engine accept this trigger" check, not a run
monitor — bastion-web consumes no SSE at all; live streaming is bastion-web block `BW.3.C`, not
started. QuickLaunch going quiet after ~6 seconds is expected behavior, not a failed run.

**The procedure is therefore: trigger from QuickLaunch, watch from the terminal** with
`scripts/sdlc_smoke.sh --watch <event_id>`.

## Run procedure

1. `git fetch origin`
2. `scripts/sdlc_smoke.sh --clean` (tolerant of nothing existing yet)
3. Trigger the run — either:
   - from bastion-web QuickLaunch with `workflow_type: SDLC_FLOW` and the event body above, noting
     the `event_id` it reports, or
   - directly via `scripts/sdlc_smoke.sh` (which triggers and watches in one step).
4. If triggered from QuickLaunch, attach the watcher: `scripts/sdlc_smoke.sh --watch <event_id>`.
5. Verify the three artifacts once the watcher reports `succeeded`:
   - `trees/sdlc/smoke-sdlc-flow/SMOKE.md` contains `ENGINE-SMOKE`.
   - `planning/smoke-sdlc-flow/sdlc/sdlc-flow-state.json` reports `status: "done"` with a `run_id`
     equal to the `event_id` from the 202 response.
   - The watcher itself exited 0.
6. Clean up: `scripts/sdlc_smoke.sh --clean`.

## Cleanup

```
git worktree remove --force trees/sdlc/smoke-sdlc-flow
git branch -D sdlc/smoke-sdlc-flow
rm -rf planning/smoke-sdlc-flow/sdlc
```

The third command is not optional. `SpecExistsRouterNode::route`
(`crates/engine-core/src/workflows/sdlc_flow/setup.rs:318-327`) checks
`dir.join("sdlc").join("sdlc-flow-state.json").exists() || dir.join("tasks.json").exists()` and
routes to `LoadTaskStateNode` when either holds. A leftover state file from a previous smoke
therefore makes the next trigger **resume a run that is already `done`** — it appears to succeed
instantly having executed nothing at all. Deleting the state dir is what makes the next smoke a
real one.

`scripts/sdlc_smoke.sh --clean` runs all three steps for you, tolerating each resource already
being absent.

## Status vocabulary

The whole block turns on this. There is no `"completed"` status anywhere in the system.

- **Terminal** (`derive_terminal_status`, `crates/engine-serve/src/http.rs:295-331`): `succeeded`,
  `failed`, `cancelled`, `budget_halted`.
- **Live** (`derive_live_status`, `crates/engine-serve/src/http.rs:333-343`): `running`,
  `suspended`.

`agentic-portfolio/scripts/health_check.sh` previously compared against the nonexistent
`"completed"` string, which meant a successful smoke run was reported as FAIL — fixed as part of
this block (committed separately in the parent `agentic-portfolio` repo, since that script lives
outside engine-rs).
