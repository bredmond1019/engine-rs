//! `engine-core` — the Node/Workflow runner and graph validator for engine-rs.
//!
//! `Node` trait + registry land in EN.1.A task 1. `WorkflowSchema`/`NodeConfig`
//! land in EN.1.A task 2. The `Workflow` pointer-walk runner lands in a later
//! EN.1.A task — see `docs/architecture.md` for the module map.

pub mod node;
pub mod schema;

pub use node::{Node, NodeError, NodeRegistry};
pub use schema::{NodeConfig, WorkflowSchema};
