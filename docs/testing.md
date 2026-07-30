---
type: Guide
title: Testing engine-rs
description: How this workspace's tests are laid out (one integration-test binary), which commands to run, and why — with the measured numbers behind the layout.
doc_id: engine-rs-testing
layer: [engine]
project: engine-rs
status: active
keywords: [testing, nextest, integration tests, link time, cargo, harness]
related: [architecture, rust-sdlc-iteration-speed, d57-rust-sdlc-iteration-speed]
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
│   └── ...                      <- 25 suites, all modules of `it`
└── fixtures/
```

**To add an integration test:**

1. Create `crates/engine-core/tests/it/<name>.rs`.
2. Add `mod <name>;` to `crates/engine-core/tests/it/main.rs` (keep the list alphabetical).

**Do not create a `crates/engine-core/tests/*.rs` file.** cargo builds one binary per file at that
level, and each statically links `engine-core` plus its ~345-crate dependency graph. A single stray
file silently re-adds ~20MB of linking to every full test run.

Unit tests are unaffected — they stay in-file under `#[cfg(test)] mod tests` as usual.

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
- **Don't pre-create directories the writer should create.** Pre-creating
  `docs/content/learning-corpus/` in every test masked a real production bug where `apply_plan`
  never created parents (`EN.7.D`, fixed in `d1a8787`).

## Per-task validation in the SDLC loop

`tasks.json` tasks that cannot break the build — docs-only, config-only — should declare their own
`validation_commands`. `/sdlc-flow` and `/sdlc-task` run those instead of the project-wide gating
checks for that task, so a markdown edit no longer triggers a Rust compile. The end review still
re-runs the full gating suite over the integrated tree. Leave the array `[]` for any task touching
`.rs` files.

Full cross-project playbook:
[`base-template/docs/rust-sdlc-iteration-speed.md`](file:///Users/brandon/Dev/agentic-portfolio/base-template/docs/rust-sdlc-iteration-speed.md).
