//! The two opportunity-edit micro-workflows (`EN.7.B` task 5) —
//! `OPPORTUNITY_SET_STAGE` and `OPPORTUNITY_ADD_ACTION` — the write half of
//! the web-side edits `BW.7.A` will trigger. Each is a single-node graph
//! wrapping the generic `crate::nodes::opportunity_edit::OpportunityEditNode`
//! (`EN.7.B` task 3) pinned to one `OpportunityEditOp`, mirroring
//! `diagnostic_intake`'s single-node-is-both-start-and-terminal shape.
//!
//! Module layout:
//! - `schema` — the two event payloads, [`schema::SetOpportunityStageEvent`]
//!   and [`schema::AddOpportunityActionEvent`] — the single source of truth
//!   for each workflow's field shape; `OpportunityEditNode::build_edit`
//!   assembles one of them from `ctx.event`.
//! - `graph` — the declared `WorkflowSchema` / `NodeRegistry` / `Workflow`
//!   assembly for both workflow types, `SET_STAGE_WORKFLOW_TYPE` /
//!   `ADD_ACTION_WORKFLOW_TYPE`.
//!
//! Unlike every other workflow module here, there is deliberately no
//! `policy` or `profiles` module: both workflows are deterministic and
//! model-free (`OpportunityEditNode` never calls a model), so there is no
//! policy layer to resolve. See `graph.rs`'s module doc for the no-policy
//! rationale in full.

pub mod graph;
pub mod schema;

pub use graph::{
    add_action_registry, add_action_schema, add_action_workflow, set_stage_registry,
    set_stage_schema, set_stage_workflow, ADD_ACTION_WORKFLOW_TYPE, SET_STAGE_WORKFLOW_TYPE,
};
pub use schema::{AddOpportunityActionEvent, SetOpportunityStageEvent};
