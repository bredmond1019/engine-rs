//! The `RECALL` micro-workflow (`EN.12.L` task 2) — a single-node
//! [`crate::nodes::brain_client::RecallNode`] wrapping, reachable as an
//! `EN.12.E` `kind: dispatch` chain step so a chain can visibly branch on
//! what Synapse's `GET /recall` already knows (D23 constraint 3).
//!
//! `RecallNode`'s query comes from the triggering event via its existing
//! unbound `InputBinding` (reads `ctx.event`), so this workflow needs no
//! new query plumbing beyond registration itself. Module layout mirrors
//! `harvest_approve`'s single-node micro-workflow shape:
//! - [`graph`] — the declared `WorkflowSchema` / `NodeRegistry` /
//!   `Workflow` assembly, built via `Workflow::new_validated` so a
//!   structurally unsound graph fails loudly at assembly.
//!
//! **No policy module, no profiles module.** `RecallNode` calls no model;
//! its `limit`/`hybrid` builder args are a call site's fixed shape, not a
//! per-run `Policy` knob — see `graph.rs`'s module doc for the full
//! rationale (CLAUDE.md standing rule 6's "where feasible" carve-out).

pub mod graph;

pub use graph::{registry, schema, workflow, RECALL_WORKFLOW_TYPE};
