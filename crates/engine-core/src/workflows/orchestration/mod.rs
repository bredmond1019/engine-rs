//! The ORCHESTRATION workflow — `EN.10.B`.
//!
//! Reads a lane chain (a roadmap + lane, or an explicit block list) and drives one
//! `SDLC_FLOW` run per block across repos, in order, honouring dependency gates,
//! admission control, structured lane directives, and operator holds. See
//! `planning/EN.10.B/tasks.md` for the full spec.
//!
//! Submodules land as their owning task completes:
//! - [`chain`] (Task 1) — lane chain resolution from a roadmap+lane or an explicit
//!   block list, consuming mev's structured `HELD-UNTIL` / `BUDGET` /
//!   `EXCLUSIVE-REPOS` directives and `planning/lane-segments.json`.
//! - [`gates`] (Task 2) — the dependency gate (every `depends_on` edge must be met
//!   before a block starts) and the `EN.9.F` admission gate (queue at capacity, never
//!   proceed or fail), both consulted before every block.
//! - [`execute`] (Task 3) — invokes the existing `SDLC_FLOW` workflow per block in a
//!   short-lived, cwd-scoped run, selecting the engine from the block's own authored
//!   `sdlc_workflow` field.

pub mod chain;
pub mod execute;
pub mod gates;
