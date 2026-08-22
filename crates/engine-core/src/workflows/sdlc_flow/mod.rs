//! The SDLC Flow (`SDLC_FLOW`) workflow — a Rust port of the Python
//! `orchestrator/app/workflows/sdlc_flow_workflow.py` pipeline's top half:
//! setup → generate/load tasks → the implement/test/triage/review task loop
//! with its runtime retry back-edges.
//!
//! Module layout (each leaf file owned by exactly one task in
//! `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.json` or
//! `planning/EN.3.B-sdlc-flow-docs-wrapup-pr/tasks.json`):
//! - `schema` — the ported `SDLCState`/`SDLCTask`/`SDLCFlowEventSchema` types.
//! - `setup` — `SetupWorktreeNode` / `SpecExistsRouterNode` /
//!   `GenerateTasksNode` / `LoadTaskStateNode`.
//! - `task_loop` — the implement→test→triage→review→update/save loop nodes
//!   and routers.
//! - `docs` — `PatchDocsNode` (bottom-half, EN.3.B).
//! - `wrap_up` — `WrapUpNode` (bottom-half, EN.3.B).
//! - `pr` — `PullRequestNode` (bottom-half, EN.3.B).
//! - `close_block` — `CloseBlockNode`: closes this run's block in
//!   `planning/state.json` through mev, under mev's advisory lock and D71
//!   operator gate (`EN.ticket.wrap-up-closes-the-block`).
//! - `emit_state` — `EmitStateNode` (bottom-half, EN.3.B).
//! - `final_validation` — `FinalValidationNode`: the unconditional run-level
//!   full-suite gate on the task-loop drain branch (`EN.3.E`).
//! - `graph` — assembles the declared `WorkflowSchema` + `NodeRegistry` for
//!   the whole workflow.
//! - `aggregate` — the cross-run `(policy -> cost, time, quality)`
//!   aggregator (EN.3.C task 7): reads a set of `sdlc-flow-state.json`
//!   snapshots and tabulates one row per distinct resolved policy.
//!
//! The node-plumbing seams shared by every submodule — `CommandOutput` /
//! `CommandRunner` / `default_command_runner` (hoisted in EN.3.B task 1 out
//! of `setup.rs`/`task_loop.rs`, which had byte-identical private copies) —
//! together with `ModelTransport`, the `put_result`/`get_result` context
//! helpers, `strip_json_fence`, and `parse_structured_or_fenced`, all now
//! live in `workflows::mod` (EN.4.0 task 4 hoisted the model-node seams;
//! `EN.11.M` task 2 hoisted the command-runner cluster), since none of them
//! are SDLC-specific; this module re-exports them so every existing
//! `super::`/`sdlc_flow::` import site keeps resolving unchanged.

// `strip_json_fence` has no direct callers left in this module now that
// `parse_structured_or_fenced` (also re-exported here) is the sole caller,
// but it stays re-exported for back-compat — any `super::strip_json_fence`
// import site elsewhere in the crate must keep resolving unchanged.
#[allow(unused_imports)]
pub(crate) use super::{get_result, parse_structured_or_fenced, put_result, strip_json_fence};
pub use super::{ModelTransport, TransportSlot};
#[allow(unused_imports)]
pub use crate::policy::command_floor::{self, evaluate_command, CommandDecision};

pub mod aggregate;
pub mod close_block;
pub mod docs;
pub mod emit_state;
pub mod end_review;
pub mod final_validation;
pub mod graph;
pub mod policy;
pub mod pr;
pub mod profiles;
pub mod schema;
pub mod setup;
pub mod task_loop;
pub mod wrap_up;

#[allow(unused_imports)]
pub(crate) use super::{commit_all, is_noop_commit};
/// The command-runner cluster (`CommandOutput`/`CommandRunner`/
/// `default_command_runner`/`commit_all`/`is_noop_commit`) now lives in
/// `workflows::mod` (`EN.11.M` task 2) alongside `ModelTransport` and the
/// other shared model-node seams, so a second engine can reuse them without
/// depending on this module. Re-exported here so every existing
/// `super::`/`sdlc_flow::` import site keeps resolving unchanged.
pub use super::{default_command_runner, CommandOutput, CommandRunner};
