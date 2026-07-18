//! `engine-core` — the Node/Workflow runner and graph validator for engine-rs.
//!
//! `Node` trait + registry land in EN.1.A task 1. `WorkflowSchema`/`NodeConfig`
//! land in EN.1.A task 2. The `Workflow` pointer-walk runner + `on_progress`
//! seam land in EN.1.A task 3 — see `docs/architecture.md` for the module map.

pub mod budget;
pub mod cancellation;
pub mod node;
pub mod nodes;
pub mod parallel;
pub mod routing;
pub mod schema;
pub mod validate;
pub mod workflow;
pub mod workflows;

pub use budget::{Budget, BudgetDecision, BudgetHaltReason, BudgetLedger};
pub use cancellation::{stamp_cancelled, CancellationToken, CANCELLATION_METADATA_KEY};
pub use node::{Node, NodeError, NodeRegistry};
pub use nodes::ClaudeCodeStep;
pub use parallel::ParallelNode;
pub use routing::{dispatch_route, Router};
pub use schema::{NodeConfig, WorkflowSchema};
pub use validate::{ValidationError, WorkflowValidator};
pub use workflow::{OnProgress, RunOptions, Workflow, WorkflowError, BUDGET_METADATA_KEY};
