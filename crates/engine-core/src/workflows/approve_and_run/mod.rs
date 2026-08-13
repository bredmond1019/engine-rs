//! The `APPROVE_AND_RUN` micro-workflow (`EN.8.D`) — drains the
//! pending-harvest queue `HARVEST_APPROVE` already serves through `EN.8.B`'s
//! depth-limited operator queue and `EN.8.C`'s approval ledger, so a
//! deferred harvest arrives as a decision the operator can answer in one
//! tap and, on approval, executes against the exact payload they reviewed.
//!
//! Composes existing primitives; it invents no new one:
//! [`crate::nodes::harvest_gate`]/[`crate::nodes::harvest_gate::pending_harvest_record`]
//! (`EN.7.C`), the payload contract in [`crate::operator`] (`EN.8.A`), the
//! queue in [`crate::operator::queue`] (`EN.8.B`), and the ledger in
//! [`crate::operator::ledger`] (`EN.8.C`).
//!
//! Module layout (built up task by task per `planning/EN.8.D/tasks.json`):
//! - [`render`] (task 1) — the pure pending-harvest-record -> validated
//!   operator payload step.
//! - [`policy`] / [`profiles`] (task 2) — the `drain_batch_max` /
//!   `harvest_item_priority` / `session_fallback_slug` knobs this block
//!   introduces, resolved through the standard four-layer precedence and
//!   documented in `planning/harness.json`'s `approve_and_run` section.

pub mod policy;
pub mod profiles;
pub mod render;

pub use policy::{policy_state, ApproveAndRunPolicy, PartialApproveAndRunPolicy};
pub use profiles::{
    profile_by_name, read_harness_policy_defaults_from, resolve_policy_for_run_from,
};
pub use render::{
    gate_id_for, render, render_and_validate, PendingHarvestRecord, OPTION_APPROVE,
    OPTION_OPEN_SESSION, OPTION_SKIP,
};
