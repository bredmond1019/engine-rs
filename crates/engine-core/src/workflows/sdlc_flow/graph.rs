//! Assembles the declared `WorkflowSchema` + `NodeRegistry` for the full
//! SDLC Flow workflow (`workflow_type = "SDLC_FLOW"`).
//!
//! Scaffolded in EN.3.A task 1; top-half implemented in EN.3.A task 4;
//! bottom-half (`docs` → `wrap_up` → `pr` → `emit_state`) and the
//! `IncrementAttemptNode` retry-bail wiring assembled in EN.3.B task 6.
//!
//! Declared graph shape (matching the `sdlc_flow_workflow.py` docstring):
//!
//! ```text
//! SetupWorktreeNode -> SpecExistsRouterNode -> { GenerateTasksNode -> LoadTaskStateNode
//!                                              | LoadTaskStateNode }
//!   -> TaskQueueRouterNode -> { ImplementTaskNode -> TestTaskNode -> TriageTaskNode
//!                                 -> TriageRouterNode -> ConsolidatedReviewNode
//!                                 -> ReviewRouterNode -> UpdateTaskStatusNode
//!                                 -> SaveStateNode -> (loop) TaskQueueRouterNode
//!                             | PatchDocsNode -> WrapUpNode -> PullRequestNode
//!                                 -> EmitStateNode }
//!
//! TriageRouterNode    -> { ConsolidatedReviewNode | IncrementAttemptNode | WrapUpNode }
//! ReviewRouterNode    -> { UpdateTaskStatusNode | IncrementAttemptNode | WrapUpNode }
//! IncrementAttemptNode -> ImplementTaskNode
//! ```
//!
//! Declared `connections` stay acyclic (per [`crate::validate::WorkflowValidator`]):
//! every router's declared connections are the runtime-consulted `route()`'s
//! branches, but the retry back-edges (`TriageRouterNode`'s `RETRYABLE` and
//! `ReviewRouterNode`'s minor `FAIL`/`PARTIAL`, both landing on
//! `IncrementAttemptNode`, which then forwards to `ImplementTaskNode`) and the
//! `MAJOR_BAIL`/structural-issue branches into `WrapUpNode` are chosen by
//! `Router::route` at runtime, never walked by the validator's cycle check —
//! per D42 (declared-acyclic / runtime-cyclic). The validator's cycle check
//! does not flag any of this because `TriageRouterNode`/`ReviewRouterNode`/
//! `TaskQueueRouterNode` are routers and their own declared out-edges are
//! skipped by that check, so no cycle is ever walked end-to-end — including
//! through `IncrementAttemptNode -> ImplementTaskNode -> ... -> TriageRouterNode`.
//! `SaveStateNode` is not a router, so its loop-closing hop back to
//! `TaskQueueRouterNode` *is* a declared connection, but is likewise never
//! walked into a cycle for the same reason.

use std::collections::HashMap;
use std::sync::Arc;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::docs::PatchDocsNode;
use super::emit_state::EmitStateNode;
use super::policy::{ModelTier, SdlcPolicy};
use super::pr::PullRequestNode;
use super::setup::{GenerateTasksNode, LoadTaskStateNode, SetupWorktreeNode, SpecExistsRouterNode};
use super::task_loop::{
    ConsolidatedReviewNode, ImplementTaskNode, IncrementAttemptNode, ReviewRouterNode,
    SaveStateNode, TaskQueueRouterNode, TestTaskNode, TriageRouterNode, TriageTaskNode,
    UpdateTaskStatusNode,
};
use super::wrap_up::WrapUpNode;
use super::ModelTransport;

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
            vec![
                "ConsolidatedReviewNode".to_string(),
                "IncrementAttemptNode".to_string(),
                "WrapUpNode".to_string(),
            ],
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
        NodeConfig::new(
            "ReviewRouterNode",
            vec![
                "UpdateTaskStatusNode".to_string(),
                "IncrementAttemptNode".to_string(),
                "WrapUpNode".to_string(),
            ],
        ),
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
        "IncrementAttemptNode".to_string(),
        NodeConfig::new(
            "IncrementAttemptNode",
            vec!["ImplementTaskNode".to_string()],
        ),
    );
    nodes.insert(
        "PatchDocsNode".to_string(),
        NodeConfig::new("PatchDocsNode", vec!["WrapUpNode".to_string()]),
    );
    nodes.insert(
        "WrapUpNode".to_string(),
        NodeConfig::new("WrapUpNode", vec!["PullRequestNode".to_string()]),
    );
    nodes.insert(
        "PullRequestNode".to_string(),
        NodeConfig::new("PullRequestNode", vec!["EmitStateNode".to_string()]),
    );
    nodes.insert(
        "EmitStateNode".to_string(),
        NodeConfig::new("EmitStateNode", vec![]),
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
    registry.register(Box::new(IncrementAttemptNode));
    registry.register(Box::new(PatchDocsNode::new()));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(PullRequestNode::new()));
    registry.register(Box::new(EmitStateNode::new()));
    registry
}

/// The real `claude_code_rs::execute` transport — the cloud fallback a
/// `local`-tier judgment stage's `openai_compat_transport` routes to when
/// its local endpoint is unavailable. `ClaudeCodeStep` deliberately keeps
/// its own equivalent default private (D4 owns that seam); this is the same
/// one-line delegation, just visible to this module so `registry_for_policy`
/// can hand it to `openai_compat_transport_live` as the fallback.
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with the single-shot
/// judgment stages the `local` model tier is scoped to — `TriageTaskNode`'s
/// `llm_triage` model branch and `ConsolidatedReviewNode` — wired to route
/// through [`openai_compat_transport_live`] whenever `policy`'s resolved
/// tier for that stage is [`ModelTier::Local`]. **Never** rewires
/// `ImplementTaskNode`: the local tier is scoped to single-shot judgment
/// calls, not the agentic `implement` stage (spec Context Pointers,
/// `planning/local-llm-tier-investigation/notes.md`). Any stage whose tier
/// is not `Local` keeps [`registry`]'s default (real-`claude`-CLI)
/// transport untouched.
///
/// Any local-endpoint failure at call time falls back to the real `claude`
/// CLI transport for that call — `openai_compat_transport`'s own fail-fast
/// + fallback (EN.3.C task 5), not something this function decides.
#[must_use]
pub fn registry_for_policy(policy: &SdlcPolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.triage == ModelTier::Local {
        registry.register(Box::new(TriageTaskNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.review == ModelTier::Local {
        registry.register(Box::new(ConsolidatedReviewNode::new().with_transport(
            openai_compat_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

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
    fn registry_contains_all_seventeen_nodes_incl_bottom_half() {
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
            "IncrementAttemptNode",
            "PatchDocsNode",
            "WrapUpNode",
            "PullRequestNode",
            "EmitStateNode",
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
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&SdlcPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("TriageTaskNode"));
        assert!(policy_registry.contains("ConsolidatedReviewNode"));
    }

    #[test]
    fn registry_for_policy_with_local_tiers_keeps_same_node_identities() {
        let policy = SdlcPolicy {
            model_tiers: super::super::policy::ModelTiers {
                triage: ModelTier::Local,
                review: ModelTier::Local,
                ..super::super::policy::ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };

        let registry = registry_for_policy(&policy);

        // Rewiring TriageTaskNode/ConsolidatedReviewNode's transport must not
        // change the registry's node count or identity set — only the
        // transport those two nodes' composed `ClaudeCodeStep` uses.
        assert_eq!(registry.len(), super::registry().len());
        assert!(registry.contains("TriageTaskNode"));
        assert!(registry.contains("ConsolidatedReviewNode"));
        assert!(registry.contains("ImplementTaskNode"));
    }

    #[test]
    fn registry_for_policy_never_rewires_implement_task_node() {
        // ImplementTaskNode has no `model_tiers.implement == Local` branch in
        // `registry_for_policy` at all — the agentic implement stage is out
        // of scope for the `local` tier (spec Context Pointers). This test
        // documents that by construction: a policy with every tier set to
        // `Local`, including `implement`, still leaves `registry_for_policy`
        // with only the triage/review branches to rewire.
        let policy = SdlcPolicy {
            model_tiers: super::super::policy::ModelTiers {
                implement: ModelTier::Local,
                triage: ModelTier::Local,
                review: ModelTier::Local,
                ..super::super::policy::ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };

        let registry = registry_for_policy(&policy);
        assert!(registry.contains("ImplementTaskNode"));
    }

    #[test]
    fn declared_graph_has_no_dangling_or_unregistered_identity() {
        let schema = schema();
        let registry = registry();

        for (identity, config) in &schema.nodes {
            assert!(
                registry.contains(identity),
                "declared node '{identity}' is not registered"
            );
            for connection in &config.connections {
                assert!(
                    schema.nodes.contains_key(connection),
                    "'{identity}' declares a connection to unregistered/undeclared '{connection}'"
                );
            }
        }
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }
}
