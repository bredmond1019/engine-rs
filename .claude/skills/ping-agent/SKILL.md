---
name: ping-agent
description: How to send, receive, and answer a cross-lane message without letting the fast channel replace the durable one — addressing a peer through the BT.6.A registry, composing each of the five message kinds as a strict JSON envelope, verifying a received claim before acting on it, writing every ping to a durable home in addition to sending it, the interrupt discipline that limits which kinds may derail a block in flight, and the four-verdict response contract. Use BEFORE sending a message to a peer lane, whenever a message arrives in an inbox and must be triaged, and BEFORE acting on any claim another lane makes about state you have not checked yourself.
allowed-tools: Bash(mev:*) Bash(git:*) Bash(bash:*) Bash(grep:*) Bash(ls:*) Bash(cat:*)
---

# Sending, receiving, and answering a ping

> **Paths below are relative to the brain root** — the directory containing `brain.toml`, found by
> walking up from wherever you are. This skill is synced into every repo, so a repo-relative link
> would be wrong in most of them.

A ping is a claim. The receiver verifies it locally before acting on it, and the sender writes it
to disk in addition to sending it. Neither half is optional, and this skill is not a role system:
every lane agent does the same job — send, receive, verify, respond — in a different repo. The only
distinct role in the fleet is the commander (BT.6.D covers that half of the conversation).

## Addressing a peer

Address a peer through the **BT.6.A registry**, not a `ListAgents` nickname. `ListAgents` nicknames
are unstable across restart and carry no repo, lane, or roadmap — a lane you addressed five minutes
ago may be a different nickname now, or gone. The registry turns that volatile nickname into a
stable, addressable identity:

```
<lock_dir>/lane-agents/agent-*.json
```

`lock_dir` resolves the same way everywhere in this fleet: `--lock-dir` flag, else `FLEET_LOCK_DIR`
env var, else a `brain.toml` discovered by walking up from cwd, joined with `.fleet-locks`. Each
registry record is a `lane-agent.schema.json` claim — `agent_name` (the current `ListAgents`
nickname), `repo`, `lane`, `roadmap`, `started_at`, `heartbeat`. Read the registry to find who is
driving the lane you need; use `ListAgents` only afterward, to confirm that nickname is still live
before you send. Sending to a nickname you have not just confirmed against the registry is how a
message gets addressed to a lane that already closed.

## Composing: one envelope per kind

Every message is a **strict JSON envelope matching `.claude/workflows/message.schema.json`
field-for-field** — not a prose format. This is deliberate: the eventual engine-rs
structured-output port must be a mechanical translation of an already-typed shape, not a redesign
of a format that was never typed in the first place. This skill does not restate the schema's field
table — read `.claude/workflows/message.schema.json` for the authoritative shape; what follows is
one worked example per kind, written to:

```
<lock_dir>/queue/<repo>/<lane>/inbox/<ts>-<uuid>.json
```

**EDGE_RELEASED** — a dependency edge cleared on the sender's side and a waiting lane may be idling
on it, unsignalled:

```json
{
  "message_id": "b3f1c2a0-2222-4a11-9e77-000000000001",
  "sender": {"agent_name": "engine-rs-5b", "repo": "engine-rs", "lane": "data-contract", "roadmap": "autonomous-foundation"},
  "sent_at": "2026-08-22T14:03:11Z",
  "kind": "EDGE_RELEASED",
  "subject": {"repo": "bastion", "block": "BA.21.A"},
  "body": "engine-rs side of the data-contract dependency is now merged on main; BA.21.A is unblocked.",
  "durable_home": {"channel": "state-edge", "ref": "bastion:BA.21.A:depends_on[0]"}
}
```

**FINDING** — a cross-lane observation worth relaying, the corpus's one measured real-world case
(cited below):

```json
{
  "message_id": "b3f1c2a0-2222-4a11-9e77-000000000002",
  "sender": {"agent_name": "mev-d8", "repo": "mev", "lane": "carryover-sweep", "roadmap": "autonomous-foundation"},
  "sent_at": "2026-08-21T09:41:00Z",
  "kind": "FINDING",
  "subject": {"repo": "base-template"},
  "body": "P0: carryover sweep found 30 of 202 entries are misfiled operator work gating nothing.",
  "durable_home": {"channel": "run-record", "ref": "base-template/planning/orchestration-run/autonomous-foundation/notes.md"}
}
```

**RENDEZVOUS** — a need for two lanes to synchronize before either proceeds:

```json
{
  "message_id": "b3f1c2a0-2222-4a11-9e77-000000000003",
  "sender": {"agent_name": "bastion-c4", "repo": "bastion", "lane": "d62-downstream-check", "roadmap": "autonomous-foundation"},
  "sent_at": "2026-08-22T15:10:05Z",
  "kind": "RENDEZVOUS",
  "subject": {"repo": "bastion"},
  "body": "D62 downstream check against bastion is DEFERRED until this lane goes idle. Ping back when your tree is quiet.",
  "durable_home": {"channel": "lane-log", "ref": "lane-log.jsonl:line-482"}
}
```

**LEASE_RELEASE** — one kind covers both taking and releasing a repo lease, discriminated in the
body:

```json
{
  "message_id": "b3f1c2a0-2222-4a11-9e77-000000000004",
  "sender": {"agent_name": "okf-core-d0", "repo": "okf-core", "lane": "schema-fix", "roadmap": "autonomous-foundation"},
  "sent_at": "2026-08-22T16:00:00Z",
  "kind": "LEASE_RELEASE",
  "subject": {"repo": "okf-core"},
  "body": "RELEASE: exclusive lease on okf-core released, tree clean at commit a1b2c3d.",
  "durable_home": {"channel": "state-edge", "ref": "okf-core:lease-release-2026-08-22"}
}
```

**QUERY** — a read-only question that implies no action by the receiver, kept separate so a question
is never dressed up as a finding:

```json
{
  "message_id": "b3f1c2a0-2222-4a11-9e77-000000000005",
  "sender": {"agent_name": "base-template-b6", "repo": "base-template", "lane": "lane-coordination", "roadmap": "autonomous-foundation"},
  "sent_at": "2026-08-22T16:20:30Z",
  "kind": "QUERY",
  "subject": {"repo": "engine-rs", "block": "ER.9.C"},
  "body": "Is ER.9.C's structured-output port still blocked on the message schema landing, or has that cleared?",
  "durable_home": {"channel": "carryover", "ref": "base-template:query-er9c-status"}
}
```

## Rule 1 — verify before acting

A ping is a claim the receiver verifies **locally** before acting on it, and the verification is
recorded. This is not invented policy: it is what the fleet's only real cross-lane exchange already
did. Verbatim from
`base-template/planning/orchestration-run/autonomous-foundation/notes.md` (2026-08-21): **"Verified
here rather than taken on report"** — the receiver independently re-checked all ten schema
properties, the ticket's `state.json` registration, and `mev lanes --json` before accepting a P0
raised by the mev lane. Without this rule a wrong claim propagates at the speed of the queue: the
next lane that reads the message inherits the error, and nothing in the channel itself catches it.

## Rule 2 — always also to disk

`SendMessage` is ephemeral. An offline or restarted target loses it with no error. So every ping is
additionally written to a **durable home** — not instead of sending, alongside it. There are
exactly three:

1. **Lane log** — `lane-log.jsonl`
2. **`state.json` edge** — a `depends_on` edge or block field
3. **Carryover** — a `carryover[]` entry (routed per the four kinds, see the repo's `CLAUDE.md`)

`/orchestrate` rule 8 already states the durable design: **"each agent reports the state change it
wants; one writer applies them centrally."** A ping does not replace that channel — it accelerates
it, giving the waiting lane a faster signal than polling `state.json` would. The envelope's required
`durable_home` field (`channel` + `ref`) is the mechanical form of this rule: a message with no
`durable_home` is not a valid message, and a received message whose `durable_home` cannot be found
on disk is a claim that has not yet been verified (Rule 1) — do not act on it until it has.

## Rule 3 — interrupt discipline

A ping lands in a session that may be mid-block. **Only `RENDEZVOUS` and `LEASE_RELEASE` may
interrupt a block in flight** — both concern the tree and are objectively time-critical. Every other
kind (`EDGE_RELEASED`, `FINDING`, `QUERY`) is triaged at the **next block boundary**, not before.

Getting this wrong trades one measured failure mode for a worse one. Without the sweep-avoidance
discipline of `/orchestrate` rule 8, cross-lane state gets silently clobbered; without this rule,
the queue trades that sweep problem for a **derailment problem** — a lane that stops mid-block loses
exactly the context that cannot be written down. Check the inbox at boundaries; do not poll it
mid-block for anything but `RENDEZVOUS` or `LEASE_RELEASE`.

## The response contract

Every response is **ACK** plus exactly one of four verdicts:

- **ACCEPTED** — the claim verified and the receiver acted on it
- **VERIFIED-FALSE** — the receiver checked and the claim did not hold
- **DEFERRED** — verification or action is queued for the next block boundary
- **NOT-MINE** — this message's subject is not this lane's concern

Every response **must name where the item was filed** — which lane-log line, which `state.json`
edge, which carryover slug. A response that accepts a claim without naming a durable home is not a
response; it is exactly the unsignalled state Rule 2 exists to prevent.

## What this is not

Not a role system. Every lane agent — sender, receiver, verifier — does the same job in a different
repo. The only distinct role in the fleet is the commander. Two values (lane agent, commander) is
not a role system; name them and stop.

## Out of scope here

- The queue layout and drain mechanics (inbox → processing → done) — BT.6.B owns those.
- Wiring block-boundary triage into `/begin-orchestration` and `/orchestrate` — BT.6.E.
- The commander's half of the conversation — BT.6.D.
