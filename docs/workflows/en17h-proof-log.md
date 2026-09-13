---
type: Log
title: EN.17.H Task 1 Proof Log
description: Positive-control results and fixture-ticket registration status for EN.17.H's unattended-chain proof.
doc_id: en17h-proof-log
layer: [engine, meta]
project: engine-rs
status: active
keywords: [en17h, sweep, coord, positive-control, telegram, fixture-tickets]
related: [workflows-sweep, coordination]
---

# EN.17.H Task 1 — Proof Log

Records, in order, the safety check, the two required positive controls, and the resulting
decision on whether to register the block's three fixture tickets in HQ's `planning/state.json`.

## Safety check

```
$ cat /Users/brandon/Dev/agentic-portfolio/.fleet-locks/leases/lease-brain.json
{
  "acquired_at": "2026-09-12T23:16:11.482991+00:00",
  "agent": "engine-rs-56",
  "heartbeat": "2026-09-12T23:16:11.482991+00:00",
  "kind": "exclusive",
  "lane": "engine-unattended",
  "repo": "brain"
}
```

Only this lane's own lease is held on `brain`; no collision.

## Positive control (a) — coord send/status round trip: PASS

Sent one throwaway `QUERY` envelope to a scratch lane via `bastion coord send`, then confirmed
`bastion coord status --json` actually surfaces it in `messages[]`:

```
$ bastion coord send --repo scratch-en17h --lane scratch-lane --file scratch-msg.json
{"path":"/Users/brandon/Dev/agentic-portfolio/.fleet-locks/queue/scratch-en17h/scratch-lane/inbox/20260912T233000Z-en17h-scratch-0001-0000-4000-8000-000000000001.json","sent":true}

$ bastion coord status --json | jq '.messages[] | select(.path | test("en17h-scratch"))'
{
  "path": ".../queue/scratch-en17h/scratch-lane/inbox/20260912T233000Z-en17h-scratch-0001-....json",
  "message": {
    "message_id": "en17h-scratch-0001-0000-4000-8000-000000000001",
    "sender": {"agent_name": "engine-rs-56", "repo": "engine-rs", "lane": "engine-unattended", "roadmap": "rust-unattended-chain"},
    "sent_at": "2026-09-12T23:30:00Z",
    "kind": "QUERY",
    "subject": {"repo": "engine-rs"},
    "body": "EN.17.H task 1 positive control (a): scratch throwaway message, safe to discard.",
    "durable_home": {"channel": "lane-log", "ref": "engine-rs/planning/EN.17.H/proof-scratch"},
    "verified_by": "UNVERIFIED: engine-rs-56"
  }
}
```

The reader sees the queue write. **PASS.**

## Positive control (b) — SWEEP -> Telegram dispatch: BLOCKED, not PASS

Task text named `crates/engine-core/src/workflows/sweep/graph.rs` as the schema source; that
path does not exist (verified with a direct file check before use — the module lives at
`crates/engine-core/src/workflows/sweep/mod.rs`, with routing in `sweep/route.rs` and snapshot
assembly in `sweep/snapshot.rs`). Resolved the real event contract there instead:
`SweepNode::process` reads `root` (an absolute filesystem path, caller-supplied) and `roadmap`
(a slug) off `ctx.event`; `run_sweep_pass` diffs a fresh snapshot of
`<root>/planning/roadmaps/<roadmap>/escalations.jsonl` against the last one on disk, and a
`channel: notification` escalation with a well-formed 2-3-entry `options` array routes to
`OperatorTransport::send` (`GatedAction::Notify`).

Built an isolated scratch root (not the real HQ tree, so the real fleet's roadmap corpus is
untouched) with one fixture escalation:

```
<scratch>/planning/roadmaps/en17h-scratch/escalations.jsonl:
{"gate_id": "en17h-scratch-gate-1", "kind": "advisory", "channel": "notification",
 "severity": "info", "summary": "EN.17.H task 1 positive control (b): fixture escalation,
 safe to discard.", "options": [{"key": "ack", "label": "Acknowledge"},
 {"key": "defer", "label": "Defer"}]}
```

Dispatch attempts, both against the real local `bastion serve` on `127.0.0.1:4317` (confirmed
live: `GET /health` -> `{"status":"ok","service":"bastion","engine_build_sha":"f7c03d66..."}`):

```
$ curl -s -X POST 127.0.0.1:4317/events/ -d '{"workflow_type":"SWEEP","root":"<scratch>","roadmap":"en17h-scratch","now":"2026-09-12T23:35:00Z"}'
{"code":"unauthorized","error":"unauthorized"}   # HTTP 401

$ bastion run SWEEP --args '{"root":"<scratch>","roadmap":"en17h-scratch","now":"2026-09-12T23:35:00Z"}'
Error: failed to trigger workflow 'SWEEP' — is the orchestrator running?
Caused by:
    trigger endpoint returned 401 — set BASTION_ENGINE_API_KEY (or config.toml's engine_api_key); trigger_workflow sends X-API-Key only when one is configured
```

`BASTION_ENGINE_API_KEY` is unset in this shell and `~/.config/bastion/config.toml` carries no
`engine_api_key`. No key was discoverable anywhere this session has read access to. **This
control could not be run — it is BLOCKED on a missing credential, not attempted and not
fabricated as passing.**

## Decision

Per the task's own gating language, both positive controls "must both be recorded as PASSING
before trusting any 'zero sends' result later in this block." Control (b) did not pass — it
never ran. Registering the block's three fixture tickets (BAIL / FALSE PREMISE / DEPENDENT) in
HQ's `planning/state.json`, and the rest of EN.17.H's live-chain proof, depend on this control
having actually exercised the Telegram path; proceeding without it would let a real "zero sends"
result later in the block go untrusted for the wrong reason (transport untested) rather than the
right one (no escalation fired). **No fixture tickets were registered this attempt.**

Unblocking this needs an operator action: set `BASTION_ENGINE_API_KEY` (env or
`~/.config/bastion/config.toml`) for the locally running `bastion serve`, matching whatever the
server itself expects — or name the correct existing key if one already exists somewhere this
session did not check.
