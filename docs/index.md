---
type: Index
title: engine-rs Docs
description: Navigation index for all engine-rs reference documentation — grouped by what you are trying to do.
doc_id: docs-index
layer: [meta]
project: engine-rs
status: active
keywords: [documentation, index, navigation, engine-rs, reference]
related: [core:engine-rs, workflows-index]
---

# engine-rs — Documentation Index

A map of every doc in this directory, grouped by what you're trying to do. Each row is one line;
the detail lives in the doc itself.

**New here? Two pages answer most questions:** [workflows/README.md](workflows/README.md) for what
the engine can do, and [architecture.md](architecture.md) for how it's built.

## Start here

| Doc | What it covers |
|---|---|
| **[workflows/](workflows/README.md)** | **"What can this thing do, and how do I run it."** Every registered workflow, what each does, and how to trigger one. Also holds every per-workflow reference doc — file listing at [workflows/index.md](workflows/index.md) |
| [architecture.md](architecture.md) | How the engine is built: crate layout, core types, injectable seams, data flow |
| [cli.md](cli.md) | The command-line surface — synopsis, subcommands, flags, exit codes |
| [coming-soon.md](coming-soon.md) | What is planned and **does not exist yet**, each with its block ID and what it waits on |

## Tuning and running workflows

| Doc | What it covers |
|---|---|
| [workflows/policy-and-profiles.md](workflows/policy-and-profiles.md) | Change cost, speed and quality without editing Rust — named profiles, the four-layer precedence, and running a stage on a local model |
| [workflows/sdlc-flow-policy.md](workflows/sdlc-flow-policy.md) | `SDLC_FLOW`'s own knobs, its five named profiles, and run telemetry |
| [suspend-resume.md](suspend-resume.md) | Pausing and resuming a run, and campaign-level crash recovery |
| [orphan-recovery.md](orphan-recovery.md) | What happens to runs stranded by a crash — the boot sweep and the stale-run alarm |
| [cron-primitive.md](cron-primitive.md) | The durable scheduling primitive: calendar vs. interval schedules, and the restart-durable store |

## Deploying and testing

| Doc | What it covers |
|---|---|
| [deployment-launchd.md](deployment-launchd.md) | The environment variables a permanently-running `bastion serve` needs, and how to verify the installed plist |
| [testing.md](testing.md) | Which test commands to run, the one-binary-per-crate test layout, and the hermetic-test conventions |

## Contracts and boundaries

Where this engine meets something it does not own — the Brain, the operator, an outside service.

| Doc | What it covers |
|---|---|
| [data-contract.md](data-contract.md) | The versioned `events`/`node_runs` schema, its Rust type mappings, and the re-pin checklist |
| [materialize-doc-node.md](materialize-doc-node.md) | The writer node that turns a workflow result into a Brain document, and its injectable seam |
| [harvest-gate.md](harvest-gate.md) | The `off`/`in_process`/`approval` gate that decides whether a Brain write happens at all |
| [operator-payload-contract.md](operator-payload-contract.md) | What the engine may send a human: payload limits, the operator queue, and run-failure notifications |
| [approval-ledger.md](approval-ledger.md) | The append-only record of every gate decision — who approved what, and when |
| [email-adapter.md](email-adapter.md) | The email channel: outbound sending, both inbound webhooks, and their auth |

## The terminal stack

How the engine drives a real tmux session. Read in this order.

| Doc | What it covers |
|---|---|
| [terminal-crates.md](terminal-crates.md) | Why `term-core`/`term-attach` are two crates, and what each holds |
| [terminal-driver.md](terminal-driver.md) | The `TerminalDriver` seam, the fail-closed session lease, and the operator hold |
| [terminal-nodes.md](terminal-nodes.md) | The node stack built on that seam — observe, guarded send, bounded await — and its invariants |

---

Project strategy and current focus live in this repo's `planning/` directory. It is a symlink into
the private company-brain vault and is **not** part of the public repo, so it is referenced here as
a bare path rather than a link: `engine-rs/planning/index.md`.
