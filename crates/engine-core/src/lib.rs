//! `engine-core` — the Node/Workflow runner and graph validator for engine-rs.
//!
//! `Node` trait + registry land in EN.1.A task 1. `WorkflowSchema`/`NodeConfig`
//! land in EN.1.A task 2. The `Workflow` pointer-walk runner + `on_progress`
//! seam land in EN.1.A task 3. Brain-root / target-corpus resolution
//! (`ENGINE_BRAIN_ROOT`, walk-up via `mev::brain::config::find_brain_root`)
//! lands in EN.7.A task 2 — see `docs/architecture.md` for the module map.

pub mod brain_root;
pub mod budget;
pub mod build_info;
pub mod cancellation;
pub mod completion;
pub mod cron;
pub mod dispatch;
pub mod evals;
pub mod locale;
pub mod loop_combinator;
pub mod node;
pub mod nodes;
pub mod operator;
pub mod parallel;
pub mod policy;
pub mod repo_registry;
pub mod routing;
pub mod schema;
pub mod suspend;
pub mod validate;
pub mod workflow;
pub mod workflows;

pub use brain_root::{resolve_brain_root, resolve_brain_root_from, BrainRootError};
pub use budget::{Budget, BudgetDecision, BudgetHaltReason, BudgetLedger, CampaignLedger};
pub use cancellation::{stamp_cancelled, CancellationToken, CANCELLATION_METADATA_KEY};
pub use completion::{
    derive_terminal_status, is_complete, stamp_completion, COMPLETION_METADATA_KEY,
};
pub use dispatch::{DispatchError, Dispatcher, WorkflowFactory};
pub use locale::{Currency, Locale};
pub use loop_combinator::{build_loop, ExitPredicate, LoopCluster, LoopSpec};
pub use node::{Identified, InputBinding, Node, NodeError, NodeExt, NodeRegistry, WithInput};
pub use nodes::ClaudeCodeStep;
pub use nodes::{BrainConfig, BrainConfigError, HttpGet};
pub use parallel::ParallelNode;
pub use routing::{dispatch_route, Router};
pub use schema::{NodeConfig, WorkflowSchema};
pub use suspend::{
    is_suspended, read_suspension, request_suspension, stamp_resumed, stamp_suspended,
    suspension_requested, LedgerSnapshot, PauseSignal, SuspendReason, Suspension, SuspensionState,
    SUSPENSION_METADATA_KEY,
};
pub use validate::{ValidationError, WorkflowValidator};
pub use workflow::{
    read_run_id, OnProgress, RunOptions, Workflow, WorkflowError, BUDGET_METADATA_KEY,
    RUN_ID_METADATA_KEY,
};
