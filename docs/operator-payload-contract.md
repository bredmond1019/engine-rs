---
type: Reference
title: Operator Payload Contract
description: engine-core::operator (EN.8.A/EN.8.B) — OperatorPayload/ValidatedOperatorPayload, confirmed WhatsApp interactive-reply limits, the OperatorChannel declaration a HarvestGate carries, and the operator::queue depth/timeout/storm-suppression mechanics
doc_id: operator-payload-contract
layer: [engine]
project: engine-rs
status: active
keywords: [operator, operator-payload, operator-channel, whatsapp, validation, digest, harvest-gate, operator-queue, EN.8.A, EN.8.B]
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

## Queue (`operator::queue`, `EN.8.B`)

`crates/engine-core/src/operator/queue/` turns pending blocked-edge state into an ordered,
depth-limited operator queue:

- `item.rs` — `OperatorQueueItem { payload, item_id, effective_priority, enqueued_at, source }`
  and `compare_items`, a total, deterministic comparator (priority descending, `enqueued_at`
  ascending, `item_id` ascending as the final tiebreak) — no I/O, no clock reads.
- `source.rs` — the injectable `QueueSource` trait plus `BlockedEdgeSource`, a file-backed reader
  of bastion's blocked-edge sink JSONL (`default_sink_path` resolves the same file bastion writes;
  a missing file yields an empty queue, malformed lines are skipped, no handle is held open). Its
  `pending()` returns lightweight `PendingBlockedEdge { session, host, to, observed_at }` records —
  turning one into a full `OperatorQueueItem` (`item_id`/`effective_priority`) is `OperatorQueue`'s
  job, not the source's.
- `mod.rs` — `OperatorQueue` enforces a policy-resolved open-item depth limit (built-in default 1,
  the §7.5 Invariant-3 floor), releases the open item back to the queue on `answer()` or on an
  unanswered `answer_timeout_secs` timeout (re-queued, never dropped), and drops items whose level
  predicate no longer holds at selection time via an injectable `with_level_predicate` closure.
- `policy.rs` — `OperatorQueuePolicy` (`operator_queue_depth`, `answer_timeout_secs`,
  `suppression_window_secs`, `digest_schedule_secs`) resolves through the standard four policy
  layers (event override > named profile > `planning/harness.json` defaults > built-in default)
  under `WORKFLOW_KEY = "operator_queue"`; `baseline`/`cheap-fast`/`thorough` profiles all hold
  `operator_queue_depth` at 1 and vary only the timeout/suppression/digest knobs. `policy_state()`
  serializes the resolved policy for `RunTelemetry`/`PolicyAggregate` stamping.
- `digest.rs` — `build_digest`/`storm_digest`, pure functions producing a top-item-plus-count
  `QueueDigest`. `storm_digest` is `build_digest` narrowed to items enqueued within
  `suppression_window_secs` of `now` (a non-positive window clamps to zero rather than being
  treated as unbounded; items with a future `enqueued_at` are excluded as clock-skew guards). The
  same `QueueDigest` shape backs both storm suppression and the scheduled digest tail
  (`digest_schedule_secs`) — one JSON shape, no duplicated digest logic.

See `planning/harness.json`'s `operator_queue` section for the policy defaults and named profiles.

## See also

- [approval-ledger.md](approval-ledger.md) — `EN.8.C`'s record of every decision taken against this
  contract: the digest a row is bound to is the one computed here, and a digest mismatch is recorded
  as a re-queue rather than an approval.
- [harvest-gate.md](harvest-gate.md) — the `HarvestGate` primitive this contract's `OperatorChannel`
  is wired onto (`EN.8.A` task 4).
- [architecture.md](architecture.md) — module map.
