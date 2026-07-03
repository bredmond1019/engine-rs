# Implementation Report — EN.0.A-cargo-workspace-ci

**Date:** 2026-07-02
**Plan:** planning/EN.0.A-cargo-workspace-ci/tasks.md
**Scope:** Full spec

## What Was Built or Changed
- Recorded the async-runtime + persistence decision as `planning/decisions/D2-async-runtime-choice.md` (tokio + sqlx with postgres/runtime-tokio/tls-rustls features) and linked it from `planning/decisions/index.md`.
- Created the workspace root `Cargo.toml` (resolver 2, `[workspace.package]`, `[workspace.dependencies]` for tokio/sqlx/serde/serde_json) declaring the four member crates.
- Created `crates/engine-core`, `crates/engine-contract`, `crates/engine-store`, `crates/engine-serve` — each with its own `Cargo.toml` and a compiling `src/lib.rs` stub carrying one trivial passing `#[test]`.
- Added `.gitignore` (`/target`) since none existed at repo root.
- Added `.github/workflows/ci.yml` — runs on push (all branches) and pull_request, installs stable Rust + rustfmt/clippy via `dtolnay/rust-toolchain`, caches Cargo via `Swatinem/rust-cache`, and runs the same four gate commands as `planning/harness.json`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`.
- Verified `planning/harness.json`'s `validation.checks[]` already match the real workspace layout byte-for-byte — no changes needed.

## Files Created or Modified
| File | Action |
|---|---|
| planning/decisions/D2-async-runtime-choice.md | created |
| planning/decisions/index.md | modified |
| Cargo.toml | created |
| Cargo.lock | created (generated) |
| .gitignore | created |
| crates/engine-core/Cargo.toml | created |
| crates/engine-core/src/lib.rs | created |
| crates/engine-contract/Cargo.toml | created |
| crates/engine-contract/src/lib.rs | created |
| crates/engine-store/Cargo.toml | created |
| crates/engine-store/src/lib.rs | created |
| crates/engine-serve/Cargo.toml | created |
| crates/engine-serve/src/lib.rs | created |
| .github/workflows/ci.yml | created |
| planning/EN.0.A-cargo-workspace-ci/sdlc/reports/implement.md | created |

## Validation Output
**Commands run:**
```
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
```
**Results:**
```
== fmt ==
(no output — clean)

== clippy ==
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s

== test ==
running 1 test
test tests::crate_name_is_engine_contract ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 1 test
test tests::crate_name_is_engine_core ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 1 test
test tests::crate_name_is_engine_serve ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 1 test
test tests::crate_name_is_engine_store ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

== build ==
    Finished `release` profile [optimized] target(s) in 0.14s
```
Status: PASSED

## Decisions and Trade-offs
- Recorded D2 (tokio + sqlx) per task 1 — see `planning/decisions/D2-async-runtime-choice.md` for full rationale. Chose `sqlx` over `tokio-postgres` + `deadpool-postgres` for compile-time query checking and native `Json<T>` ergonomics against the data-contract's JSON columns; `tls-rustls` keeps the dependency tree pure-Rust for the Mac Mini deploy target. This block only pins the dependency — no Postgres code is written here (deferred to EN.0.B).
- `engine-core` stub carries `tokio` only as a dev-dependency (its trivial test doesn't need async yet); `engine-store` and `engine-serve` carry `tokio` + `sqlx` as real dependencies since they are the crates that will actually use the async/persistence stack.
- No repo-root `.gitignore` existed before this block; added a minimal one (`/target`) since the workspace now produces a `target/` directory that must not be committed. `Cargo.lock` is committed (workspace binaries/libraries convention for a solo-maintained repo where reproducible builds matter more than flexible dep resolution downstream).
- CI trigger is `push: branches: ["**"]` (all branches) plus `pull_request`, matching "on every push" from the spec's Goal without restricting to a single branch, since this repo doesn't yet have a fixed branching model documented.

## Follow-up Work
- `engine-store`'s actual `sqlx::PgPool` read/write path for the `events` row is EN.0.B, not this block.
- `docs/architecture.md`'s Module Map / Core Types / Data Flow sections remain intentionally stubbed until the corresponding Phase 0/1 blocks land (per that file's own inline comments) — not part of EN.0.A's acceptance criteria.

## git diff --stat
```
 planning/decisions/index.md | 3 +++
 planning/status.md          | 4 ++--
 2 files changed, 5 insertions(+), 2 deletions(-)
```
(New/untracked files not shown by `git diff --stat`: Cargo.toml, Cargo.lock, .gitignore, crates/**, .github/workflows/ci.yml, planning/decisions/D2-async-runtime-choice.md, planning/EN.0.A-cargo-workspace-ci/sdlc/reports/implement.md — see `git status --short` above.)
