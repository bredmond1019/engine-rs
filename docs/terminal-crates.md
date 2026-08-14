---
type: Reference
title: Terminal Crates (term-core / term-attach)
description: What term-core and term-attach hold, why the tmux attach path is a second crate rather than a feature, the tmux_locale_env trap, the include_str! data-file coupling, and the two-repo reversibility constraint.
doc_id: terminal-crates
layer: [engine]
project: engine-rs
status: active
keywords: [tmux, term-core, term-attach, agent-detection, feature unification, reversibility, bastion]
related: [architecture, engine-rs-testing]
---

# Terminal Crates (`term-core` / `term-attach`)

`EN.9.A` ports bastion's tmux session-control and agent-detection code into this workspace as two
new crates, so a future engine-side workflow can drive terminals without ever linking the code
path that attaches a process to a live tty. Neither crate is wired into `engine-core` or
`engine-serve` yet — that is `EN.9.B`. This doc covers the split and its traps; see
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

## Two-repo reversibility

`core/bastion` and `core/engine-rs` are separate git repos with separate remotes, separate
`Cargo.lock`s, unpinned path deps, and no submodules — there is no atomic cross-repo commit. If
this block needs to be reverted, revert **bastion first, then engine-rs**. Bastion's CI checks out
engine-rs `main` on every run, so there is a window — between reverting bastion and reverting
engine-rs — where bastion's `main` does not build against engine-rs's `main`, and bastion CI is red
for that window regardless of which order the two reverts happen in. engine-rs CI does not build
bastion and cannot detect that a revert here broke it. This is an accepted operator trade-off, not
a defect to fix.

## Status as of `EN.9.A`

Neither crate is a dependency of `engine-core` or `engine-serve`. `term-core` builds and tests
standalone (`cargo nextest run -p term-core`); wiring it into a workflow node is `EN.9.B`.
