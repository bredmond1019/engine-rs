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
//!                             | FinalValidationNode -> EndReviewNode -> EndReviewRouterNode
//!                                 -> { PatchDocsNode -> WrapUpNode | WrapUpNode }
//!                                 -> CloseBlockNode -> PullRequestNode -> EmitStateNode }
//!
//! EndReviewRouterNode -> { PatchDocsNode | WrapUpNode }
//!
//! TriageRouterNode    -> { ConsolidatedReviewNode | UpdateTaskStatusNode
//!                        | IncrementAttemptNode | WrapUpNode }
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
//!
//! **`ImplementTaskNode -> TestTaskNode` stays a single plain edge**
//! (`ticket-implement-node-transport-retry`, shape (A)): a `claude_code_rs`
//! transport failure inside `ImplementTaskNode` (and every other
//! `ClaudeCodeStep`-backed node — `TriageTaskNode`, `ConsolidatedReviewNode`,
//! `GenerateTasksNode`, `PatchDocsNode`) is retried with backoff *inside*
//! `ClaudeCodeStep::process` before it ever returns `Err`, so this file's
//! graph topology does not gain a branch point for it. Only a persistent
//! transport failure (retry budget exhausted, or a non-retryable
//! `claude_code_rs::Error` variant) still surfaces as a plain `NodeError`
//! and halts the walk exactly as before. See
//! `crates/engine-core/src/nodes/claude_code_step.rs` and this ticket's
//! Amendment Log for the retryable/non-retryable classification.

use std::collections::HashMap;
use std::sync::Arc;

use claude_code_rs::Config;

use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_meta_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::close_block::CloseBlockNode;
use super::docs::PatchDocsNode;
use super::emit_state::EmitStateNode;
use super::end_review::{EndReviewNode, EndReviewRouterNode};
use super::final_validation::FinalValidationNode;
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
        NodeConfig::new("TriageTaskNode", vec!["TriageRouterNode".to_string()]),
    );
    nodes.insert(
        "TriageRouterNode".to_string(),
        NodeConfig::new(
            "TriageRouterNode",
            vec![
                "ConsolidatedReviewNode".to_string(),
                // `UpdateTaskStatusNode` is the review-skipping `PASS`
                // target: `route`'s `PASS` arm returns it under
                // `ReviewMode::EndOnly` (always) and under
                // `ReviewMode::TrivialSkip` (trivial diffs). It was an
                // UNDECLARED edge until 2026-08-30 — runs worked, because
                // nothing enforces declared connections at walk time, but
                // the published `GET /workflows/{type}/graph` shape and any
                // reachability analysis over it were wrong for two of the
                // three review modes. Keep this list a superset of every
                // target `TriageRouterNode::route` can return; the
                // `router_connections_declared` integration test enforces it.
                "UpdateTaskStatusNode".to_string(),
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
        "FinalValidationNode".to_string(),
        NodeConfig::new("FinalValidationNode", vec!["EndReviewNode".to_string()]),
    );
    // `EndReviewNode` (`EN.ticket.review-mode-endonly-reviews-nothing`):
    // the drain-branch, end-of-run review. Always present in the declared
    // graph — under `PerTask`/`TrivialSkip` it is a zero-call pass-through
    // (standing rule 6: a policy knob must not change the declared node
    // set). `EndReviewRouterNode` decides PatchDocs vs WrapUp from its
    // stamped verdict (or the pass-through absence of one).
    nodes.insert(
        "EndReviewNode".to_string(),
        NodeConfig::new("EndReviewNode", vec!["EndReviewRouterNode".to_string()]),
    );
    nodes.insert(
        "EndReviewRouterNode".to_string(),
        NodeConfig::new(
            "EndReviewRouterNode",
            vec!["PatchDocsNode".to_string(), "WrapUpNode".to_string()],
        ),
    );
    nodes.insert(
        "PatchDocsNode".to_string(),
        NodeConfig::new("PatchDocsNode", vec!["WrapUpNode".to_string()]),
    );
    nodes.insert(
        "WrapUpNode".to_string(),
        NodeConfig::new("WrapUpNode", vec!["CloseBlockNode".to_string()]),
    );
    // `CloseBlockNode` sits between `WrapUpNode` and `PullRequestNode` —
    // ORDER IS LOAD-BEARING (`EN.ticket.wrap-up-closes-the-block` task 5):
    // the close must land BEFORE `EmitStateNode` re-derives the freshness
    // spine, matching base-template's JS `sdlc-flow.js` step 2b (close)
    // then 2c (emit). `mev::set_block_status` itself chains into an
    // in-process `emit_state(root, true, None)` on a successful write
    // (`mev/src/lib.rs:977`), so by the time this graph's own
    // `EmitStateNode` subprocess-runs `mev emit-state --write` a moment
    // later, the derived views (master-plan tables, project caches, tier
    // rollups, boards, …) are ALREADY fresh off `CloseBlockNode`'s call.
    // The two do not fight: `emit_state` recomputes every derived view
    // fully from the current authored `state.json` each time it runs, so
    // it is idempotent — a second run just re-derives the same views from
    // the same (now-closed) authored state and re-writes byte-identical
    // output. `EmitStateNode`'s subprocess run is authoritative for
    // freshness purposes (it runs last, after `PullRequestNode` may have
    // patched the `pr` block, and its own `patch_pr_into_state` step
    // depends on running after the write), but neither run corrupts or
    // undoes the other — this is deliberate redundancy, not a race, since
    // `CloseBlockNode` holds mev's own advisory `.mev-emit.lock` for the
    // duration of its write and releases it before `EmitStateNode` starts.
    nodes.insert(
        "CloseBlockNode".to_string(),
        NodeConfig::new("CloseBlockNode", vec!["PullRequestNode".to_string()]),
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
///
/// `ImplementTaskNode` and `PatchDocsNode` — the two nodes that actually
/// mutate the worktree via an agentic tool-use session — are granted real
/// headless write permission via [`agentic_write_config`] so a `bastion
/// serve`-mounted `/sdlc-flow` run is not a silent no-op on the codebase
/// (see `planning/decisions/D8-autonomous-node-write-permission.md`).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SetupWorktreeNode::new()));
    registry.register(Box::new(SpecExistsRouterNode::new()));
    registry.register(Box::new(GenerateTasksNode::new()));
    registry.register(Box::new(LoadTaskStateNode::new()));
    registry.register(Box::new(TaskQueueRouterNode));
    registry.register(Box::new(
        ImplementTaskNode::new().with_config(agentic_write_config("claude-sonnet-4-5")),
    ));
    registry.register(Box::new(TestTaskNode::new()));
    registry.register(Box::new(TriageTaskNode::new()));
    registry.register(Box::new(TriageRouterNode));
    registry.register(Box::new(ConsolidatedReviewNode::new()));
    registry.register(Box::new(ReviewRouterNode));
    registry.register(Box::new(UpdateTaskStatusNode));
    registry.register(Box::new(SaveStateNode::new()));
    registry.register(Box::new(IncrementAttemptNode));
    // Unconditional, deterministic run-level gate — not policy-gated (CLAUDE.md
    // standing rule 6: a policy knob must never change the declared node set).
    // Its depth is pinned to `TestDepth::Full`; see `planning/decisions/
    // D12-per-task-vs-final-check-depth.md`.
    registry.register(Box::new(FinalValidationNode::new()));
    // `EndReviewNode`/`EndReviewRouterNode` — always registered; behavior is
    // gated on the resolved policy, not the graph's declared node set (see
    // schema()'s comment above).
    registry.register(Box::new(EndReviewNode::new()));
    registry.register(Box::new(EndReviewRouterNode));
    // The model string here is a fallback only: `PatchDocsNode::process`
    // overwrites it from the resolved `model_tiers.docs` tier (whose
    // built-in default resolves to this same string). This registration
    // exists for the write grant.
    registry.register(Box::new(
        PatchDocsNode::new().with_config(agentic_write_config("claude-sonnet-4-5")),
    ));
    registry.register(Box::new(WrapUpNode::new()));
    registry.register(Box::new(CloseBlockNode::new()));
    registry.register(Box::new(PullRequestNode::new()));
    registry.register(Box::new(EmitStateNode::new()));
    registry
}

/// Shared `Config` for the two nodes that need real headless write
/// permission to an agentic `claude-code-rs` session — `ImplementTaskNode`
/// and `PatchDocsNode`. Matches the proven `sdlc_flow_live.rs` config
/// (`dangerously_skip_permissions: true`, `disallowed_tools: ["Bash"]`,
/// `isolated: true`): file edits only, no shell/network execution, no
/// bleed into another interactive session. `model` is the given tier's
/// resolved model string, and for BOTH nodes it is overwritten again by
/// `process` per the resolved policy — `ImplementTaskNode` via
/// `apply_policy(.., Stage::Implement)` and `PatchDocsNode` via
/// `apply_policy_config(.., Stage::Docs)` — so passing a fixed default here
/// is safe and purely a fallback. What this function actually contributes is
/// the write grant, not the model.
///
/// See `planning/decisions/D8-autonomous-node-write-permission.md` for the
/// safety tradeoff this configuration encodes.
#[must_use]
pub fn agentic_write_config(model: &str) -> Config {
    Config {
        model: Some(model.to_string()),
        disallowed_tools: vec!["Bash".to_string()],
        dangerously_skip_permissions: true,
        isolated: true,
        ..Config::default()
    }
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
/// through [`openai_compat_meta_transport_live`] whenever `policy`'s resolved
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
///
/// `FinalValidationNode` has no branch here, deliberately: it is a pure
/// `CommandRunner` consumer with no `Config` and no `ModelTransport`, so the
/// `local` model tier has nothing on it to rewire.
#[must_use]
pub fn registry_for_policy(policy: &SdlcPolicy) -> NodeRegistry {
    let mut registry = registry();

    if policy.model_tiers.triage == ModelTier::Local {
        registry.register(Box::new(TriageTaskNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
        )));
    }

    if policy.model_tiers.review == ModelTier::Local {
        registry.register(Box::new(ConsolidatedReviewNode::new().with_meta_transport(
            openai_compat_meta_transport_live(policy.local.clone(), real_cloud_transport()),
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
    fn registry_contains_all_twenty_two_nodes_incl_bottom_half() {
        // Was misnamed "...eighteen..." while actually asserting nineteen
        // (`EN.ticket.wrap-up-closes-the-block` task 5 notes this rather
        // than compounding it); twenty with `CloseBlockNode` added between
        // `WrapUpNode` and `PullRequestNode`; now twenty-two with
        // `EndReviewNode`/`EndReviewRouterNode`
        // (`EN.ticket.review-mode-endonly-reviews-nothing`) added between
        // `FinalValidationNode` and `PatchDocsNode`.
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
            "FinalValidationNode",
            "EndReviewNode",
            "EndReviewRouterNode",
            "PatchDocsNode",
            "WrapUpNode",
            "CloseBlockNode",
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
    fn schema_contains_final_validation_node() {
        let schema = schema();
        assert!(schema.nodes.contains_key("FinalValidationNode"));
    }

    #[test]
    fn final_validation_node_sits_on_the_drain_path() {
        let schema = schema();

        let router = schema
            .nodes
            .get("TaskQueueRouterNode")
            .expect("TaskQueueRouterNode declared");
        assert!(router
            .connections
            .contains(&"FinalValidationNode".to_string()));
        assert!(!router.connections.contains(&"PatchDocsNode".to_string()));

        let final_validation = schema
            .nodes
            .get("FinalValidationNode")
            .expect("FinalValidationNode declared");
        assert_eq!(
            final_validation.connections,
            vec!["EndReviewNode".to_string()]
        );

        let end_review = schema
            .nodes
            .get("EndReviewNode")
            .expect("EndReviewNode declared");
        assert_eq!(
            end_review.connections,
            vec!["EndReviewRouterNode".to_string()]
        );

        let end_review_router = schema
            .nodes
            .get("EndReviewRouterNode")
            .expect("EndReviewRouterNode declared");
        assert!(end_review_router
            .connections
            .contains(&"PatchDocsNode".to_string()));
        assert!(end_review_router
            .connections
            .contains(&"WrapUpNode".to_string()));
    }

    /// The graph's declared node set must be IDENTICAL across every
    /// `review_mode` — `EndReviewNode`/`EndReviewRouterNode` are always
    /// present, only their *behavior* is policy-gated (standing rule 6).
    /// `registry_for_policy` never adds/removes an identity for any
    /// `review_mode`, so this asserts against the plain `registry()`/
    /// `schema()` pair directly rather than driving a full run per mode.
    #[test]
    fn node_set_is_identical_across_all_review_modes() {
        use super::super::policy::ReviewMode;

        let base_registry = registry();
        for mode in [
            ReviewMode::PerTask,
            ReviewMode::TrivialSkip,
            ReviewMode::EndOnly,
        ] {
            let policy = SdlcPolicy {
                review_mode: mode,
                ..SdlcPolicy::default()
            };
            let policy_registry = registry_for_policy(&policy);
            assert_eq!(
                policy_registry.len(),
                base_registry.len(),
                "node count must be identical under {mode:?}"
            );
            assert!(policy_registry.contains("EndReviewNode"));
            assert!(policy_registry.contains("EndReviewRouterNode"));
        }
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

    #[test]
    fn agentic_write_config_grants_headless_write_permission() {
        let config = agentic_write_config("claude-sonnet-4-5");

        assert!(config.dangerously_skip_permissions);
        assert!(config.disallowed_tools.iter().any(|tool| tool == "Bash"));
        assert!(config.isolated);
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-5"));
    }

    #[test]
    fn registry_still_validates_and_contains_all_identities_after_write_grant() {
        // registry() now grants ImplementTaskNode/PatchDocsNode headless
        // write permission via agentic_write_config; assert that grant did
        // not disturb the registry's identity set or schema validation.
        let schema = schema();
        let registry = registry();

        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");

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
            "FinalValidationNode",
            "EndReviewNode",
            "EndReviewRouterNode",
            "PatchDocsNode",
            "WrapUpNode",
            "CloseBlockNode",
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
}
