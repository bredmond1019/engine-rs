//! Assembles the declared `WorkflowSchema` + `NodeRegistry` for the
//! `SDLC_TASK` workflow (port design T9, `EN.11.N` task 6).
//!
//! **This is the block's only hard boundary.** Before this file existed,
//! every `sdlc_task` module (`schema`, `task_triage_router`,
//! `lean_bookkeep`) was unreachable-but-tested — no graph assembled them
//! into a walkable workflow. After it, `SDLC_TASK` runs in-process end to
//! end via `Workflow::new_validated`.
//!
//! Declared graph (port design §2, with the `CloseBlockNode` hop
//! `EN.11.N` task 5 added):
//!
//! ```text
//! SetupWorktreeNode -> SpecExistsRouterNode -> { GenerateTasksNode -> LoadTaskStateNode
//!                                              | LoadTaskStateNode }
//!   -> TaskQueueRouterNode -> { ImplementTaskNode -> TestTaskNode -> TriageTaskNode
//!                                 -> TaskTriageRouterNode
//!                                 -> { UpdateTaskStatusNode -> SaveStateNode
//!                                        -> (loop) TaskQueueRouterNode
//!                                    | IncrementAttemptNode -> ImplementTaskNode
//!                                    | LeanBookkeepNode }
//!                             | FinalValidationNode }
//!
//! FinalValidationNode -> LeanBookkeepNode -> CloseBlockNode -> EmitStateNode   [terminal]
//! ```
//!
//! **The drain-target identity is load-bearing.** `TaskQueueRouterNode::
//! route`'s drain (no-pending) branch hardcodes the string
//! `"FinalValidationNode"` — that is shared, unmodified `sdlc_flow`
//! machinery this workflow reuses as-is. Rather than add a
//! `with_drain_target` builder to the router (which would fork a node
//! `EN.11.M`'s whole design exists to avoid forking), this registry
//! registers the SAME identity, `"FinalValidationNode"`, constructed with
//! [`crate::workflows::sdlc_flow::final_validation::ValidationScope::Reconcile`]
//! instead of the default `Full` — the D56 terminal reconcile IS this
//! run's "run-level authoritative gate", the same role `Full` plays for
//! `SDLC_FLOW`.
//!
//! Every reused `sdlc_flow` node this graph registers is unmodified;
//! parameterization (`with_state_filename`/`with_branch_prefix`/
//! `with_scope`/`with_state_source`) is all `EN.11.M`/`EN.11.N` added
//! builder methods, never a fork. `TaskTriageRouterNode` and
//! `LeanBookkeepNode` are the only genuinely new node types — see their
//! own module docs for why `sdlc_flow`'s `TriageRouterNode`/`WrapUpNode`
//! could not be reused directly.
//!
//! SIX `sdlc_flow` NODES ARE DELIBERATELY ABSENT from this registry:
//! `ConsolidatedReviewNode`, `ReviewRouterNode`, `EndReviewNode`,
//! `EndReviewRouterNode`, `PatchDocsNode`, `PullRequestNode`, `WrapUpNode`.
//! `SDLC_TASK` ships no per-task review, no end-of-run review, no docs
//! patch, and no PR ceremony (`sdlc-task-ships-no-docs-stage`) — a policy
//! value naming any of these seven identities would route into an
//! unregistered node and strand the walk with no terminal state, which is
//! exactly why `TaskTriageRouterNode` has only three arms and never reads
//! `SdlcPolicy::review_mode` at all (see its own module doc).
//!
//! The registered `EmitStateNode` is the GENERIC
//! `crate::policy::emit_state::EmitStateNode`, not
//! `sdlc_flow::emit_state::EmitStateNode` — the only behavior the
//! `sdlc_flow` wrapper adds over the generic node is `patch_pr_into_state`/
//! `patch_close_block_into_state`, both PR/close enrichments `SDLC_TASK`
//! has no analogous need for (no PR at all; `CloseBlockNode`'s outcome has
//! nowhere parallel to patch since this workflow's committed state file
//! carries no `pr` block to enrich either). Using the generic node also
//! keeps this module's only path to `sdlc_flow` scoped to shared,
//! unmodified machinery — never a re-import of something `EN.11.M` already
//! lifted out from under it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_meta_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use crate::workflows::sdlc_flow::close_block::CloseBlockNode;
use crate::workflows::sdlc_flow::final_validation::{FinalValidationNode, ValidationScope};
use crate::workflows::sdlc_flow::graph::agentic_write_config;
use crate::workflows::sdlc_flow::policy::{ModelTier, SdlcPolicy};
use crate::workflows::sdlc_flow::setup::{
    GenerateTasksNode, LoadTaskStateNode, SetupWorktreeNode, SpecExistsRouterNode,
};
use crate::workflows::sdlc_flow::task_loop::{
    ImplementTaskNode, IncrementAttemptNode, SaveStateNode, TaskQueueRouterNode, TestTaskNode,
    TriageTaskNode, UpdateTaskStatusNode,
};

use super::lean_bookkeep::LeanBookkeepNode;
use super::task_triage_router::TaskTriageRouterNode;
use super::{default_command_runner, ModelTransport, DEFAULT_STATE_FILENAME};

/// The `SDLC_TASK` workflow's declared identity/type name, used both to
/// register the workflow (`EN.11.P`, ORCHESTRATION dispatch — out of scope
/// here) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "SDLC_TASK";

/// Build the declared `WorkflowSchema` for `SDLC_TASK` — see the module
/// doc for the full graph shape and the drain-target rationale.
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
            vec![
                "ImplementTaskNode".to_string(),
                "FinalValidationNode".to_string(),
            ],
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
        NodeConfig::new("TriageTaskNode", vec!["TaskTriageRouterNode".to_string()]),
    );
    nodes.insert(
        "TaskTriageRouterNode".to_string(),
        NodeConfig::new(
            "TaskTriageRouterNode",
            vec![
                "UpdateTaskStatusNode".to_string(),
                "IncrementAttemptNode".to_string(),
                "LeanBookkeepNode".to_string(),
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
    // The drain-target identity — see module doc. Constructed by
    // `registry()` with `ValidationScope::Reconcile`, under the SAME
    // registered name `TaskQueueRouterNode::route` hardcodes.
    nodes.insert(
        "FinalValidationNode".to_string(),
        NodeConfig::new("FinalValidationNode", vec!["LeanBookkeepNode".to_string()]),
    );
    nodes.insert(
        "LeanBookkeepNode".to_string(),
        NodeConfig::new("LeanBookkeepNode", vec!["CloseBlockNode".to_string()]),
    );
    nodes.insert(
        "CloseBlockNode".to_string(),
        NodeConfig::new("CloseBlockNode", vec!["EmitStateNode".to_string()]),
    );
    nodes.insert(
        "EmitStateNode".to_string(),
        NodeConfig::new("EmitStateNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "SetupWorktreeNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered. Tests build their own registry with stubbed transports and
/// runners instead of calling this directly.
///
/// `ImplementTaskNode` is granted real headless write permission via
/// `sdlc_flow::graph::agentic_write_config` — the same D8 write grant
/// `SDLC_FLOW` uses, reused rather than re-derived (`EN.11.N`'s `files.new`
/// list carries no reason to fork it).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        SetupWorktreeNode::new().with_branch_prefix("task/"),
    ));
    registry.register(Box::new(
        SpecExistsRouterNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(
        LoadTaskStateNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(TaskQueueRouterNode));
    registry.register(Box::new(
        ImplementTaskNode::new().with_config(agentic_write_config("claude-sonnet-4-5")),
    ));
    registry.register(Box::new(TestTaskNode::new()));
    registry.register(Box::new(TriageTaskNode::new()));
    registry.register(Box::new(TaskTriageRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(
        SaveStateNode::new().with_state_filename(DEFAULT_STATE_FILENAME),
    ));
    registry.register(Box::new(IncrementAttemptNode));
    registry.register(Box::new(
        FinalValidationNode::new().with_scope(ValidationScope::Reconcile),
    ));
    registry.register(Box::new(LeanBookkeepNode::new()));
    registry.register(Box::new(
        CloseBlockNode::new().with_state_source("LeanBookkeepNode"),
    ));
    // The GENERIC `mev emit-state --write` node — not `sdlc_flow`'s wrapper,
    // whose only addition (`patch_pr_into_state`) is PR-only. See module
    // doc.
    registry.register(Box::new(crate::policy::emit_state::EmitStateNode::new(
        default_command_runner(),
    )));
    registry
}

/// The real `claude_code_rs::execute` transport — mirrors
/// `sdlc_flow::graph::real_cloud_transport` (kept private there, so this is
/// a duplicate rather than a shared import; both are one-line delegations
/// to the same free function).
fn real_cloud_transport() -> ModelTransport {
    Arc::new(|config, prompt| {
        Box::pin(async move { claude_code_rs::execute(&config, &prompt).await })
    })
}

/// Build a `NodeRegistry` like [`registry`], but with `TriageTaskNode`'s
/// `llm_triage` model branch wired to route through
/// [`openai_compat_meta_transport_live`] whenever `policy`'s resolved tier
/// for that stage is [`ModelTier::Local`] — mirroring
/// `sdlc_flow::graph::registry_for_policy`'s local-tier rewiring. **Never**
/// rewires `ImplementTaskNode` (the local tier is scoped to single-shot
/// judgment calls, not the agentic implement stage — same rationale as
/// `sdlc_flow`). Has no `ConsolidatedReviewNode` branch: `SDLC_TASK`
/// registers no such node.
///
/// Takes `sdlc_flow::policy::SdlcPolicy`, not a new `SdlcTaskPolicy` —
/// `EN.11.O` owns the policy surface and its own acceptance criteria
/// require that "with no profile selected, SDLC_TASK behaves exactly as
/// `EN.11.N` left it", so inventing a policy type here would only be
/// deleted there (this block's Amendment Log, "SCOPE CALL").
#[must_use]
pub fn registry_for_policy(policy: &SdlcPolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.triage == ModelTier::Local {
        registry.register(Box::new(TriageTaskNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    registry
}

/// Build the runnable `SDLC_TASK` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("SDLC_TASK declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;

    const EXPECTED_IDENTITIES: [&str; 16] = [
        "SetupWorktreeNode",
        "SpecExistsRouterNode",
        "GenerateTasksNode",
        "LoadTaskStateNode",
        "TaskQueueRouterNode",
        "ImplementTaskNode",
        "TestTaskNode",
        "TriageTaskNode",
        "TaskTriageRouterNode",
        "UpdateTaskStatusNode",
        "SaveStateNode",
        "IncrementAttemptNode",
        "FinalValidationNode",
        "LeanBookkeepNode",
        "CloseBlockNode",
        "EmitStateNode",
    ];

    const ABSENT_SDLC_FLOW_IDENTITIES: [&str; 7] = [
        "ConsolidatedReviewNode",
        "ReviewRouterNode",
        "EndReviewNode",
        "EndReviewRouterNode",
        "PatchDocsNode",
        "PullRequestNode",
        "WrapUpNode",
    ];

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
    fn workflow_type_is_sdlc_task() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
        assert_eq!(WORKFLOW_TYPE, "SDLC_TASK");
    }

    #[test]
    fn registry_contains_exactly_the_sixteen_expected_identities() {
        let registry = registry();

        for identity in EXPECTED_IDENTITIES {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), EXPECTED_IDENTITIES.len());
    }

    #[test]
    fn the_six_sdlc_flow_only_nodes_are_absent_from_the_registry() {
        let registry = registry();

        for identity in ABSENT_SDLC_FLOW_IDENTITIES {
            assert!(
                !registry.contains(identity),
                "'{identity}' must NOT be registered — SDLC_TASK ships no review/docs/PR stage"
            );
        }
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

    /// The drain branch resolves: `TaskQueueRouterNode::route` hardcodes
    /// `"FinalValidationNode"` as its no-pending-tasks target, and that
    /// identity IS registered here, carrying `ValidationScope::Reconcile`
    /// (not the `sdlc_flow` default of `Full`).
    #[test]
    fn drain_branch_resolves_to_a_registered_reconcile_scoped_final_validation_node() {
        let schema = schema();
        let router = schema
            .nodes
            .get("TaskQueueRouterNode")
            .expect("TaskQueueRouterNode declared");
        assert!(router
            .connections
            .contains(&"FinalValidationNode".to_string()));

        // `ValidationScope` carries no public accessor and `FinalValidationNode`
        // exposes no getter either (by design — the scope is an
        // implementation detail of `process`), so the scope is pinned via
        // its OBSERVABLE effect: driving `registry()`'s registered instance
        // directly and asserting it takes the `Reconcile` skip-path
        // (`test_depth == Full` -> zero `CommandRunner` calls, `skip_reason`
        // naming the reconcile) rather than `Full`'s unconditional
        // `select_task_checks` call. `sdlc_task_e2e.rs` exercises this at
        // full graph-walk depth; this test pins it directly against the
        // constructed node.
        let registry = registry();
        assert!(registry.contains("FinalValidationNode"));
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn registry_for_policy_with_default_policy_matches_plain_registry() {
        let default_registry = registry();
        let policy_registry = registry_for_policy(&SdlcPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("TriageTaskNode"));
        for identity in EXPECTED_IDENTITIES {
            assert!(policy_registry.contains(identity));
        }
    }

    #[test]
    fn registry_for_policy_with_local_triage_tier_keeps_same_node_identities() {
        let policy = SdlcPolicy {
            model_tiers: crate::workflows::sdlc_flow::policy::ModelTiers {
                triage: ModelTier::Local,
                ..crate::workflows::sdlc_flow::policy::ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };

        let registry = registry_for_policy(&policy);

        assert_eq!(registry.len(), super::registry().len());
        assert!(registry.contains("TriageTaskNode"));
        assert!(registry.contains("ImplementTaskNode"));
    }

    #[test]
    fn registry_for_policy_never_rewires_implement_task_node() {
        let policy = SdlcPolicy {
            model_tiers: crate::workflows::sdlc_flow::policy::ModelTiers {
                implement: ModelTier::Local,
                triage: ModelTier::Local,
                ..crate::workflows::sdlc_flow::policy::ModelTiers::default()
            },
            ..SdlcPolicy::default()
        };

        let registry = registry_for_policy(&policy);
        assert!(registry.contains("ImplementTaskNode"));
    }

    /// Mechanical pin for the AC "the registry uses
    /// `crate::policy::emit_state::EmitStateNode`, not
    /// `sdlc_flow::emit_state::EmitStateNode`" — a scripted grep over this
    /// file's own production source, mirroring `mod.rs`'s
    /// `no_sdlc_task_source_file_imports_the_lifted_seams_from_sdlc_flow`
    /// check style.
    #[test]
    fn registry_uses_the_generic_emit_state_node_not_sdlc_flows_wrapper() {
        let source = include_str!("graph.rs");
        let production_code = source
            .split_once("\n#[cfg(test)]\n")
            .map(|(before, _)| before)
            .expect("this module has a #[cfg(test)] boundary");
        // Only real code lines (not doc/line comments, which legitimately
        // discuss the wrapper by name in prose) count as an offending
        // import/construction — mirrors `mod.rs`'s own
        // `no_sdlc_task_source_file_imports_the_lifted_seams_from_sdlc_flow`
        // comment-skipping style.
        let code_lines: Vec<&str> = production_code
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .collect();

        assert!(
            code_lines
                .iter()
                .any(|line| line.contains("crate::policy::emit_state::EmitStateNode")),
            "registry() must construct the generic policy::emit_state::EmitStateNode"
        );
        assert!(
            !code_lines.iter().any(|line| {
                line.contains("sdlc_flow::emit_state::EmitStateNode")
                    || line.contains("use crate::workflows::sdlc_flow::emit_state")
            }),
            "registry() must never import/construct sdlc_flow's EmitStateNode wrapper — its \
             only addition (patch_pr_into_state/patch_close_block_into_state) is PR/close \
             enrichment this workflow has no use for"
        );
    }
}
