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
//! - [`drain`] (task 3) — renders + validates a batch of pending-harvest
//!   records, enqueues the conforming ones onto an
//!   [`crate::operator::queue::OperatorQueue`], routes the rest to
//!   `session-<slug>`, and makes exactly one `next_deliverable` call.
//! - [`verdict`] (task 4) — resolves one operator verdict for a delivered
//!   item into exactly one [`crate::operator::ledger`] row via
//!   `record_decision`, without re-implementing its digest-mismatch ->
//!   `Requeued` enforcement, and authorizes execution against the
//!   pending-harvest record's stored payload — never a re-derived one —
//!   only on a matched `Approved` verdict.
//! - [`graph`] (task 5) — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `Workflow` assembly. Its single node drives the existing
//!   `nodes::harvest_approve::HarvestApproveNode` over the injectable
//!   `HttpPost` seam against task 4's authorization — no second push path —
//!   and stamps the resolved policy from [`policy`] into its own
//!   `ctx.nodes` result.

pub mod drain;
pub mod graph;
pub mod policy;
pub mod profiles;
pub mod render;
pub mod verdict;

pub use drain::{drain, DrainReport, SessionRoutedRecord};
pub use graph::{
    execution_event, registry, registry_with, schema, workflow, ApproveAndRunExecuteNode,
    APPROVE_AND_RUN_NODE_NAME, APPROVE_AND_RUN_WORKFLOW_TYPE,
};
pub use policy::{policy_state, ApproveAndRunPolicy, PartialApproveAndRunPolicy};
pub use profiles::{
    profile_by_name, read_harness_policy_defaults_from, resolve_policy_for_run_from,
};
pub use render::{
    gate_id_for, render, render_and_validate, PendingHarvestRecord, OPTION_APPROVE,
    OPTION_OPEN_SESSION, OPTION_SKIP,
};
pub use verdict::{decide, ExecutionAuthorization, UnknownOptionKey, VerdictOutcome};
