//! The single integration-test binary for `engine-serve` tests that genuinely exercise
//! `engine-serve`'s own public API (dispatch, workflows, journal, durable, blocked_bridge) from
//! outside the crate — every file under `tests/it/` is a module of THIS binary, not a binary of
//! its own.
//!
//! Why: cargo builds one test binary per `tests/*.rs` file, and each one statically links the
//! whole crate plus its dependency graph. These modules were moved out of `engine-core`'s own
//! `tests/it/` binary (see that file's header) because they depend on `engine-serve` — which used
//! to be a dev-dependency of `engine-core`, dragging the sqlx/actix-web graph into every
//! `cargo test -p engine-core --lib` run even though the unit tests never touch it. Moving them
//! here keeps `engine-core --lib` cheap while these tests keep running exactly as before.
//!
//! Test ISOLATION is unaffected: `cargo nextest run` (this repo's mandated runner — CLAUDE.md
//! standing rule 8) executes every test in its own process regardless of how many binaries the
//! tests are packed into. That is what makes this collapse safe here when it would not be under
//! plain `cargo test`.
//!
//! `crates/engine-serve/tests/*.rs` (8 pre-existing loose files) is a SEPARATE, out-of-scope
//! concern — collapsing those into this binary is its own chore. Only the modules moved from
//! `engine-core` live here.
//!
//! Adding an integration test here: create `tests/it/<name>.rs` and add one `mod <name>;` line
//! below. Do NOT add a new `tests/*.rs` file at this level — that reintroduces a second binary.

mod blocked_bridge;
mod composition;
mod content_pipeline_e2e;
mod deliverable_render_e2e;
mod diagnostic_intake_e2e;
mod journal_wiring;
mod policy_dispatch_e2e;
mod proposal_generator_e2e;
mod research_agent_e2e;
