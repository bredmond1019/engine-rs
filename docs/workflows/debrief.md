---
type: Reference
title: DEBRIEF
description: The single-node workflow that renders a morning brief from one campaign's journal, dispatches it downstream, and writes it back as a durable journal row.
doc_id: debrief-workflow
layer: [engine]
project: engine-rs
status: active
keywords: [debrief, journal, brief, campaign, morning brief, DEBRIEF, POST_DRAFT]
related: [workflows-readme, orchestration-workflow, architecture]
---

# `DEBRIEF`

Turns one campaign's [journal](../architecture.md) — the durable decision log an `ORCHESTRATION`
run writes as it goes — into a single readable text brief: every step, in order, every bail named
with its reason. Point it at a campaign id and it hands back what happened overnight, readable on
a phone.

## Quickstart

```bash
curl -X POST $ENGINE/events/ \
  -H "X-API-Key: $ENGINE_EVENTS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"workflow_type":"DEBRIEF","data":"<campaign-uuid>"}'
```

The campaign id is the **only** input. `data` may be the bare UUID string above, or an object with
a `campaign_id` field (`{"data":{"campaign_id":"<campaign-uuid>"}}`) — same two-shape convention as
`RECALL`'s query field. No conductor, chain, roadmap, or lane is involved; `DEBRIEF` is dispatchable
on its own.

| Must exist first | Why |
|---|---|
| A campaign with journal rows | `DEBRIEF` reads `GET /campaigns/{id}/journal`'s underlying data via an injected `JournalReader`; an empty or unknown campaign renders an empty brief, not an error |
| `DATABASE_URL` configured on the serving process | With no pool, the live `JournalReader` self-skips to an empty row set (mirrors the durable write path's own no-DB self-skip) rather than erroring |

Read the rendered brief back the same way you'd read any journal entry:

```bash
curl $ENGINE/campaigns/<campaign-uuid>/journal -H "X-API-Key: $ENGINE_EVENTS_API_KEY"
```

Look for rows with `kind: "DebriefRendered"` — their detail payload holds the text. A completed
run writes **up to two** such rows (see step 3 below): one always, one only when the campaign's
rows clear a quality bar — filter on `detail.step` (`"DebriefNode"` for the ops digest,
`"PostDraft"` for the publishable draft) to tell them apart.

## How it works

One node, both start and terminal, producing up to two outputs:

```mermaid
flowchart LR
    A["DebriefNode<br/>read journal rows<br/>render brief + draft<br/>dispatch + write back"]
```

1. Resolve the campaign id out of `ctx.event`.
2. Fetch that campaign's journal rows through the injectable `JournalReader` trait (`engine-core`
   depends only on `engine-contract`, so it cannot call `engine_store` directly — the same reason
   `HttpGet`/`HttpPost` are injectable rather than direct calls; see
   [`../architecture.md`](../architecture.md) § Injectable Seams).
3. Render one deterministic text digest, steps in `created_at` order, every `StepBailed` /
   `GateRefused` / `StateWriteVerificationFailed` / `BudgetHalted` row naming its reason in the
   text — this is enforced in code (`brief_names_every_bail`), not left to a summarizing model.
   **Separately**, attempt a second, publishable output — a `POST_DRAFT` — from the same rows; it
   is produced only when the rows clear a quality bar, and is refused (not emptied) otherwise. See
   [orchestration.md, § "`DEBRIEF`'s two outputs"](orchestration.md) for the bar, the refusal
   rule, and where a cleared draft lands.
4. Dispatch each produced output (the ops digest always; the draft only when produced) to
   `CONTENT_PIPELINE` over the existing `ChannelTransport` seam, for downstream delivery. This leg
   is **fire-and-forget** — every `OutboundBody::TriggerWorkflow` dispatch in this codebase is (see
   [`../architecture.md`](../architecture.md) § Injectable Seams for `ChannelTransport`) — so
   `DebriefNode` never waits on it and never reads a result back from it.
5. **Separately, synchronously**, write each produced output back as its own
   `JournalDecisionKind::DebriefRendered` journal row (`step: "DebriefNode"` for the ops digest,
   `step: "PostDraft"` for the draft) through the injected journal-sink seam. This is why each
   reads back through `GET /campaigns/{id}/journal` reliably: the text a reader gets is the text
   this node itself produced, not whatever the fire-and-forget `CONTENT_PIPELINE` run does with it.

The production wiring (`register_debrief`, `crates/engine-serve/src/workflows.rs`) uses the live
`JournalReader` (`journal::journal_reader_live`) and journal-sink (`journal::journal_sink_live`),
plus the live `ChannelTransport`. `register_debrief` itself — the call that registers the
`Dispatcher` factory — still runs before the serving process's Postgres pool exists (the same gap
`ORCHESTRATION`'s own `StepFanoutContext` documents), but the *closure body* resolves the reader's
pool and the sink fresh on every dispatch, from a process-global `DurableHandle` cell
(`journal::set_journal_durable_handle`/`journal_durable_handle`) rather than at registration time —
so once something has installed a handle (e.g. `bastion`'s `serve/mod.rs`, out of this repo), both
the read and the synchronous write-back are live for that and every later dispatch. With no handle
installed, the reader self-skips to an empty campaign and the sink drops the row, exactly as before
this wiring — never an error. See [`../architecture.md`](../architecture.md) for the seam's full
detail.

## No policy, no profiles

`DebriefNode` calls no model — it renders from journal data deterministically, so there is nothing
for a policy layer to act on. Same no-op carve-out as `TERMINAL_PROBE` and `RECALL`.

## See also

- [README.md](README.md) — the capability catalogue.
- [`../architecture.md`](../architecture.md) — the Journal section (`JournalDecisionKind`, the
  read route) and the `DEBRIEF` module entry.
- [orchestration.md](orchestration.md) — how a campaign and its journal rows come to exist in the
  first place.
