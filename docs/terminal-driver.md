---
type: Reference
title: Terminal Driver, Lease, and Hold
description: The TerminalDriver seam, the fail-closed session lease's read-back arbitration, the operator-hold read/write asymmetry, and the C-u recovery term-core ships for EN.9.B.
doc_id: terminal-driver
layer: [engine]
project: engine-rs
status: active
keywords: [term-core, tmux, terminal-driver, session-lease, operator-hold, tokio, async-mirror, C-u-recovery]
related: [terminal-crates, architecture]
---

# Terminal Driver, Lease, and Hold

`EN.9.B` wires `term-core` (`EN.9.A`) into an async-capable driver seam, plus the advisory,
fail-closed session lease and the operator hold every terminal node must go through before
acting on a real tmux session. All of it lives behind `term-core`'s non-default `tokio` feature —
`bastion` still links the crate blocking-only and pays nothing for any of this.

## Why the async mirror is a requirement, not a preference

Engine runs are `spawn_local`'d onto the actix worker that accepted their POST, and
`node_context` carries no `web::block` wrapper the way a sync-tolerant handler would. A blocking
`tmux` invocation on that worker therefore does not just stall the one run driving it — it stalls
every other run co-resident on that worker, and the HTTP/WS surface those runs' clients are
polling. `run_tmux_async` (`crates/term-core/src/tmux.rs`) exists so a terminal node can `.await`
tmux instead of blocking a shared executor thread.

The blocking and async paths share one classification function, `classify_output`
(`tmux.rs`), so the two never drift on what counts as success, "no server", or a non-zero exit.
`run_tmux_async` wraps the child's `.output()` in `tokio::time::timeout`; on elapsed it kills the
child (never leaks it) and returns `TmuxError::Timeout { action, after }` rather than hanging the
awaiting node indefinitely.

## The `TerminalDriver` seam

`crates/term-core/src/driver.rs` follows the same injectable-seam shape as
`crates/engine-core/src/nodes/http_post.rs`'s `HttpPost` trait (`EN.4.C`): a `Send + Sync`
trait — `list_sessions`, `capture_pane`, `new_session`, `kill_session`, `send_keys`,
`send_named_key`, `set_option`, `show_option` — held as `Arc<dyn TerminalDriver>` so production
code and tests can swap implementations behind one type.

- **`TmuxDriver`** (live) delegates every call to the pure `*_args` builders already in `tmux.rs`
  plus `run_tmux_async`. It constructs no argv of its own.
- **`StubTerminalDriver`** (recording) mirrors `StubHttpPost`: an `Arc<Mutex<..>>` of every argv
  sequence it received, plus per-operation configurable responses, so node tests assert on
  outbound calls and failure handling without ever spawning a real tmux process.

## The capture cache

`crates/term-core/src/capture_cache.rs` sits in front of `TmuxDriver::capture_pane` with a
`DEFAULT_CAPTURE_TTL` of 400ms (named constant, constructor override via `with_ttl`). It exists
so the hub's 2s sweep and a node's own await loop, both reading the same session around the same
moment, collapse onto one tmux invocation instead of two. Concurrent cold callers for the same
session single-flight onto one underlying capture. `StubTerminalDriver` stays uncached — node
tests need to observe every call, not a coalesced one.

## The session lease — why read-back, not check-then-set

tmux has no compare-and-swap over user-options: `set-option` always succeeds and always
overwrites whatever was there. A naive "read, see it's empty, then write" is a classic TOCTOU —
two concurrent acquirers can both observe an empty option and both write. The only way to know
whether a write actually won is to **write, then re-read, and confirm the value read back is the
one just written** (by nonce). `crates/term-core/src/lease.rs`'s `SessionLease::acquire` does
exactly this against the `@engine_lease` tmux user-option; nothing in this module may skip the
read-back and substitute a pre-write check.

The lease value is `<run_id>:<nonce>:<identity>:<expires_at_ms>`, a pure string with its own
parser (`Lease::parse`) and round-trip tests. A malformed value is treated as **not held by
us** — never guessed at, never partially accepted.

**Fail-closed default.** An expired lease is stealable only past an explicit `steal_after` bound
supplied by the caller. With `steal_after` unset, an expired lease is never acquired — the safe
failure mode here is "nobody can act," not "anybody can." A losing acquirer backs off (bounded,
jittered around `DEFAULT_BACKOFF`) rather than spinning or stealing.

## The operator hold — the read/write asymmetry

`crates/term-core/src/hold.rs` resolves two hold signals, in precedence order:

1. `@operator_hold` — a tmux user-option written by managed attach paths the moment they take a
   pane. Authoritative when present.
2. `#{session_attached}` (via `tmux display-message -p`, `display_message_args`) — the fallback
   for a raw `tmux attach` that no managed path ever saw, so a hold is never silently missed just
   because nothing wrote the option.

Both signals are truthy flags rather than a tri-state, so the decision is computed as an OR:
either signal being true holds.

**The asymmetry is the point, and it is enforced in exactly one place.** A hold pauses SENDS and
never pauses READS — `capture_pane` stays live under a hold. `OperatorHold` exposes no read-side
guard at all; that absence *is* the "reads continue" property. Only `guard_send` can return
`HoldError::Paused`.

Detaching does not immediately resume sends: once a session has been observed attached, sends
stay paused for a grace window (`DEFAULT_DETACH_GRACE`, 60s) after the last attached observation.
The grace clock is threaded through every call as an explicit `now_ms`, the same pattern
`lease.rs` uses, so tests exercise the 60s boundary against an injected clock rather than
sleeping real time.

**The lease is retained, not released, for the duration of a hold.** `hold.rs` never touches
`@engine_lease` in either direction — it holds no reference to a `SessionLease` at all, so the
invariant holds by construction. The human attached to the pane is the participant that matters;
losing the lease mid-attach would let another run step in underneath them. A caller integrating
both (the guarded sender below) keeps renewing the lease through a hold rather than releasing it.

Grace and `steal_after`-style tuning ship here as named defaults with call-site overrides, **not**
as a per-workflow `Policy` surface. That boundary is `EN.9.G`, built over this Phase-1 block —
see `EN.9.B`'s block record for the rationale.

## The per-session send mutex and the `C-u` recovery

`send_keys` is two tmux invocations under the hood — the literal text, then Enter
(`tmux.rs:321-329`). Two runs interleaving between them concatenates their input into one line at
the prompt. `GuardedSender` (`driver.rs`) wraps `TerminalDriver` with a `tokio::Mutex` keyed **per
session name** — never one global lock, which would serialize unrelated sessions against each
other — held across the full literal+Enter(+recovery) sequence. Every send path goes through
`GuardedSender`, which also verifies the lease (renewing it) and the hold before acting; nothing
should call `TerminalDriver::send_keys` directly on a driver a node holds.

If the literal send succeeds and the Enter send fails, the pane is left holding a half-typed
line. `GuardedSender` recovers by sending the `C-u` line-clear key before returning, and it
surfaces the **original** send error, not the recovery's — the caller needs to know what actually
failed. If the `C-u` recovery itself fails, that is folded into the returned error rather than
swallowed; a silently half-typed prompt at a human's terminal is the worst outcome this code can
produce.

## Verified live

**This section was executed against a real tmux, not authored from the design above.** Recorded
by `EN.9.B` task 8, run on the Mini's installed tmux.

- **tmux version:** `tmux 3.7b`
- **Date:** 2026-08-17 (UTC)

Recipe:

```bash
# 1. Create a detached session.
tmux new-session -d -s en9b-live-test -x 80 -y 24

# 2. Read #{session_attached} while nothing is attached.
tmux display-message -p -t en9b-live-test '#{session_attached}'

# 3. Attach a second client via a background pty (a nested tmux session
#    running `tmux attach` in its pane, with $TMUX unset so it treats the
#    target as a foreign server rather than refusing to nest):
tmux new-session -d -s en9b-attacher "env -u TMUX tmux attach -t en9b-live-test"

# 4. Re-read while attached.
tmux display-message -p -t en9b-live-test '#{session_attached}'

# 5. Detach (kill the attacher client) and re-read.
tmux kill-session -t en9b-attacher
tmux display-message -p -t en9b-live-test '#{session_attached}'
```

Verbatim captured output (`xxd` of each `display-message -p` call's stdout, showing the exact
bytes including the trailing newline tmux always emits):

| Step | Raw bytes | Meaning |
|---|---|---|
| Detached (before attach) | `30 0a` → `"0\n"` | not attached |
| Attached (second client connected) | `31 0a` → `"1\n"` | attached |
| Detached again (after `kill-session` on the attaching client) | `30 0a` → `"0\n"` | not attached |

`tmux list-clients -t en9b-live-test` during step 4 confirmed one real client attached
(`/dev/ttys013: en9b-live-test [80x24 tmux-256color] (attached,focused,UTF-8)`), and reported no
clients before/after — the pty-backed nested-attach trick genuinely connects a client, it is not
just `@operator_hold`-shaped bookkeeping.

**No contradiction with task 5's design.** `parse_session_attached` in `hold.rs` already treats
the bare digit with a trailing newline as the wire format (`raw.trim() == "1"`), which is exactly
what tmux emits here — `"0\n"` and `"1\n"`, never an unadorned `"0"`/`"1"` or anything else. The
exact captured strings are pinned as a golden test in `hold.rs`
(`session_attached_parses_the_captured_live_strings`) so a future tmux that rewords this output
fails a test instead of silently classifying every session as unattached.
