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
//! - [`integrate`] (Task 4) — operator hold as pause-and-resume through the `EN.9.G`
//!   Blocked-edge bridge, state-write verification that fails loudly on a mismatch, and
//!   exactly one `lane-log.jsonl` line per integrated block, with the roadmap directory
//!   resolved by `/begin-orchestration`'s Step 1C two-location rule.
//! - [`graph`] (Task 5) — the `ORCHESTRATION` `WorkflowSchema`/`NodeRegistry`/`Workflow`
//!   assembly (`Workflow::new_validated`), wrapping Tasks 1-4 in a single
//!   `OrchestrationRunNode`, plus the three named policy profiles.
//! - [`engine_kind`] (`EN.10.C` Task 1) — the closed, two-variant [`engine_kind::EngineKind`]
//!   type that makes an unsanctioned runner structurally unrepresentable, plus the
//!   `sdlc_workflow` -> `EngineKind` mapping. Re-exported from [`execute`] as
//!   `execute::EngineKind` for every existing caller/test of this module.
//! - [`corpus_gates`] (`EN.ticket.orchestration-production-gates-unwired` Task 1) — the
//!   corpus-backed `resolve_depends_on` / `is_edge_met` / `is_block_open`
//!   implementations that wire [`graph::OrchestrationRunNode`]'s gate seams to real
//!   `planning/state.json` files via `RepoRegistry` + `okf_core::load_state`,
//!   instead of [`graph::OrchestrationRunNode::new`]'s permissive always-proceed
//!   defaults. `engine-serve`'s `register_orchestration_with_registry` (Task 2) is
//!   what actually installs these into the served workflow.

pub mod chain;
pub mod corpus_gates;
pub mod engine_kind;
pub mod execute;
pub mod gates;
pub mod graph;
pub mod integrate;
