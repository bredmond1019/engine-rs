---
type: Guide
title: Deploying `bastion serve` Under launchd
description: The plist EnvironmentVariables a permanently-running bastion serve (engine embedded) needs on the Mac Mini, what WorkingDirectory still determines after EN.3.K, and the soft-to-loud failure mode ENGINE_BRAIN_ROOT now carries for SDLC_FLOW.
doc_id: deployment-launchd
layer: [engine, infra]
project: engine-rs
status: active
keywords: [launchd, deployment, env-vars, brain-root, repo-registry, working-directory, mac-mini]
related: [sdlc-flow-workflow, sdlc-flow-smoke, architecture]
---

# Deploying `bastion serve` Under launchd

`bastion serve` (the engine embedded, per `core/engine-rs/CLAUDE.md`) runs permanently on the Mac
Mini under launchd — not started interactively from a shell with an inherited environment.
launchd does **not** read `.env` files (`agentic-portfolio/docs/infrastructure.md:140`): every env
var the process needs must be listed explicitly in the plist's `EnvironmentVariables` dictionary,
or the process simply doesn't see it. This doc is the checklist for that dictionary, plus what
`WorkingDirectory` does and does not still determine now that `EN.3.K` gives `SDLC_FLOW` an
explicit, registry-resolved `repo` slug.

## Required `EnvironmentVariables`

| Var | Required? | What it's for |
|---|---|---|
| `ENGINE_BRAIN_ROOT` | **Mandatory as of EN.3.K** | The root the repo registry resolves `brain.toml` against (`crates/engine-core/src/brain_root.rs`). Without it, `RepoRegistry::from_env()` falls back to walking up from the process cwd looking for `brain.toml` — a walk-up that only succeeds if `WorkingDirectory` happens to sit inside the brain tree. Under launchd there is no guarantee of that, so treat this as mandatory, not optional. |
| `ENGINE_EVENTS_API_KEY` | Already required (pre-EN.3.K) | The `X-API-Key` value `check_api_key` gates `POST /events/` (and friends) against; read by `WorkflowTriggerDispatch`/channel egress (`crates/engine-core/src/nodes/channel_transport.rs`). |
| `ENGINE_EVENTS_URL` | Already required (pre-EN.3.K) | The deployment-configured base URL for the server's own `/events/` endpoint, used by egress nodes that loop back through HTTP (`crates/engine-serve/src/workflows.rs`). |
| `ENGINE_REPO_ALLOWLIST` | Optional | Comma-separated slugs narrowing the repo registry (see [`sdlc-flow-workflow.md`](workflows/sdlc-flow.md)). Unset — the default, and what the Mac Mini runs — means every `brain.toml` slug is reachable. |
| `ENGINE_LOG` | Optional | `tracing_subscriber::EnvFilter` string controlling log verbosity for the JSON tracing subscriber `engine_serve::init_tracing()` installs (e.g. `debug`, `engine_core=debug,info`). Unset defaults to `info`, matching pre-tracing `eprintln!` visibility. |

`DATABASE_URL` and `BASTION_ENGINE_API_KEY` remain required for `bastion serve` to mount the
engine routes at all (`decide_engine_mount`, `core/bastion/src/serve/mod.rs`) — unrelated to
`EN.3.K` but listed here because a missing engine mount produces the same class of confusing
404-with-no-boot-error this doc exists to head off. See
[`sdlc-flow-smoke.md`](workflows/sdlc-flow-smoke.md)'s Prerequisites section for the full list.

## What `WorkingDirectory` still determines

Before `EN.3.K`, the plist's `WorkingDirectory` **was** the answer to "which repo does this server
serve" — every `SDLC_FLOW` run resolved its target repo as `std::env::current_dir()`, so one
launchd service could drive runs against exactly one of the fleet's repos.

As of `EN.3.K`, `WorkingDirectory` **no longer determines which repo a run targets** for any
`SDLC_FLOW` event that sends a `repo` slug — that event now carries its own explicit,
registry-resolved target root, resolved from `brain.toml` via `ENGINE_BRAIN_ROOT`, independent of
the process's cwd. A single permanently-running `bastion serve` can now drive runs against any repo
named in the registry.

`WorkingDirectory` **remains the fallback target for absent-`repo` events** — an event with no
`repo` field still resolves to `current_dir()`, byte-identical to pre-EN.3.K behavior (see
[`sdlc-flow-workflow.md`](workflows/sdlc-flow.md)). Set it to a sane checkout (e.g. this repo's
working tree) rather than leaving it at `/` — an absent-`repo` event dispatched against `/` would
still 422 downstream (no `planning/<slug>/` under `/`), but there is no reason to depend on that
rather than pointing it somewhere sane.

## The soft-to-loud failure this block introduces

Before `EN.3.K`, a missing `ENGINE_BRAIN_ROOT` degraded gracefully: `resolve_brain_root()`'s
walk-up from cwd usually succeeded, because the process typically ran with its cwd already inside
the brain tree (an interactive shell, a dev loop). Under launchd, with a `WorkingDirectory` that
may sit outside the brain tree entirely, that walk-up fails — and per `brain_root.rs`'s own design
(never a silent `.` fallback), the failure is loud: **every `repo`-bearing `SDLC_FLOW` event 422s**
until `ENGINE_BRAIN_ROOT` is set correctly in the plist. `init_repo_registry_from_env`
(`crates/engine-serve/src/workflows.rs`) emits a structured `tracing::warn!` event with the reason
at startup and leaves the process-global registry unset rather than failing to boot — absent-`repo`
events are unaffected,
but every `repo`-bearing one fails until the plist is fixed.

This makes deploying this plist correctly a **hard prerequisite** for using the `repo` field at
all under launchd, not a documentation nicety — and it partially closes carryover
`en7d-brain-root-not-set-in-deployment` (`ENGINE_BRAIN_ROOT` was previously set nowhere in
`scripts/` or `docs/infrastructure.md`).

## Verify the labels before trusting them

`agentic-portfolio/scripts/restart_services.sh` assumes the launchd label `com.brandon.engine-serve`
on port `8090` — but its own header warns this configuration is "A STARTING POINT, NOT A CONFIRMED
FACT." Before wiring `ENGINE_BRAIN_ROOT` (or any other var in the table above) into a real plist,
confirm the label, port, and `WorkingDirectory` against the plist actually installed on the Mac
Mini (`launchctl list | grep bastion`, then inspect the plist file itself) rather than assuming the
script's header comment is current. Do not treat this doc's table as a substitute for that check —
it states which vars are needed, not the plist's exact installed shape.
