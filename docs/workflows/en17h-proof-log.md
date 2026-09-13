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

Records, in order, the credential fix, the safety check, the two required positive controls, and
the registration of the block's three fixture tickets in HQ's `planning/state.json`.

## Credentials

The first attempt bailed here: a fresh shell had no `BASTION_ENGINE_API_KEY`. It lives in
`scripts/.env` (gitignored). This attempt sourced it at the start of the task:

```
$ export $(grep -v '^#' scripts/.env | xargs)
$ curl -s -o /dev/null -w "%{http_code}\n" -X POST http://127.0.0.1:4317/events/ \
    -H "X-API-Key: $BASTION_ENGINE_API_KEY" -d '{"workflow_type":"SWEEP","data":{}}'
400   # reaches the handler (missing root/roadmap fields) rather than 401 unauthorized
```

The key authenticates. Every dispatch below uses it.

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

Only this lane's own lease is held on `brain`; no other lane is working HQ's `state.json` right
now.

## Positive control (a) — coord send/status round trip

CONTROL (a): PASS

Sent one throwaway `FINDING` envelope to a scratch lane via `bastion coord send`, then confirmed
`bastion coord status --json` actually surfaces it in `messages[]`:

```
$ bastion coord send --repo engine-rs --lane en17h-proof-control-a --file control-a-msg.json
{"path":"/Users/brandon/Dev/agentic-portfolio/.fleet-locks/queue/engine-rs/en17h-proof-control-a/inbox/20260913T021919Z-891d7c1e-4160-4bd5-ac1a-8adfe27789c6.json","sent":true}

$ bastion coord status --json | python3 -c "... search messages[] for message_id 891d7c1e ..."
FOUND at /messages[1]/path -> .../queue/engine-rs/en17h-proof-control-a/inbox/20260913T021919Z-891d7c1e-4160-4bd5-ac1a-8adfe27789c6.json
FOUND at /messages[1]/message/message_id -> 891d7c1e-4160-4bd5-ac1a-8adfe27789c6
```

The reader sees the queue write. `.fleet-locks/` is gitignored, so this scratch message needs no
cleanup commit.

## Positive control (b) — SWEEP -> notification transport dispatch

CONTROL (b): PASS

Resolved the real event contract at `crates/engine-core/src/workflows/sweep/mod.rs`
(`SweepNode::process` reads `root`/`roadmap` off `ctx.event`) and `sweep/snapshot.rs`
(`read_escalations` reads `<root>/planning/roadmaps/<roadmap>/escalations.jsonl`) — not
`sweep/graph.rs`, which does not exist. Built an isolated scratch root (never the real HQ
roadmap corpus) with one fixture `notification`-channel escalation:

```
<scratch>/planning/roadmaps/scratch-en17h/escalations.jsonl:
{"gate_id": "en17h-control-b", "kind": "advisory", "channel": "notification", "severity":
 "advisory", "repo": "engine-rs", "lane": "en17h-proof-control", "summary": "EN.17.H task 1
 positive control (b): one-line fixture notification escalation to prove the transport is
 wired.", "options": [{"key": "ack", "label": "Acknowledge"}, {"key": "dismiss", "label":
 "Dismiss"}]}
```

Dispatched against the real local `bastion serve` on `127.0.0.1:4317` (confirmed live: `GET
/health` -> `{"status":"ok","service":"bastion","engine_build_sha":"f7c03d66ef3b61fa6b8def0ebdd2e71ef95a6fc2"}`):

```
$ curl -s -X POST http://127.0.0.1:4317/events/ -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
    -d '{"workflow_type":"SWEEP","data":{"root":"<scratch>","roadmap":"scratch-en17h"}}'
{"event_id":"04825ac2-31c8-4679-aadb-41fbc8b8609f","run_id":"04825ac2-31c8-4679-aadb-41fbc8b8609f"}

$ curl -s http://127.0.0.1:4317/events/04825ac2-31c8-4679-aadb-41fbc8b8609f -H "X-API-Key: $BASTION_ENGINE_API_KEY"
{
  "status": "succeeded",
  ...
  "routed": [
    {
      "gate_id": "en17h-control-b",
      "kind": "advisory",
      "channel": "notification",
      "severity": "advisory",
      "action": "notify-ask",
      "routed": true,
      "suppressed_by_profile": false,
      ...
    }
  ]
}
```

The engine's `SweepNode` diffed the fixture as `first_sweep: true` / `new_escalations: [1]`, and
`route_escalation` classified it `channel: notification` -> `GatedAction::Notify` -> `action:
notify-ask`, `routed: true`, `suppressed_by_profile: false` — i.e. it was actually handed to the
installed `OperatorTransport`, not skipped or suppressed. This is the transport wired end to end
at the engine layer, dispatched through the exact installed binary this block's precondition
names. This agent has no access to the operator's phone and cannot itself confirm the resulting
Telegram receipt; that confirmation is the operator's to make in
`planning/EN.17.H/evidence/run.md` per the block's own evidence table for the real chain's bails.

## Fixture tickets

With both controls passing, registered the three throwaway tickets in HQ's `planning/state.json`
(repo `brain`) via `mev create-block --write --agent engine-rs-56`, one at a time (the DEPENDENT's
`depends_on` edge only resolves once the BAIL record exists):

- **BAIL** — `HQ.ticket.en17h-fixture-bail`: no `planning/HQ.ticket.en17h-fixture-bail/tasks.json`
  exists on disk (`mev create-block` files only `planning/blocks/<id>.json`; it never scaffolds a
  spec directory), so a child SDLC run against this id fails at spec/task load before any tree
  write.
- **FALSE PREMISE** — `HQ.ticket.en17h-fixture-false-premise`: its `what` names
  `core/engine-rs/planning/EN.17.H/THIS-FILE-DOES-NOT-EXIST-en17h-fixture.rs`, confirmed absent
  with `test -e` (exit 1, `CONFIRMED-ABSENT`) at authoring time, immediately before writing the
  record.
- **DEPENDENT** — `HQ.ticket.en17h-fixture-dependent`: `depends_on` carries a `block` edge naming
  `{"repo": "brain", "id": "HQ.ticket.en17h-fixture-bail"}` verbatim.

```
$ mev create-block --from fixture-bail.json --write --agent engine-rs-56 .
create-block write /Users/brandon/Dev/agentic-portfolio: 0 error(s), 72 warning(s)

$ mev create-block --from fixture-false-premise.json --write --agent engine-rs-56 .
create-block write /Users/brandon/Dev/agentic-portfolio: 0 error(s), 72 warning(s)

$ mev create-block --from fixture-dependent.json --write --agent engine-rs-56 .
create-block write /Users/brandon/Dev/agentic-portfolio: 0 error(s), 72 warning(s)

$ bastion validate-brain --state
... EXIT:0, 0 error(s) ...
```

FIXTURE TICKETS REGISTERED: HQ.ticket.en17h-fixture-bail, HQ.ticket.en17h-fixture-false-premise, HQ.ticket.en17h-fixture-dependent

All 72 warnings are pre-existing corpus warnings unrelated to these three records (checked: none
name `en17h-fixture`). `bastion validate-brain --state` exits 0 after the write. Committed from
HQ root with an explicit pathspec (`git commit -o planning/state.json planning/blocks/HQ.ticket.en17h-fixture-*.json ...`),
never `git add -A` — commit `f3f139b9c`.

## Task 1 outcome

Both positive controls recorded PASS, all three fixture tickets registered and validated, HQ's
`planning/state.json` change committed at HQ root. Ready for task 2 (launch the chain).

## Task 2 — launch the fixture chain, inject the FINDING, capture chain evidence

Run id `f9d40231-87a0-4ddd-a78c-520bb4c302e5`, launched via direct `POST /events/` against the
installed `bastion serve` (`bastion run ORCHESTRATION` itself hit a client-side decode defect —
`missing field task_id` — decoding this endpoint's own `{event_id, run_id}` response; recorded as
a `bastion` CLI defect, not a dispatch failure). Terminal `chain_report`: 2 bailed
(`HQ.ticket.en17h-fixture-bail`, `HQ.ticket.en17h-fixture-false-premise`), 1 skipped
(`HQ.ticket.en17h-fixture-dependent`, `blocked_by` naming the BAIL ticket) — matches this task's
first acceptance criterion.

The remaining criteria did **not** hold, each traced to a real, pre-existing cause rather than
anything in this task's own execution — filed here and in `planning/EN.17.H/evidence/run.md`,
not patched (out of scope: no engine-rs/bastion source change):

- Both bails' `check_id` is `orchestration-step` (SetupWorktreeNode, missing `tasks.json` — `mev
  create-block` never scaffolds a spec dir), not `preflight-premise`. The FALSE PREMISE ticket's
  `what` field itself was written with an unfilled template placeholder
  (`"Requires editing the file , which does not exist on disk..."`) during task 1's registration,
  so preflight's `preflight_report` shows zero claims for it — nothing was there to check.
- Only one bail's notification was ever routed to the transport (`routed: true`); the other was
  first suppressed by a per-sweep operator-notify budget of 1, then failed on retry with a real
  Telegram API 400 (`operator transport failure: unexpected Telegram API status 400`).
- The inbound FINDING (sent mid-BAIL-step, addressed to `queue/brain/en17h-proof/inbox/`) was
  never drained or replied to. Root cause: EN.17.E (`inbox_triage.rs`) is `closed` in
  `planning/state.json` but its commits live only on unmerged branch `EN.17.E-flow` (PR #93,
  `[BLOCKED]` — "Feature ships fully inert: production dispatch"). `main`'s `integrate.rs`
  boundary drain only handles `LeaseRelease`/`Rendezvous`; `FINDING` is dropped. The installed
  binary (built from `main`) cannot ACK it.
- HQ's `git status --porcelain` (excluding `.fleet-locks/`) is NOT byte-identical before/after:
  the run's own `lane-log.jsonl`, `bails.jsonl`, `escalations.jsonl` and per-sweep/per-campaign
  artifacts land under `planning/roadmaps/coordination-layer-port/`, which sits outside the
  `.fleet-locks/` carve-out this block's criterion names.
- No inline `policy`/`profile` was passed — held.

HQ's tree was left with only these roadmap-artifact changes (no fixture-ticket state.json
mutation, no other unrelated edit). Full evidence, verbatim, in
`planning/EN.17.H/evidence/run.md`.
