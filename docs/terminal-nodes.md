---
type: Reference
title: The terminal nodes
description: The Phase 9/10 node stack that lets an engine-rs workflow drive a real tmux session — session identity and leases, read-only observation, guarded sends, bounded awaits, the no-match alarm, admission control, and held sessions.
doc_id: terminal-nodes
layer: [engine]
project: engine-rs
status: active
keywords: [terminal nodes, tmux, session lease, await predicate, no-match alarm, admission control, held session]
related: [terminal-driver, terminal-crates, orchestration-workflow, architecture, orphan-recovery]
---

# The terminal nodes

`crates/engine-core/src/nodes/terminal/` — the nodes that let a workflow create, watch and drive a
real tmux session. They sit on `term-core`'s `TerminalDriver` seam (see
[`terminal-driver.md`](terminal-driver.md)) and never touch `term-attach`, which `engine-core`
deliberately does not link (see [`terminal-crates.md`](terminal-crates.md)).

Built across `EN.9.D`/`E`/`F`/`G` and `EN.10.A`.

## The node stack

| Node / module | Block | What it does |
|---|---|---|
| `identity.rs` | EN.9.D | `session_name_for(run_id, node_identity)` — deterministic, `eng-` namespaced, with `:` and `.` sanitized out because tmux treats both as target separators. Plus the per-struct `session_input: InputBinding` builder that threads session identity between nodes. |
| `TerminalSessionNode` (`session.rs`) | EN.9.D | Ensures the session exists, acquires the lease, optionally launches a command and waits for readiness. **Read-only** — no sends. |
| `TerminalObserveNode` (`observe.rs`) | EN.9.D | One `capture_pane`, one `detect`, single shot. Stamps the bounded pane and the resolved policy. Never stamps `cost_usd`; `usage` stays `null`. |
| `pane.rs` | EN.9.D | Pane bounding: 40-line / 8KB caps, redaction **before** hashing, `pane_sha256` and `pane_truncated`. |
| `predicate.rs` | EN.9.E | `AwaitPredicate { Marker, Detect, Regex, Silence, ExitCode }` and its pure evaluation. |
| `TerminalSendNode` (`send.rs`) | EN.9.E | Guarded sends: command floor, lease verified first, per-session mutex, `send_id` back-edge idempotency. |
| `TerminalAwaitNode` (`await_node.rs`) | EN.9.E | Bounded poll over a predicate, its **own** timeout, and a `CancellationToken` `select!`ed on. |
| `manifest_source.rs` | EN.9.F | Runtime detect-manifest override with last-good caching — edit a manifest on disk and the next capture uses it, no rebuild. |
| `no_match_alarm.rs` | EN.9.F | Raises when N consecutive captures classify `Unknown`, naming the active manifest **and its digest**. |
| `admission.rs` | EN.9.F | Semaphore bounding concurrent terminal runs. Over the cap, runs **queue** — they do not fail. |
| `hold_policy.rs` | EN.9.G | Per-workflow policy surface over the `EN.9.B` lease: operator-hold grace and `steal_after`. |
| `held_session.rs` | EN.10.A | One tmux session carried **across node boundaries**, renewing its lease, with external-kill detection. |
| `LiveClaudeSessionNode` (`live_claude.rs`) | EN.10.A / `EN.ticket.otel-pane-telemetry` | Opens an interactive Claude Code session inside the held session mid-run by typing the command into the pane (`TerminalDriver::send_keys`) — never `claude_code_rs::execute`, which is the headless subprocess path `ClaudeCodeStep` uses instead (D4). Visible to `bastion sessions`. Prepends `CLAUDE_CODE_ENABLE_TELEMETRY=1 OTEL_METRICS_EXPORTER=otlp OTEL_RESOURCE_ATTRIBUTES='run_id=...,node.identity=...'` to the launch line so cost telemetry (`claude_code.cost.usage`) correlates to the run without scraping `/usage` (N7); `usage`/`cost_usd` are never stamped by this node. |

## Three rules that look like style and are not

**1. Stamp before anything fallible.** `TerminalSessionNode` writes `@engine_run_id` and
`@engine_created_at` as tmux options *before* any operation that can fail. The runner snapshots
`pre_call_ctx` (`workflow.rs:587`) and restores it wholesale on `Err` (`:599`), so everything a node
wrote to `ctx` is discarded on failure. A tmux option lives outside `ctx` and survives — it is the
only reason a half-created session stays discoverable by `@engine_run_id`, which is what
[`orphan-recovery.md`](orphan-recovery.md)'s boot sweep needs.

**2. The node owns its timeout and its cancellation.** `RunOptions` has no deadline field and
nothing wraps `node.process(ctx).await`. The runner observes cancellation only *between* nodes, so an
abort against a long await returns `202 Accepted` and then does nothing. `TerminalAwaitNode`
therefore takes a `CancellationToken` through its **own** builder and `select!`s it against the poll
and its own timeout.

**3. Never `remove_file` a marker.** The marker contract is `{out}.{nonce}.done`, content equal to
the nonce, with `out`'s mtime postdating the send. Deleting a marker races a concurrent reader and
makes a stale marker indistinguishable from an absent one. `predicate.rs` performs no file IO at all.

## The no-match alarm — why it exists

The claude manifest matches **three literal UI strings**, and Claude Code ships frequently. If a
release reworded "Do you want to proceed?", every session would classify `Unknown`, the Blocked edge
would never fire, no approval would ever surface, and **nothing would error anywhere** — the operator
surface would be silently dead. Manifests are `include_str!`'d at compile time, so the one-word fix
would otherwise need a rebuild *and* a redeploy.

So: `manifest_source.rs` makes the manifest replaceable at runtime, and `no_match_alarm.rs` fires
after N consecutive unmatched captures, naming the manifest **and its digest** — the digest being what
tells an operator whether the manifest they think is deployed is the one actually running. It fires
once per streak and resets on any match.

**Do not weaken this alarm to make a test pass.** It is the only thing standing between a reworded UI
string and a dead operator surface.

## Policy knobs and their defaults

All resolve through the standard four layers (per-run event override > named profile >
`planning/harness.json` > built-in default), ship `baseline` / `cheap-fast` / `thorough` profiles, and
stamp the resolved value into the node's result.

| Knob | Default | Module |
|---|---|---|
| `poll_interval_ms` / `timeout_ms` | `1000` / `600000` (10 min) | `await_node.rs` |
| `consecutive_unmatched_threshold` | `5` | `no_match_alarm.rs` |
| `max_concurrent_terminal_runs` | `8` | `admission.rs` |
| `grace_ms` (operator hold) | `60000` | `hold_policy.rs` |
| `steal_after` | `None` — **fail-closed** | `hold_policy.rs` |
| `lease_ttl_ms` / `renew_interval_ms` | `300000` / `100000` (a third of TTL) | `held_session.rs` |
| `PaneTailPolicy` | `HashOnly` when adopted, `Text` otherwise | `pane.rs` |

`steal_after: None` means an expired-but-present foreign lease is **never** acquired. That is
deliberate: a lease whose owner may still be alive is not free.

## How Claude-specific is this?

Relevant if you want to drive something other than Claude Code in a tmux session:

- **Already generic:** `session.rs` and `send.rs` contain zero Claude references. They create
  sessions and send arbitrary keys.
- **Where the assumption lives:** the *await* side. `predicate.rs` and `await_node.rs` carry Claude
  references because "is it finished?" currently leans on the detect rules. But the predicate enum
  already ships `Marker`, `Regex`, `Silence` and `ExitCode` variants, and `ExitCode`/`Marker` are
  exactly what a non-Claude command would use — no new mechanism needed.
- **Already multi-agent:** the detect manifest system ships **two** manifests (`claude.toml`,
  `pi.toml`), so recognising a second tool is a manifest, not a code change.
- **Genuinely Claude-specific:** `LiveClaudeSessionNode` only.

## What is proven on real hardware

`EN.9.D`'s `TERMINAL_PROBE` ran against the real Mac Mini — session created, pane observed, detect
classification returned, lease released. Evidence:
`planning/EN.9.D/artifacts/mini-probe-run.txt`. `EN.10.A` adds four real-tmux integration tests
(identity across nodes, lease renewal, external-kill bounded error, abandoned-lease reconcile).

Two defects were found by that real run which a 2319-test green suite could not see, both because
`StubTerminalDriver`'s `show_option` default returned a canned `Ok("")` — a state real tmux never
produces for an unset option. Both are fixed
(`EN.ticket.term-core-real-tmux-option-reads`); the durable lesson is in `planning/knowledge.md`.
