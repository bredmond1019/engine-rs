---
type: Reference
title: Email Adapter (EN.6.B)
description: The Resend-backed email channel — outbound EmailChannelTransport, inbound-mail and delivery/bounce webhooks, tag-echo opportunity correlation, and why there is no policy surface here
doc_id: email-adapter
layer: [engine]
project: engine-rs
status: active
keywords: [email, resend, channel transport, inbound webhook, bounce, opportunity action, ingress envelope, tag echo]
related: [content-pipeline-workflow, opportunity-edit-workflows, en-6-a-egress-dispatch-tasks, en-6-b-email-adapter-tasks]
---

# Email Adapter (`EN.6.B`)

The email channel has two halves, both wired around Resend:

- **Outbound** — [`EmailChannelTransport`](../crates/engine-core/src/nodes/email/transport.rs),
  a `ChannelTransport` (`EN.6.A`) impl that sends through the Resend HTTP API.
- **Inbound** — two `engine-serve` webhook routes that turn Resend payloads into engine
  dispatches: one starts a `CONTENT_PIPELINE` run from an incoming message, the other appends a
  delivery/bounce `Action` onto an opportunity via `EN.7.B`.

Neither half embeds, retrieves, or scans the Brain corpus. Per **THE BOUNDARY TEST** (`CLAUDE.md`,
governed by brain D51/D53), a bounce becomes an `Action` only by dispatching `EN.7.B`'s
`OPPORTUNITY_ADD_ACTION` workflow — never by a direct corpus write, an address-book lookup, or an
index scan. engine-rs acquires and reasons; Synapse (if this data should ever reach the Brain)
would own the ingest seam, which this module never touches.

## Outbound: `EmailChannelTransport`

`crates/engine-core/src/nodes/email/transport.rs` implements
`crate::nodes::channel_transport::ChannelTransport` for `ChannelType::Email`, patterned directly
on `WorkflowTriggerDispatch`: it sends over the same injectable `HttpPost` seam
(`post_with_headers`) so tests inject a `StubHttpPost` and assert the exact outbound payload with
no live network call.

**Construction / builders**

```rust
EmailChannelTransport::new()                 // reads RESEND_API_KEY + ENGINE_EMAIL_FROM at construction
    .with_api_key("...")                     // override the API key (tests only — never a real key)
    .with_sender("...")                      // override the `from` address
    .with_http_post(stub);                   // inject a StubHttpPost
```

`EmailChannelTransport::new()` is also what `LiveChannelTransport`'s `channel_type` router
constructs for `ChannelType::Email`, so a `ChannelType::Email` action sent through
`channel_transport_live` routes here rather than to `UnwiredChannelTransport` — whose
`unwired_channel_error` attributions this block also corrected to name `EN.6.C` for Slack and
`EN.6.D` for Telegram/WhatsApp (the pre-`EN.6.B` message had them inverted).

**Body-kind mapping** — `OutboundBody::Message { text }` maps to Resend `text`;
`OutboundBody::Digest { markdown, html }` maps `html` to Resend `html` when present, otherwise
falls back to `markdown` as `text`. `OutboundBody::TriggerWorkflow` is rejected with an `Err`
before any request is issued — email is not a workflow-trigger channel.

**`ReplyContext` threading** — `reply_context.channel_token` (falling back to
`conversation_id`) resolves the recipient (`to`); a present `reply_context.thread_id` becomes
both `In-Reply-To` and `References` headers on the Resend payload, so a reply threads into the
same mail conversation.

**Sender address** — defaults to `bastion@mail.bastiel.com.br`
(`transport::DEFAULT_EMAIL_FROM`), the verified `mail.bastiel.com.br` domain (brain-side infra
`HQ.3.B`). Overridable via `ENGINE_EMAIL_FROM` or `.with_sender(..)`.

## Environment variables

| Var | Read by | Purpose |
|---|---|---|
| `RESEND_API_KEY` | `EmailChannelTransport::new()` | The Resend API key, sent as `Authorization: Bearer <key>`. **Never** a literal in source, a test fixture, or `harness.json` — read only from the environment. |
| `ENGINE_EMAIL_FROM` | `EmailChannelTransport::new()` | Overrides the `from` sender address; falls back to `DEFAULT_EMAIL_FROM` (`bastion@mail.bastiel.com.br`) when unset. |

**Credentials-only-from-the-environment rule.** `RESEND_API_KEY` is read once, at construction
time, and is empty (never a panic) if the env var is unset. `send()` then checks the key at
send time: an empty/unset key produces a descriptive `Err` naming `RESEND_API_KEY` rather than a
silent no-op or a request carrying an empty `Authorization` header — so a misconfigured
deployment fails loudly instead of leaking an unauthenticated request or masking the gap.

## Tag-echo opportunity correlation

Bounce/delivery correlation is a **tag echo on the send** — no address index, no corpus scan:

1. A caller that wants a bounce/delivery event correlated to an opportunity attaches
   `metadata["opportunity_slug"]` on the `OutboundAction` (`EN.6.A`'s `metadata` field, added
   additively by this block — an action with no metadata serializes byte-identically to the
   pre-`EN.6.B` three-field shape).
2. `EmailChannelTransport::send` echoes that entry onto the Resend payload as a `tags` entry
   (`[{"name": "opportunity_slug", "value": "<slug>"}]`); an action with no
   `opportunity_slug` metadata produces no `tags` key at all.
3. Resend echoes tags back on its delivery/bounce webhook payload (`payload["data"]["tags"]`,
   either the array-of-objects shape or a flat `{name: value}` object — both are accepted).
4. `crates/engine-core/src/nodes/email/webhook_events.rs`'s `map_delivery_event` reads the slug
   back off the echoed tags and maps the event to `EN.7.B`'s `AddOpportunityActionEvent{slug, at,
   kind, note}`.

The **sender** that actually populates `metadata["opportunity_slug"]` on an outbound action is
`EN.6.H2` — this block only plumbs and tests the field end to end; no caller in this repo sets it
yet.

## Webhook routes

Both routes live in `crates/engine-serve/src/email_webhooks.rs`, are registered in
`crates/engine-serve/src/http.rs::configure()`, and gate on `X-API-Key` via the same
`check_api_key` every other mutating route in this repo uses.

### `POST /webhooks/email/inbound`

Parses a Resend inbound-mail payload (`engine_core::nodes::email::parse_inbound_email`) into an
`IngressEnvelope{channel_type: Email, source: ChannelMessage{..}}` and dispatches exactly one
`CONTENT_PIPELINE` run carrying `{envelope}` (deserializing as `ContentPipelineInput`).

- `sender_id` from `payload["from"]`; `reply_context.thread_id` from the message id
  (`message_id`, then `headers["Message-ID"]`, then `in_reply_to`); `timestamp` from
  `payload["created_at"]` when present, else `Utc::now()` in RFC 3339 (the only clock read on
  this path); `raw_payload` is the untouched input.
- `envelope_id` is **deterministic**: `email:<message_id>` when a message id is present,
  otherwise a stable UUIDv5 derived from `(from, subject, body)`. Parsing the same payload twice
  yields the same `envelope_id`; `Uuid::new_v4()` appears nowhere on this path.
- **Responses:** `401` without a valid `X-API-Key` (nothing parsed, nothing dispatched). `400`
  with a descriptive `message` for a malformed payload (missing `from`, or neither `text` nor
  `html` present) — nothing dispatched. Otherwise `202 {run_id, event_id, envelope_id}`
  (`event_id` always equals `run_id`, matching `POST /events/`'s contract).

### `POST /webhooks/email/events`

Maps a Resend delivery/bounce webhook payload (`engine_core::nodes::email::map_delivery_event`)
onto `EN.7.B`'s `OPPORTUNITY_ADD_ACTION` workflow.

- Handled `type` values: `email.delivered` -> kind `email-delivered`, `email.bounced` -> kind
  `email-bounced`, `email.complained` -> kind `email-complained`. `at` comes from
  `payload["created_at"]` (falling back to `payload["data"]["created_at"]`) — never a clock
  read, so a redelivered webhook maps to the identical `{at, kind, note}` triple and
  `plan_add_action`'s idempotency absorbs the duplicate.
- **Responses:** `401` without a valid `X-API-Key`. `400` for a structurally malformed payload
  (missing `type`, or a handled type missing `created_at`). An event with **no**
  `opportunity_slug` tag, or an unrecognized `type`, is accepted and explicitly skipped —
  `202 {skipped: true, reason}` (`"no_opportunity_slug_tag"` or `"unhandled_event_type"`) —
  never a 500, never a guessed slug. A correlated event dispatches exactly one
  `OPPORTUNITY_ADD_ACTION` run and returns `202 {run_id, event_id}`.

## Auth gate and the deferred Svix signature

Both routes reuse this repo's existing `check_api_key` (`X-API-Key`) gate, for consistency with
every other mutating route rather than inventing a second auth mechanism for email. Resend signs
its own webhooks with [Svix](https://www.svix.com/) signatures, not a static header, so the
deployment contract is that these routes sit behind a shim or tunnel that supplies
`X-API-Key`. **Stated rather than silently assumed:** Svix signature verification is out of
scope for this block — a deliberate, tracked follow-up tightening, alongside the retry/queue
durability `EN.6.A` also deferred.

## Why there is no policy surface here

Per `CLAUDE.md` standing rule 6, a `Policy` surface (`baseline`/`cheap-fast`/`thorough` bundles,
four-layer resolution) exists for knobs a run could reasonably want set differently for cost,
speed, or quality reasons. `EN.6.B` adds a *transport adapter*, not a workflow, and has no such
knob:

- `RESEND_API_KEY` is a credential, not a policy choice.
- The sender address and the webhook routes' base are deployment topology (which mailbox, which
  host), not something a single run dials per-invocation.
- The Resend API URL and its request/response shape are fixed by an external contract (Resend's
  own API), not a lever this repo controls.

Configuration therefore arrives from the environment plus constructor overrides
(`with_http_post` / `with_sender` / `with_api_key`), exactly as `WorkflowTriggerDispatch` already
does — and `planning/harness.json` documents these env vars in a comment rather than declaring an
`email` policy section. If a later block gives email sends a real quality/cost dial (for example,
plain-text vs. HTML digest rendering), that dial belongs on the *calling workflow's* policy, not
here.

## Live-send verification

The live Resend call is exercised by exactly one `#[ignore]`d, env-gated test
(`transport::tests::live_send`) so the hermetic suite stays green — and dispatches no live
network call — with `RESEND_API_KEY` unset:

```bash
RESEND_API_KEY=... EMAIL_TEST_TO=you@example.com \
  cargo nextest run -p engine-core --run-ignored all live_send
```

See `planning/EN.6.B-email-adapter/tasks.md`'s Notes for the one-time manual verification record.
