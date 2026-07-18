//! Assembles the declared `WorkflowSchema` + `NodeRegistry` for the SDLC
//! Flow top-half workflow (`workflow_type = "SDLC_FLOW"`).
//!
//! Scaffolded in EN.3.A task 1; implemented in EN.3.A task 4.
//!
//! Declared graph shape (matching the `sdlc_flow_workflow.py` docstring for
//! the top half):
//!
//! ```text
//! SetupWorktreeNode -> SpecExistsRouterNode -> { GenerateTasksNode -> LoadTaskStateNode
//!                                              | LoadTaskStateNode }
//!   -> TaskQueueRouterNode -> { ImplementTaskNode -> TestTaskNode -> TriageTaskNode
//!                                 -> TriageRouterNode -> ConsolidatedReviewNode
//!                                 -> ReviewRouterNode -> UpdateTaskStatusNode
//!                                 -> SaveStateNode -> (loop) TaskQueueRouterNode
//!                             | PatchDocsNode (terminal stub; EN.3.B replaces it) }
//! ```
//!
//! Declared `connections` stay acyclic (per [`crate::validate::WorkflowValidator`]):
//! every router's declared connections are the runtime-consulted `route()`'s
//! *forward* branches only. The retry back-edges from `TriageRouterNode`
//! (`RETRYABLE`) and `ReviewRouterNode` (minor-issue `FAIL`/`PARTIAL`) into
//! `ImplementTaskNode`, and the `MAJOR_BAIL`/structural-issue branches into the
//! unregistered `WrapUpNode` identity (an EN.3.B stub, deliberately left out
//! of this graph's registry and declared connections), are runtime-only —
//! chosen by `Router::route` and never declared here — per D42
//! (declared-acyclic / runtime-cyclic). `SaveStateNode` is not a router, so
//! its loop-closing hop back to `TaskQueueRouterNode` *is* a declared
//! connection; the validator's cycle check does not flag it because
//! `TaskQueueRouterNode` is itself a router and its own declared out-edges
//! are skipped by that check, so no cycle is ever walked end-to-end.

use std::collections::HashMap;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError, NodeRegistry};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::setup::{GenerateTasksNode, LoadTaskStateNode, SetupWorktreeNode, SpecExistsRouterNode};
use super::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, ReviewRouterNode, SaveStateNode,
    TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode, UpdateTaskStatusNode,
};

/// Terminal stub identity for the "task queue is empty" branch of
/// `TaskQueueRouterNode`. EN.3.B replaces this with the real `PatchDocsNode`
/// (the next stage of the pipeline); here it only needs to exist so the
/// declared graph is fully reachable and the walk has somewhere to land.
pub struct PatchDocsNode;

#[async_trait::async_trait]
impl Node for PatchDocsNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PatchDocsNode"
    }
}

/// The `SDLC_FLOW` workflow's declared identity/type name, used both to
/// register the workflow (engine-serve, Task 5) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "SDLC_FLOW";

/// Build the declared `WorkflowSchema` for the top-half SDLC Flow workflow.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "SetupWorktreeNode".to_string(),
        NodeConfig::new(
            "SetupWorktreeNode",
            vec!["SpecExistsRouterNode".to_string()],
        ),
    );
    nodes.insert(
        "SpecExistsRouterNode".to_string(),
        NodeConfig::new(
            "SpecExistsRouterNode",
            vec![
                "GenerateTasksNode".to_string(),
                "LoadTaskStateNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "GenerateTasksNode".to_string(),
        NodeConfig::new("GenerateTasksNode", vec!["LoadTaskStateNode".to_string()]),
    );
    nodes.insert(
        "LoadTaskStateNode".to_string(),
        NodeConfig::new("LoadTaskStateNode", vec!["TaskQueueRouterNode".to_string()]),
    );
    nodes.insert(
        "TaskQueueRouterNode".to_string(),
        NodeConfig::new(
            "TaskQueueRouterNode",
            vec!["ImplementTaskNode".to_string(), "PatchDocsNode".to_string()],
        ),
    );
    nodes.insert(
        "ImplementTaskNode".to_string(),
        NodeConfig::new("ImplementTaskNode", vec!["TestTaskNode".to_string()]),
    );
    nodes.insert(
        "TestTaskNode".to_string(),
        NodeConfig::new("TestTaskNode", vec!["TriageTaskNode".to_string()]),
    );
    nodes.insert(
        "TriageTaskNode".to_string(),
        NodeConfig::new("TriageTaskNode", vec!["TriageRouterNode".to_string()]),
    );
    nodes.insert(
        "TriageRouterNode".to_string(),
        NodeConfig::new(
            "TriageRouterNode",
            vec!["ConsolidatedReviewNode".to_string()],
        ),
    );
    nodes.insert(
        "ConsolidatedReviewNode".to_string(),
        NodeConfig::new(
            "ConsolidatedReviewNode",
            vec!["ReviewRouterNode".to_string()],
        ),
    );
    nodes.insert(
        "ReviewRouterNode".to_string(),
        NodeConfig::new("ReviewRouterNode", vec!["UpdateTaskStatusNode".to_string()]),
    );
    nodes.insert(
        "UpdateTaskStatusNode".to_string(),
        NodeConfig::new("UpdateTaskStatusNode", vec!["SaveStateNode".to_string()]),
    );
    nodes.insert(
        "SaveStateNode".to_string(),
        NodeConfig::new("SaveStateNode", vec!["TaskQueueRouterNode".to_string()]),
    );
    nodes.insert(
        "PatchDocsNode".to_string(),
        NodeConfig::new("PatchDocsNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "SetupWorktreeNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-subprocess/real-transport)
/// configuration. Tests build their own registry with stubbed transports and
/// runners instead of calling this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SetupWorktreeNode::new()));
    registry.register(Box::new(SpecExistsRouterNode));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode));
    registry.register(Box::new(TaskQueueRouterNode));
    registry.register(Box::new(ImplementTaskNode::new()));
    registry.register(Box::new(TestTaskNode::new()));
    registry.register(Box::new(TriageTaskNode::new()));
    registry.register(Box::new(TriageRouterNode));
    registry.register(Box::new(ConsolidatedReviewNode::new()));
    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(SaveStateNode::new()));
    registry.register(Box::new(PatchDocsNode));
    registry
}

/// Build the runnable top-half SDLC Flow `Workflow`: [`registry`] paired
/// with [`schema`], constructed via `Workflow::new_validated` so assembly
/// fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("SDLC_FLOW declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_setup_worktree() {
        assert_eq!(schema().start_node, "SetupWorktreeNode");
    }

    #[test]
    fn workflow_type_is_sdlc_flow() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_all_thirteen_nodes_plus_patch_docs_stub() {
        let registry = registry();

        let expected = [
            "SetupWorktreeNode",
            "SpecExistsRouterNode",
            "GenerateTasksNode",
            "LoadTaskStateNode",
            "TaskQueueRouterNode",
            "ImplementTaskNode",
            "TestTaskNode",
            "TriageTaskNode",
            "TriageRouterNode",
            "ConsolidatedReviewNode",
            "ReviewRouterNode",
            "UpdateTaskStatusNode",
            "SaveStateNode",
            "PatchDocsNode",
        ];

        for identity in expected {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), expected.len());
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }
}
