---
type: Guide
title: Testing engine-rs
description: How this workspace's tests are laid out (one integration-test binary), which commands to run, and why — with the measured numbers behind the layout.
doc_id: engine-rs-testing
layer: [engine]
project: engine-rs
status: active
keywords: [testing, nextest, integration tests, link time, cargo, harness]
related: [architecture, base-template:rust-sdlc-iteration-speed, brain:d57-rust-sdlc-iteration-speed]
---

# Testing engine-rs

## Commands

```bash
cargo nextest run -p engine-core <module::path>   # mid-task: just what you touched
cargo nextest run --lib --workspace               # fast signal: all 1044+ unit tests, ~2s warm
cargo nextest run --workspace                     # authoritative: unit + integration, ~3s warm
cargo fmt --check && cargo clippy -- -D warnings  # the other two gates
cargo build --release                             # the fourth gate
```

**Never plain `cargo test`.** It is a numbered standing rule in `CLAUDE.md` and a `PreToolUse` hook
in `.claude/settings.json` denies it. The hook's message explains the rewrite; the one task per spec
that owns full-suite validation can prefix `NEXTEST_POLICY_OVERRIDE=1` if it genuinely needs to.

The SDLC pipeline reads its checks from `planning/harness.json` — keep these in sync with
`validation.checks[]`.

## Test layout — one integration-test binary

```
crates/engine-core/tests/
├── it/
│   ├── main.rs                  <- the ONLY test target; mod declarations only
│   ├── harvest_gate_e2e.rs
│   ├── content_pipeline_e2e.rs
│   └── ...                      <- 29 suites, all modules of `it`
└── fixtures/
```

**To add an integration test:**

1. Create `crates/engine-core/tests/it/<name>.rs`.
2. Add `mod <name>;` to `crates/engine-core/tests/it/main.rs` (keep the list alphabetical).

**Do not create a `crates/engine-core/tests/*.rs` file.** cargo builds one binary per file at that
level, and each statically links `engine-core` plus its ~345-crate dependency graph. A single stray
file silently re-adds ~20MB of linking to every full test run.

Unit tests are unaffected — they stay in-file under `#[cfg(test)] mod tests` as usual.

**`term-core` and `term-attach` (added by `EN.9.A`) have no `tests/` directory at all**, and that is
deliberate rather than an oversight. All 116 `term-core` tests and all 3 `term-attach` tests are
in-file `#[cfg(test)]` units, carried over in that form from bastion, so neither crate adds an
integration-test binary to the workspace. Keep it that way: a `tests/*.rs` file in either crate would
re-introduce exactly the per-binary link cost this section exists to prevent. The `detect/` golden
tests are the case worth understanding — they read their manifests and fixtures through
`include_str!`, so they need no filesystem I/O and no test harness, which is why they work as plain
unit tests despite being fixture-driven.

### Why the layout, and why it is safe

Measured 2026-07-29 (see [D57](file:///Users/brandon/Dev/agentic-portfolio/docs/decisions/D57-rust-sdlc-iteration-speed.md)):

| | before | after (1 binary + nextest, no sccache) | + `target/` clean |
|---|---|---|---|
| Per-task tripwire (`--lib --workspace`, after an edit) | 2m44s | 1m17s | **6.4s** |
| Full-suite build after a one-line `engine-core` edit | 2m24s | 35s | **5.3s** |
| Full-suite run | 58s | 2.2s | **2.2s** |
| Full suite, nothing changed | minutes | 2.9s | **2.8s** |

*Running* the tests was never the cost — 1215 tests execute in ~2s. Linking was, plus a `target/`
directory that had rotted to 40GB (930,599 files). See "Keep `target/` clean" below.

Collapsing binaries would merge processes under `cargo test`, which could break any test relying on
process isolation (env vars, CWD, global state). **`cargo nextest run` executes every test in its
own process regardless of binary packing**, which is exactly what makes this safe here — and why the
nextest rule is a prerequisite for the layout rather than a stylistic preference.

A concrete case: `engine-serve`'s `suspend.rs`/`resume.rs`/`http.rs` unit and route tests share
process-global `OnceLock`-backed registries (the suspended-run index and pause signals). Under
`cargo nextest run` this is inert — each test gets a fresh process, so the statics start empty every
time. Under plain `cargo test` the same tests run as threads in one process and can race on those
statics (one test's transient insert flips another's `is_empty()` assertion, or CPU contention pushes
a polling loop past its deadline). Rather than special-casing the layout, these tests take a
test-only `crate::suspend::registry_test_lock()` mutex for their duration — visible only when a task
is explicitly overridden to run plain `cargo test` (`NEXTEST_POLICY_OVERRIDE=1`, see above).

## Keep `target/` clean

Cargo has no garbage collection: incremental state accumulates across branches, rebases, and
abandoned builds and is never reclaimed. This repo's `target/` had reached **40GB** (17GB of it
`incremental/`; `cargo clean` removed 930,599 files / 78.4GiB), and it was silently taxing every
single build — a from-scratch cold build afterwards (48.9s) came out nearly **3x faster** than the
*incremental* build after a one-line edit had been on the bloated tree (2m24s).

```bash
du -sh target        # check this first when the loop feels slow
cargo clean          # ~3m20s when the tree is that large; budget for it once
```

Clean when it passes a few GB, or after a long branch/rebase-heavy stretch. A healthy working size
for this workspace is ~2.4GB.

## Hermetic-test conventions

The e2e suites in `tests/it/` follow rules worth preserving:

- **No network.** Inject `StubHttpPost` / `StubChannelTransport` / stub `ModelTransport`s. The live
  seams (`http_post_live()`, `doc_materializer_live()`) are production-only.
- **No writes outside a tempdir.** Suites driving the real `MevDocMaterializer` pass an explicit
  `with_brain_root(tempdir)` rather than relying on `ENGINE_BRAIN_ROOT`, so they are immune to
  another test's env mutation and can never touch the real corpus.
- **Assert on-disk bytes, not just stamped paths.** mev's idempotency guard zero-stamps `paths` when
  content is unchanged, so a `paths`-only assertion can pass vacuously (learned in `EN.7.D`).
- **A test that drives real tmux gets its OWN socket, never the default server.** Build the driver
  with `TmuxDriver::new(..).with_socket(<unique-per-process>)` and tear down the whole server
  (`kill-server`) via a `Drop` guard, so a panicking test cannot leak. Do NOT add a helper that
  boots a throwaway session and kills it to "ensure a server" — killing the last session
  *terminates* the server, so on a clean machine that helper destroys the thing it is named for.
  Before `EN.ticket.real-tmux-tests-need-an-isolated-socket` these tests shared the default server
  and were **self-masking**: each failing run leaked sessions that kept the server alive, so the
  *next* run passed. Measured 2026-08-22 — 492 leaked sessions dating back four days, a green local
  suite, and the same tests red on CI, which has no pre-existing server. Green that depends on what
  the previous run left behind is not green.
- **Don't pre-create directories the writer should create.** Pre-creating
  `docs/content/learning-corpus/` in every test masked a real production bug where `apply_plan`
  never created parents (`EN.7.D`, fixed in `d1a8787`).

## The nextest terminate-after bound

`.config/nextest.toml` sets `[profile.default] slow-timeout = { period = "60s", terminate-after = 5 }`
(300s total). Before this existed, the default profile had no `terminate-after` at all: a
genuinely wedged test would report a SLOW line and then simply never return, blocking the
authoritative gate (`cargo nextest run --workspace --all-features`) forever with no verdict and
no diagnostic — the next agent has to guess what happened. With the bound set, any test that runs
past `period * terminate-after` is killed by nextest and reported as **TIMEOUT** instead of
hanging the run.

300s is set well above the slowest test observed on 2026-08-23
(`terminal_admission::on_disk_manifest_edit_is_picked_up_by_the_next_capture_with_no_rebuild` at
1.118s), so no existing test should ever trip it.

**If a legitimately slow test does trip it**, do not weaken the global bound — give that one test
its own override in `.config/nextest.toml`:

```toml
[[profile.default.overrides]]
filter = 'test(name_of_the_slow_test)'
slow-timeout = { period = "5s", terminate-after = 1 }
```

`crates/engine-core/tests/it/gate_timeout_fixture.rs` holds a deliberately-wedging `#[ignore]`d
test (`deliberately_wedges_to_prove_the_gate_terminates_it`) with exactly this shape of override,
so `scripts/test_nextest_terminates_a_hang.sh` can prove the bound actually fires without waiting
out the full 300s global bound.

### Two ways a slow build masquerades as a hang

The bound above exists because a P1 was once filed claiming a specific test hung. It did not
reproduce, and the investigation found two unrelated, non-defect causes that look identical to a
real hang from the outside:

- **Piping the command through `tail` (or anything else) buffers output** — a slow build then
  shows as zero output until the whole pipeline finishes, which reads exactly like a wedged test.
  Run gate commands unpiped when diagnosing a suspected hang.
- **A bloated `target/` tree makes incremental builds far slower than a cold build** — see "Keep
  `target/` clean" above: a 40GB tree's incremental build was measured at ~3x slower than a
  from-scratch cold build. "Builds for a while, then no output for 90+ seconds" matches this far
  better than a test hang does.

Before concluding a test has genuinely wedged, check `du -sh target` and re-run the command
unpiped first.

## Per-task validation in the SDLC loop

`tasks.json` tasks that cannot break the build — docs-only, config-only — should declare their own
`validation_commands`. `/sdlc-flow` and `/sdlc-task` run those instead of the project-wide gating
checks for that task, so a markdown edit no longer triggers a Rust compile. The end review still
re-runs the full gating suite over the integrated tree. Leave the array `[]` for any task touching
`.rs` files.

Full cross-project playbook:
[`base-template/docs/rust-sdlc-iteration-speed.md`](file:///Users/brandon/Dev/agentic-portfolio/base-template/docs/rust-sdlc-iteration-speed.md).
