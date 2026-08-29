---
type: Reference
title: Operator Payload Contract
description: engine-core::operator (EN.8.A/EN.8.B) — OperatorPayload/ValidatedOperatorPayload, confirmed WhatsApp interactive-reply limits, the OperatorChannel declaration a HarvestGate carries, and the operator::queue depth/timeout/storm-suppression mechanics
doc_id: operator-payload-contract
layer: [engine]
project: engine-rs
status: active
keywords: [operator, operator-payload, operator-channel, whatsapp, validation, digest, harvest-gate, operator-queue, run-failure-notification, EN.8.A, EN.8.B]
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
  `open_item(item_id) -> Option<(&OperatorQueueItem, DateTime<Utc>)>` (`EN.8.D` task 6) resolves a
  delivered item's id back to what is currently open, alongside its `opened_at` — the lookup
  [`approve-and-run-workflow.md`](workflows/approve-and-run.md)'s `ApproveAndRunSeams` composes
  into bastion's `PendingLookup` shape. `crates/engine-serve/src/blocked_bridge.rs` (`EN.9.G`) is
  a second `with_level_predicate` consumer: it re-checks a live `LevelSource` (current state ==
  Blocked) on every trigger before delivering into the queue, using a deterministic
  `blocked-edge:<session>` item id so repeated triggers for one session collapse to the queue's
  single open slot rather than needing separate dedup logic.
- `policy.rs` — `OperatorQueuePolicy` (`operator_queue_depth`, `answer_timeout_secs`,
  `suppression_window_secs`, `digest_schedule_secs`) resolves through the standard four policy
  layers (event override > named profile > `planning/harness.json` defaults > built-in default)
  under `WORKFLOW_KEY = "operator_queue"`; `baseline`/`cheap-fast`/`thorough` profiles all hold
  `operator_queue_depth` at 1 and vary only the timeout/suppression/digest knobs. `policy_state()`
  serializes the resolved policy for `RunTelemetry`/`PolicyAggregate` stamping.

## Run-failure notifications (`operator::failure`, `ticket-run-failure-notification`)

`crates/engine-core/src/operator/failure.rs` reuses this contract verbatim to tell the operator
when a run ends terminal-failed, instead of leaving that fact in a log line nobody reads. It adds
no new primitive: `should_notify` is a decision function, `render_failure_payload` is a payload
renderer, and the single hook is `engine-serve/src/live_state.rs::mark_terminal` — the one choke
point every run passes through on its way out of the live map, on every exit path.

**Which statuses notify.** `http.rs::derive_terminal_status` reports four outcomes in precedence
order: `cancelled` -> `budget_halted` -> `failed` -> `succeeded`. The built-in default notifies on
`failed` and `budget_halted`; it does not notify on `cancelled` (the operator caused it themselves)
or `succeeded`. `budget_halted` notifying is a judgement call flagged for reversal in this ticket's
spec Notes: a run stopped by its own cost ceiling is not a crash, but it is exactly the kind of
thing an operator wants to hear about unprompted. Because it is the `notify_on_statuses` policy
knob (see below), reversing it is a config change, not a code change.

**How this differs from an approval gate.** `HarvestGate`/`APPROVE_AND_RUN` items are an approval
set — a tapped response authorizes an execution. A failure notification's response options
(`"acknowledge"`, `"view_run"`) are an **acknowledgement set** — nothing executes on either
response. It exists so the operator knows, not so the operator authorizes.

**Same queue, same depth limit.** A failure notification is an `OperatorQueueItem` like any other
— it goes through the same `OperatorQueue` (`EN.8.B`) and the same policy-resolved open-item depth
limit. A burst of failed runs produces one open item plus a digest tail (`build_digest`/
`storm_digest`), never N separate messages.

**Once per run, guaranteed by the hook site.** `mark_terminal` runs exactly once per run on its way
out of the live map, regardless of how many nodes failed — so wiring the notification there,
rather than at each node-failure site, is what makes "one notification per run" true even when a
run's walk fails three nodes. `render_failure_payload` names only the first failed node
(`suspend.rs`'s existing first-stamped-node convention) plus a count of the rest; it never renders
every failed node into one summary.

**Truncate, never drop.** Unlike an approval gate — which has a safe fallback of routing to a
session — an unreportable run failure has no useful fallback: silence is exactly the failure mode
this ticket exists to close. `render_failure_payload` truncates an over-limit error message
deterministically to fit `OperatorPayloadLimits`, always marking the cut with a trailing
`…[truncated]` marker, rather than failing to notify at all.

**Policy knobs** (`operator::failure::FailureNotifyPolicy`), resolved through the standard four
layers under `harness.json` key `run_failure_notification`:

| Knob | Built-in default | What it controls |
|---|---|---|
| `notify_on_statuses` | `failed`+`budget_halted` on, `cancelled`+`succeeded` off | Which of the four terminal statuses enqueue a notification |
| `failure_item_priority` | `0` | The `effective_priority` a rendered failure item carries into `OperatorQueue` |

Both are set explicitly in all three named profiles (`baseline`, `cheap-fast`, `thorough`); only
`failure_item_priority` varies across them (`-5`/`0`/`+5`) — which statuses notify is not a
cost/latency dial. `policy_state()` stamps the resolved policy into `ctx.nodes` and never emits a
`cost_usd` key.
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
