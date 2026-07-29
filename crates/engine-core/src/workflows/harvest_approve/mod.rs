//! The `HARVEST_APPROVE` micro-workflow (`EN.7.C` task 6) — the human-approval
//! completion hop for a deferred (`HarvestMode::Approval`) harvest. A single
//! node wrapping `crate::nodes::harvest_approve::HarvestApproveNode`,
//! mirroring `opportunity_edit`'s single-node-is-both-start-and-terminal
//! shape.
//!
//! An operator observes a pending harvest via `EN.5.F`'s run readback (the
//! `pending` record `PersistToBrainNode` stamped under `HarvestMode::
//! Approval`) and completes it by feeding that record back in as this
//! workflow's triggering event — `HarvestApproveNode` POSTs the record's
//! `payload` to its `url` verbatim, so the eventual push is byte-identical
//! to what an `in_process` push would have sent at materialize time. This
//! reuses the ONE existing ingest endpoint; it is not a second indexing
//! path (D51).
//!
//! Module layout:
//! - `schema` — [`schema::HarvestApproveEvent`], the single source of truth
//!   for the pending-record event shape.
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` / `Workflow`
//!   assembly, `HARVEST_APPROVE_WORKFLOW_TYPE`.
//!
//! Like `opportunity_edit`, there is deliberately no `policy` or `profiles`
//! module: `HarvestApproveNode` calls no model, so there is no policy layer
//! to resolve. See `graph.rs`'s module doc for the no-policy rationale in
//! full.

pub mod graph;
pub mod schema;

pub use graph::{registry, schema as workflow_schema, workflow, HARVEST_APPROVE_WORKFLOW_TYPE};
pub use schema::HarvestApproveEvent;
