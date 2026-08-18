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

pub mod chain;
