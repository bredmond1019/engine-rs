---
type: Reference
title: Terminal Crates (term-core / term-attach)
description: What term-core and term-attach hold, why the tmux attach path is a second crate rather than a feature, the tmux_locale_env trap, the include_str! data-file coupling, and the two-repo reversibility constraint.
doc_id: terminal-crates
layer: [engine]
project: engine-rs
status: active
keywords: [tmux, term-core, term-attach, agent-detection, feature unification, reversibility, bastion]
related: [architecture, engine-rs-testing, terminal-driver]
---

# Terminal Crates (`term-core` / `term-attach`)

`EN.9.A` ports bastion's tmux session-control and agent-detection code into this workspace as two
new crates, so a future engine-side workflow can drive terminals without ever linking the code
path that attaches a process to a live tty. `EN.9.B` adds the async driver seam, capture cache,
session lease, and operator hold on top of `term-core` (see
[terminal-driver.md](terminal-driver.md)). `EN.9.D` wires `term-core` (`tokio` feature) into
`engine-core` as `nodes/terminal/` (`TerminalSessionNode`/`TerminalObserveNode`, registered as the
`TERMINAL_PROBE` workflow); `term-attach` is still linked from neither binary. This doc covers the
split and its traps; see
[architecture.md](architecture.md) for where the crates sit in the module map.

## What each crate holds

**`crates/term-core`** — everything except attach:

- `sessions::tmux` — tmux session listing/parsing, `tmux_locale_env()`, and the new
  `set_option_args`/`show_option_args` builders (net-new in this block, backing the `§5` lease).
- `sessions::model` — session/state types shared by the tmux and detection layers.
- `sessions::claude_state` — Claude-session state tracking.
- `detect/` — agent-detection (`AgentState`, `AgentDetection`), its manifest-driven matcher, and
  the `manifests/{claude,pi}.toml` + `fixtures/{claude_blocked,claude_idle,claude_working,
  pi_idle,pi_working}.txt` data files the golden tests read via `include_str!`.

All fallible functions return `Result<T, TmuxError>` (or another typed error) — no `anyhow`
anywhere in the crate.

**`crates/term-attach`** — `attach_session` and `suspend_and_attach`, and nothing else. It depends
on `term-core` for the shared types but is not depended on by anything else in this workspace.
`attach_session` surfaces tmux's real stderr on failure rather than fabricating a
`"can't find session: {session_name}"` message the way bastion's original did.

## Why `term-attach` is a second crate, not a feature

Cargo features unify **additively** across a build: if any target in the final binary enables a
feature, every target sees it enabled. `bastion` needs the blocking attach path (it runs
interactively with a controlling tty); `engine-core`/`engine-serve` need only the non-blocking
tmux/detection surface, and run as a headless service with no tty at all. Both `bastion ->
term-core` and `bastion -> engine-serve -> engine-core -> term-core` are ordinary
`[dependencies]` edges on the *same* crate in the *same* build graph — no resolver v2/v3 exemption
applies, so cargo builds exactly one `term-core` rlib. Had attach lived behind a feature flag on
`term-core`, whichever binary in the graph turned the feature on would turn it on for everyone,
including `engine-serve`: `attach_session` would end up `pub` and callable from the exact
headless process that has no controlling tty to attach to.

Putting attach in its own crate, `term-attach`, is the only guarantee that survives a future
`bastion -> engine-serve` dependency edge without a human remembering to check a feature flag.
`engine-core` and `engine-serve` simply never depend on `term-attach` — `cargo tree -p engine-core
-i term-attach` and `cargo tree -p engine-serve -i term-attach` both report no match. Do not
collapse this back into a feature gate on `term-core`.

## The `tmux_locale_env()` trap

`tmux_locale_env()` is carried into `term-core` **verbatim**, including its doc comment. It looks
like unnecessary cruft — a hardcoded `LC_ALL=en_US.UTF-8` env override — but it is load-bearing:
on macOS, tmux 3.6b emits a non-tab field separator when the process locale isn't UTF-8-aware,
which silently breaks every `parse_session_line` call downstream. Deleting it "to simplify" turns
into a session-listing bug that only reproduces on macOS with a non-UTF-8 locale set, which is
exactly the kind of failure that is expensive to bisect back to this line.

## The `include_str!` data-file coupling

`detect/golden_tests.rs` loads its fixtures via `include_str!`, which resolves paths at compile
time relative to the source file. `detect/manifests/` and `detect/fixtures/` therefore had to move
together with the Rust source that references them — leaving either behind is a compile failure,
not a runtime one. Any future refactor that reorganizes `detect/`'s directory layout must keep the
data files co-located with the module that `include_str!`s them, or update every path literal in
lockstep.

## The embedded manifests and fixtures are public API, not an implementation detail

`crates/term-core/src/detect/mod.rs` publishes four consts over the bytes it already embeds via
`include_str!`: `CLAUDE_MANIFEST_TOML`, `PI_MANIFEST_TOML`, `CLAUDE_AWAITING_QUESTION_FIXTURE`, and
`CLAUDE_BLOCKED_FIXTURE`. These are not exposed for convenience — they are the only way bastion can
reach this data at all once `BA.18.F` deletes `bastion/src/detect/`. Four bastion files are **not**
part of that extraction and keep reaching into the deleted directory by relative `include_str!`:
`src/serve/status/detect.rs:25` — the production path, compiled into a `OnceLock<CompiledManifest>`
— plus `src/sessions/ask_question.rs:372,416,418,495` for the two shared fixtures. A `pub use` shim
re-exports *items*; it cannot re-export an `include_str!` target, because the macro resolves its
path at the call site's compile time, not at the re-exporting module's. So the shim alone cannot
fix bastion's build, and these bytes have to become real public consts on this crate.

Two alternatives were considered and rejected:

- **A bastion-side copy of `manifests/`.** This reintroduces the exact two-sources-of-truth drift
  that silently dropped the `awaiting_question` rule and produced `EN.ticket.term-core-port-gaps`
  — two copies of the same manifest inevitably diverge, and the divergence fails silently (a
  detection rule just stops firing) rather than as a compile or test error.
- **`include_str!`-ing across the repo boundary.** This would hard-code a sibling-checkout
  filesystem layout (`../term-core/...`) into bastion's build, which breaks the moment bastion and
  engine-rs are not checked out as siblings — a constraint neither repo's CI nor a fresh clone
  guarantees.

**Consequence:** the manifest and fixture bytes are now a cross-repo contract, not private test
data. A change to `manifests/claude.toml`, `manifests/pi.toml`, or either published fixture is a
downstream-visible change to bastion and wants the D62 downstream consumer check (`cargo nextest
run --no-run --locked --manifest-path ../bastion/Cargo.toml`) run alongside it, the same way any
other cross-repo data-contract change in this fleet is verified before landing.

## Two-repo reversibility

`core/bastion` and `core/engine-rs` are separate git repos with separate remotes, separate
`Cargo.lock`s, unpinned path deps, and no submodules — there is no atomic cross-repo commit. If
this block needs to be reverted, revert **bastion first, then engine-rs**. Bastion's CI checks out
engine-rs `main` on every run, so there is a window — between reverting bastion and reverting
engine-rs — where bastion's `main` does not build against engine-rs's `main`, and bastion CI is red
for that window regardless of which order the two reverts happen in. engine-rs CI does not build
bastion and cannot detect that a revert here broke it. This is an accepted operator trade-off, not
a defect to fix.

## Status as of `EN.9.D`

`term-core` (`tokio` feature) is now a real `engine-core` dependency, wired into
`nodes/terminal/` (`TerminalSessionNode`/`TerminalObserveNode`) and registered as the
`TERMINAL_PROBE` builtin workflow in `engine-serve` — see [architecture.md](architecture.md).
`term-attach` is still linked from neither binary. `term-core` also still builds and tests
standalone (`cargo nextest run -p term-core`), including the non-default `tokio` feature's async
driver/lease/hold surface added in `EN.9.B` (see [terminal-driver.md](terminal-driver.md)).
`nodes/terminal/` also now carries `HoldPolicyNode` (`hold_policy.rs`, `EN.9.G`), the per-workflow
policy surface over that lease/hold — it resolves and stamps policy only, and does not itself call
`term_core::lease`/`hold`. `EN.10.A` adds `HeldSessionNode` (`held_session.rs`) — a session held
once per run and reused across node boundaries via a process-global registry, with a background
lease-renewal loop that detects both lease loss and external tmux kill — and
`LiveClaudeSessionNode` (`live_claude.rs`), which types an interactive `claude` CLI invocation
into an already-held pane so the session is visible to `bastion sessions`' tmux listing. See
[architecture.md](architecture.md) for the module-map detail.

## `TmuxError`'s wrapped-error contract

Every public fn in `tmux.rs` returns its error wrapped in `TmuxError::Context { source, .. }` — a
caller matching on variant sees `Context`, never a bare `NoServer`/`NotInstalled`/`ExitError`.
`TmuxError::root_cause()` (`EN.ticket.tmux-error-root-cause`) is the supported way to classify:
it recursively unwraps `Context` down to the innermost non-`Context` variant, returning `self` for
every other variant, with `ExitError`'s `code`/`stderr` still reachable afterward. Match on
`root_cause()`, never on the raw error.

The variant set itself is a cross-repo contract, not a private implementation detail: bastion's
`serve/handlers/sessions.rs` maps `TmuxError` variants to HTTP status codes and error codes as
part of its serve contract (`NotInstalled`/`NoServer` → 503/C001, unknown-session `ExitError` →
404/C002, other → 500/C010). Adding, removing, or renaming a variant is therefore a
downstream-visible change and wants the D62 consumer check
(`cargo nextest run --no-run --locked --manifest-path ../bastion/Cargo.toml`) run alongside it,
the same way the manifest/fixture data contract above is verified before landing.

**The bare-variant test trap.** bastion's tests construct `TmuxError` variants directly by hand
(e.g. `make_tmux_err(TmuxError::NotInstalled)`) rather than by driving a real `Context`-wrapped
error through the actual code path. That lets bastion's status-mapping suite stay green while
still asserting against a shape production no longer produces — a hand-built bare variant proves
nothing about what a real caller receives once every public fn wraps its result. A test that
constructs the error value it then matches on can pass even when the mapping it's meant to guard
has silently regressed.
