---
type: Reference
title: Operator Payload Contract
description: engine-core::operator (EN.8.A) — OperatorPayload/ValidatedOperatorPayload, confirmed WhatsApp interactive-reply limits, and the OperatorChannel declaration a HarvestGate carries
doc_id: operator-payload-contract
layer: [engine]
project: engine-rs
status: active
keywords: [operator, operator-payload, operator-channel, whatsapp, validation, digest, harvest-gate, EN.8.A]
related: [architecture, docs-index, harvest-gate]
---

# Operator Payload Contract (`EN.8.A`)

`crates/engine-core/src/operator/` is the operator-facing payload contract: what reaches the
operator is a validated shape, not a convention, and the channel a workflow sends it over
(`notification` vs. `session-<slug>`) is declared at gate-definition time, never discovered or
degraded at emit time.

## Limits (`operator::limits`)

`OperatorPayloadLimits` (`max_options`, `min_options`, `max_label_chars`, `max_summary_chars`) is
configurable, not hardcoded (CLAUDE.md rule 6). Its `Default` is the confirmed WhatsApp Cloud API
interactive-reply-buttons limits, checked 2026-08-12 against Meta's developer docs:

| Limit | Value |
|---|---|
| Max reply buttons per message | 3 |
| Max button label length | 20 characters |
| Max body text length | 1024 characters |
| Min response options (`OPERATOR_MIN_RESPONSE_OPTIONS`, engine-rs's own floor, not a WhatsApp limit) | 2 |

The list-message fallback (10 rows / 24-char titles) is documented but not used — the contract
targets the tighter reply-buttons shape.

## Payload (`operator::payload`)

`OperatorPayload { gate_id, rendered_summary, options: Vec<OperatorResponseOption>, digest }`.
`rendered_summary` is a required `String` (no "unrendered payload" state). `digest` is computed by
`OperatorPayload::new` over `rendered_summary` + `options` only — `gate_id` is deliberately
excluded, since the digest scopes the rendered artifact, not its source. `recomputed_digest()` /
`digest_matches()` detect a payload mutated after rendering, which is what lets a changed payload
re-queue instead of executing.

## Validation (`operator::validate`)

`validate(payload, &limits) -> Result<ValidatedOperatorPayload, OperatorValidationError>` is the
only constructor for `ValidatedOperatorPayload` — the type the `notification` channel accepts.
There is no other public constructor, so a payload that never validated cannot reach the
`notification` channel regardless of caller. Checks run in fixed order (first violation wins):

1. `MissingRenderedSummary` — empty or whitespace-only summary
2. `RenderedSummaryTooLong { chars, max }`
3. `TooFewOptions { count, min }` / `TooManyOptions { count, max }`
4. `OptionLabelTooLong { key, chars, max }`

Lengths are checked with `.chars().count()`, not byte length.

## Channel (`operator::channel`)

`OperatorChannel` is the two-channel routing declaration (wire form tagged on `kind`):

- `Notification` (default) — a reducible decision that fits `OperatorPayloadLimits`.
- `Session { slug }` (`OperatorChannel::session(slug)`) — an irreducible decision (judgement, a
  credential, drafting, anything open-ended).

It is attached to a gate's *definition*, not decided at emit time. `crate::nodes::harvest_gate::HarvestGate`
now carries a `channel: OperatorChannel` field (default `Notification`), set via
`HarvestGate::with_channel(...)` and read via `HarvestGate::channel()` — readable off the gate
definition without executing the workflow. See [harvest-gate.md](harvest-gate.md) for `HarvestGate`
itself.

## See also

- [harvest-gate.md](harvest-gate.md) — the `HarvestGate` primitive this contract's `OperatorChannel`
  is wired onto (`EN.8.A` task 4).
- [architecture.md](architecture.md) — module map.
