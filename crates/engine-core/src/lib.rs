//! `engine-core` — the Node/Workflow runner and graph validator for engine-rs.
//!
//! `Node` trait + registry land in EN.1.A task 1. `WorkflowSchema`/`NodeConfig`
//! and the `Workflow` pointer-walk runner land in later EN.1.A tasks — see
//! `docs/architecture.md` for the module map.

pub mod node;

pub use node::{Node, NodeError, NodeRegistry};
