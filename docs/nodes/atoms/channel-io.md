---
type: Reference
title: Channel IO Atom Nodes
description: Generic HTTP and outbound-channel delivery primitives other Nodes compose rather than hand-roll their own client — HttpPost, HttpRequestNode, ChannelTransport, the email adapter.
doc_id: nodes-atoms-channel-io
layer: [engine]
project: engine-rs
status: active
keywords: [http, httppost, channeltransport, email, egress, outbound]
related: [nodes-atoms-index, email-adapter, nodes-molecules-research-dispatch-pair]
---

# Channel IO Atom Nodes

- Generic HTTP and channel-delivery primitives other Nodes compose rather than hand-rolling their
  own client → table below
- `ChannelTransport` is the genuinely open multi-channel seam (Slack/Telegram/WhatsApp adapters are
  named as pending, not built) — new outbound channels should implement it, not invent a parallel
  interface
- `EmailChannelTransport` is candidly Resend-only today — the seam is generic, the one concrete
  implementation isn't

## Nodes and seams

| Node / seam | File | Verdict | Evidence | Own doc |
|---|---|---|---|---|
| `HttpPost` seam | [`nodes/http_post.rs`](../../../crates/engine-core/src/nodes/http_post.rs) | seam | Injectable HTTP-POST seam `PersistToBrainNode`/`HarvestApproveNode`/`ActionDispatchNode`/`approve_and_run` all call | — |
| `HttpRequestNode` | [`nodes/http_request.rs`](../../../crates/engine-core/src/nodes/http_request.rs) | **atom** | URL/body/headers/method all via builders; sends through injectable `HttpPost`. Zero in-repo callers yet — own doc names `price-scout` (a different repo) as first consumer, so genericity is by design, not yet proven by a second in-repo caller | — |
| `ChannelTransport` seam | [`nodes/channel_transport.rs`](../../../crates/engine-core/src/nodes/channel_transport.rs) | seam | Injectable egress seam `ActionDispatchNode`/`ResearchIngressDispatchNode`/`orchestration` all call — genuinely open, Slack/Telegram/WhatsApp adapters named as pending | — |
| `EmailChannelTransport` | [`nodes/email/`](../../../crates/engine-core/src/nodes/email/) — `transport.rs`, `inbound.rs`, `webhook_events.rs` | seam (one concrete impl) | Resend-backed outbound send + both inbound webhook paths. Module doc candidly states this is Resend-only — the trait is generic, the implementation isn't | [`../../email-adapter.md`](../../email-adapter.md) |

`MarkerNode` (`nodes/channel_transport.rs`, `#[cfg(test)]`) is a throwaway test fixture hardcoding
its own node name and payload — not exported, excluded from counts.

## See also

- [`../molecules/research-dispatch-pair.md`](../molecules/research-dispatch-pair.md) — `ActionDispatchNode`/`ResearchIngressDispatchNode`, both built on `ChannelTransport`, hand-duplicated
- [`../../operator-payload-contract.md`](../../operator-payload-contract.md) — payload limits per channel (e.g. `WHATSAPP_MAX_REPLY_BUTTONS`)
