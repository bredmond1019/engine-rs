# Task Spec — Phase 0, Block A (EN.0.A) — Cargo workspace + CI

**Status:** Not started · **Last run:** never

## Goal
Stand up the `engine-rs` Cargo workspace (root + four member crates), wire CI to run fmt/clippy/test/build on every push, and settle the async-runtime + persistence stack as a recorded decision.

## Context Pointers
- **Plan:** `planning/master-plan.md` → Phase 0 → **EN.0.A — Cargo workspace + CI** (the authoritative block definition — Files, Out of scope, Acceptance criteria).
- **Module map:** `docs/architecture.md` → *Module Map* names the four crates: `engine-core` (Node/Workflow/validator), `engine-contract` (data-contract serde types), `engine-store` (Postgres durable record), `engine-serve` (`bastion serve` embedding).
- **Standing rules** (`CLAUDE.md`): tests ship with every block (rule 1); new `.md` under `planning/` needs OKF frontmatter + an `index.md` update (rule 2); decisions are append-only atomic files in `planning/decisions/` (rule 4).
- **Downstream dependency:** the async-runtime/persistence choice is load-bearing for `engine-store` in EN.0.B — it must be settled here.
- **Validation config:** `planning/harness.json` → `validation.checks[]` already carries the Rust profile (fmt / clippy / test / build --release).

## Step-by-Step Tasks
See `tasks.json` in this directory — the task list is defined there, not here.

## Acceptance Criteria
- `cargo build --release`, `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` all succeed on a clean checkout of the workspace.
- The workspace root `Cargo.toml` declares all four member crates (`engine-core`, `engine-contract`, `engine-store`, `engine-serve`); each crate has a `Cargo.toml` and a compiling `src/lib.rs` stub.
- At least one trivial test exists and passes so `cargo test` exercises the suite (standing rule 1).
- CI config runs the same four commands (fmt / clippy / test / build) on push and pull request.
- The async-runtime + persistence choice is recorded as an atomic, OKF-frontmatter'd decision file in `planning/decisions/` and linked from `planning/decisions/index.md`.
- `planning/harness.json`'s validation commands are confirmed to match the real workspace layout (no path/command drift).

## Validation Commands
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```

## Notes
<filled in as work happens>

## Amendment Log
<!-- Append-only. Pipeline stages append one dated line here when they deviate from the spec. -->
_No amendments yet._
