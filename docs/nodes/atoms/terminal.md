---
type: Reference
title: Terminal Atom Nodes
description: The tmux session stack — open, observe, guarded send, bounded await, admission control, held sessions — built on one injectable TerminalDriver seam.
doc_id: nodes-atoms-terminal
layer: [engine]
project: engine-rs
status: active
keywords: [terminal, tmux, terminaldriver, session, admission, held session]
related: [nodes-atoms-index, terminal-nodes, terminal-driver, terminal-crates]
---

# Terminal Atom Nodes

- The tmux stack — every Node here holds an injectable `Arc<dyn TerminalDriver>`, no Node hardcodes
  its own tmux calls → table below
- **Full contract already documented** — [`../../terminal-nodes.md`](../../terminal-nodes.md) and
  [`../../terminal-driver.md`](../../terminal-driver.md) are the authority; this page is the atom
  catalogue, not a duplicate
- One node here (`LiveClaudeSessionNode`) is correctly and explicitly Claude-specific — don't try
  to generalize it, its own doc already explains why

## Nodes

| Node | File | Verdict | Evidence |
|---|---|---|---|
| `TerminalSessionNode` | [`nodes/terminal/session.rs`](../../../crates/engine-core/src/nodes/terminal/session.rs) | **atom** | Driver injected; `dir`/`lease_ttl`/`steal_after` all builders; module doc confirms zero Claude references. Single consumer today (`terminal_probe`) |
| `TerminalObserveNode` | [`nodes/terminal/observe.rs`](../../../crates/engine-core/src/nodes/terminal/observe.rs) | **near-atom** | Driver + session identity injected, but hits a hardcoded module-local `static CLAUDE_MANIFEST`, bypassing the `ManifestSource` seam that `no_match_alarm.rs` already built for exactly this — **[flagged gap](../gaps.md)**. Refactor: take `manifest: Arc<CompiledManifest>` (or `ManifestSource`) as a constructor field, defaulting to the current Claude manifest for behavior-stability |
| `TerminalSendNode` | [`nodes/terminal/send.rs`](../../../crates/engine-core/src/nodes/terminal/send.rs) | **atom** | Driver injected; session identity via configurable `InputBinding`; command/send_id read from `ctx.event`, not literals. Own doc: "zero Claude references." Not yet wired into any `graph.rs` |
| `HoldPolicyNode` | [`nodes/terminal/hold_policy.rs`](../../../crates/engine-core/src/nodes/terminal/hold_policy.rs) | **atom** | `workflow_key` + `PolicyConfigSource` fields, resolves grace/steal-after through the standard 4-layer Policy machinery. No LLM call. Zero graph callers yet but generic by construction |
| `LiveClaudeSessionNode` | [`nodes/terminal/live_claude.rs`](../../../crates/engine-core/src/nodes/terminal/live_claude.rs) | **specific — correctly so** | Hardwires the `claude` CLI binary, Claude-specific flags (`--model`/`--continue`/`--resume`), Claude OTel attributes. `terminal-nodes.md` itself calls this out as the one genuinely Claude-specific terminal node — do not generalize, no second consumer would justify the cost |
| `TerminalAwaitNode` | [`nodes/terminal/await_node.rs`](../../../crates/engine-core/src/nodes/terminal/await_node.rs) | **atom** | Policy-driven timeouts (4-layer + 3 profiles); driver + `CancellationToken` injected; polls a generic `AwaitPredicate` enum (`Marker`/`Detect`/`Regex`/`Silence`/`ExitCode`) — only `Detect` leans Claude-specific. Registered only in an integration test today |
| `HeldSessionNode` | [`nodes/terminal/held_session.rs`](../../../crates/engine-core/src/nodes/terminal/held_session.rs) | **atom** | Policy fully configurable (`lease_ttl_ms`/`renew_interval_ms`); identity derived, not hardcoded; process-global `HeldSessionRegistry` is a real injectable seam. Exactly one production registration (`orchestration`'s `HELD_SESSION` graph) |

## Seams underneath

| Seam | File | Notes |
|---|---|---|
| `TerminalDriver` | see [`../../terminal-driver.md`](../../terminal-driver.md) | The actual swappable tmux seam every node above holds as `Arc<dyn TerminalDriver>` |
| `AdmissionControl` | `nodes/terminal/admission.rs` | Config-driven concurrency gate, reused by `orchestration/gates.rs` and `orchestration/integrate.rs` |
| `ManifestSource` | `nodes/terminal/no_match_alarm.rs` | Hot-reloadable, env-driven — only `no_match_alarm.rs` consumes it today, which is exactly what `TerminalObserveNode` should also use |
| `NoMatchAlarmTracker` | `nodes/terminal/no_match_alarm.rs` | Policy-driven counter, single-module use |
| `HasSessionInput` / `WithSessionInput` | `nodes/terminal/mod.rs` | Blanket-impl builder seam for session identity wiring |
| `PolicyConfigSource` | shared | 4-layer Policy resolution source used by `HoldPolicyNode`/`TerminalAwaitNode` |

## See also

- [`../../terminal-nodes.md`](../../terminal-nodes.md) — the full node-stack contract and invariants
- [`../../terminal-driver.md`](../../terminal-driver.md) — the `TerminalDriver` seam and the fail-closed session lease
- [`../../workflows/terminal-probe.md`](../../workflows/terminal-probe.md) — the one workflow currently using this stack
- [`../gaps.md`](../gaps.md) — the `TerminalObserveNode` manifest-hardcode gap
