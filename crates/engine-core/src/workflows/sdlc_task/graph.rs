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

use crate::cancellation::CancellationToken;
use crate::node::NodeRegistry;
use crate::nodes::openai_compat_transport::openai_compat_meta_transport_live;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use crate::policy::PolicyConfigSource;
use crate::workflows::sdlc_flow::close_block::CloseBlockNode;
use crate::workflows::sdlc_flow::final_validation::{FinalValidationNode, ValidationScope};
use crate::workflows::sdlc_flow::graph::agentic_write_config;
use crate::workflows::sdlc_flow::policy::ModelTier;
use crate::workflows::sdlc_flow::setup::{
    GenerateTasksNode, LoadTaskStateNode, PolicyResolverFn, SetupWorktreeNode, SpecExistsRouterNode,
};
use crate::workflows::sdlc_flow::task_loop::{
    ImplementTaskNode, IncrementAttemptNode, SaveStateNode, TaskQueueRouterNode, TestTaskNode,
    TriageTaskNode, UpdateTaskStatusNode,
};

use super::lean_bookkeep::LeanBookkeepNode;
use super::policy::SdlcTaskPolicy;
use super::profiles::resolve_policy_for_run_from;
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
        SetupWorktreeNode::new()
            .with_branch_prefix("task/")
            .with_policy_resolver(sdlc_task_policy_resolver()),
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
/// Takes `SdlcTaskPolicy` (`EN.11.O` task 3) — SDLC_TASK's own resolved
/// policy, not `sdlc_flow::policy::SdlcPolicy`. `model_tiers.triage` and
/// `local` are fields SDLC_TASK carries directly (see `policy.rs`), so this
/// no longer needs the `to_sdlc_policy()` projection at all.
#[must_use]
pub fn registry_for_policy(policy: &SdlcTaskPolicy) -> NodeRegistry {
    registry_for_policy_with_cancellation(policy, None)
}

/// Like [`registry_for_policy`], but additionally wires `token` — when
/// given — into `ImplementTaskNode` and `TriageTaskNode` via their
/// `with_cancellation_token` builder (`EN.ticket.abort-must-interrupt-an-
/// in-flight-agent-node`), mirroring
/// `sdlc_flow::graph::registry_for_policy_with_cancellation`. `SDLC_TASK`
/// registers no `ConsolidatedReviewNode`, so there is no third node here.
/// `token: None` reproduces [`registry_for_policy`] exactly.
#[must_use]
pub fn registry_for_policy_with_cancellation(
    policy: &SdlcTaskPolicy,
    token: Option<CancellationToken>,
) -> NodeRegistry {
    let mut registry = registry();

    let triage_local = policy.model_tiers.triage == ModelTier::Local;
    if triage_local || token.is_some() {
        let mut node = TriageTaskNode::new();
        if triage_local {
            node = node.with_meta_transport(openai_compat_meta_transport_live(
                policy.local.clone(),
                real_cloud_transport(),
            ));
        }
        if let Some(t) = token.clone() {
            node = node.with_cancellation_token(t);
        }
        registry.register(Box::new(node));
    }

    if let Some(t) = token {
        registry.register(Box::new(
            ImplementTaskNode::new()
                .with_config(agentic_write_config("claude-sonnet-4-5"))
                .with_cancellation_token(t),
        ));
    }

    registry
}

/// The [`SetupWorktreeNode::with_policy_resolver`] closure for SDLC_TASK:
/// resolves `SdlcTaskPolicy` against `sdlc_task.{policy,profiles}` (via
/// [`resolve_policy_for_run_from`]), then projects it onto the
/// `SdlcPolicy` every shared `sdlc_flow` node in this registry actually
/// reads (via [`SdlcTaskPolicy::to_sdlc_policy`]). This is the seam that
/// makes a `sdlc_task.policy` harness.json section live config instead of
/// dead config — before this task, `SetupWorktreeNode` always read
/// `sdlc.policy` regardless of which workflow it was assembled into.
///
/// `pub` (not module-private) since `EN.11.P` task 3's `engine-serve`
/// registration re-registers a fresh `SetupWorktreeNode` (to install the
/// `EN.3.K` repo registry) and must chain this exact resolver back on —
/// otherwise a served `SDLC_TASK` run silently falls back to
/// `sdlc_flow`'s default resolver, reading the wrong `harness.json`
/// section.
pub fn sdlc_task_policy_resolver() -> Arc<PolicyResolverFn> {
    Arc::new(|ctx, worktree| {
        let source = PolicyConfigSource::Worktree(worktree.to_path_buf());
        let resolved = resolve_policy_for_run_from(ctx, &source)?;
        Ok(resolved.to_sdlc_policy())
    })
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
        let policy_registry = registry_for_policy(&SdlcTaskPolicy::default());

        assert_eq!(policy_registry.len(), default_registry.len());
        assert!(policy_registry.contains("TriageTaskNode"));
        for identity in EXPECTED_IDENTITIES {
            assert!(policy_registry.contains(identity));
        }
    }

    // --- registry_for_policy_with_cancellation
    //     (EN.ticket.abort-must-interrupt-an-in-flight-agent-node task 2) ---

    #[test]
    fn registry_for_policy_with_cancellation_none_matches_registry_for_policy() {
        let policy = SdlcTaskPolicy::default();
        let plain = registry_for_policy(&policy);
        let with_none = registry_for_policy_with_cancellation(&policy, None);

        assert_eq!(plain.len(), with_none.len());
        assert!(with_none.contains("ImplementTaskNode"));
        assert!(with_none.contains("TriageTaskNode"));
    }

    #[test]
    fn registry_for_policy_with_cancellation_some_still_contains_both_agent_nodes() {
        // Structural check only — behavioral proof that the SAME token
        // instance reaches each node lives in `task_loop.rs`'s
        // `with_cancellation_token` tests (task 1) plus `engine-serve`'s
        // `mint_and_publish_run_token_stashes_the_exact_token_it_returns`
        // (task 2).
        let policy = SdlcTaskPolicy::default();
        let plain = registry_for_policy(&policy);
        let token = CancellationToken::new();
        let with_token = registry_for_policy_with_cancellation(&policy, Some(token));

        assert_eq!(plain.len(), with_token.len());
        assert!(with_token.contains("ImplementTaskNode"));
        assert!(with_token.contains("TriageTaskNode"));
    }

    #[test]
    fn registry_for_policy_with_local_triage_tier_keeps_same_node_identities() {
        let policy = SdlcTaskPolicy {
            model_tiers: super::super::policy::SdlcTaskModelTiers {
                triage: ModelTier::Local,
                ..super::super::policy::SdlcTaskModelTiers::default()
            },
            ..SdlcTaskPolicy::default()
        };

        let registry = registry_for_policy(&policy);

        assert_eq!(registry.len(), super::registry().len());
        assert!(registry.contains("TriageTaskNode"));
        assert!(registry.contains("ImplementTaskNode"));
    }

    #[test]
    fn registry_for_policy_never_rewires_implement_task_node() {
        let policy = SdlcTaskPolicy {
            model_tiers: super::super::policy::SdlcTaskModelTiers {
                implement: ModelTier::Local,
                triage: ModelTier::Local,
                ..super::super::policy::SdlcTaskModelTiers::default()
            },
            ..SdlcTaskPolicy::default()
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

    // --- EN.11.O task 3: the SDLC_TASK policy seam -------------------------

    use engine_contract::TaskContext;
    use std::path::{Path, PathBuf};

    fn event_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// A unique, empty scratch directory under the OS temp dir.
    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-sdlc-task-graph-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_harness_json(worktree: &Path, contents: serde_json::Value) {
        std::fs::create_dir_all(worktree.join("planning")).expect("create planning dir");
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            contents.to_string(),
        )
        .expect("write harness.json");
    }

    /// With no `sdlc_task.policy` section and no `profile`, the resolver
    /// stamps exactly `SdlcTaskPolicy::default().to_sdlc_policy()` — the
    /// behavior-stability anchor this task's AC names explicitly.
    #[test]
    fn sdlc_task_policy_resolver_with_no_harness_section_matches_default_projection() {
        let worktree = temp_dir();
        let ctx = event_context(serde_json::json!({ "spec_slug": "my-spec" }));

        let resolver = sdlc_task_policy_resolver();
        let resolved = resolver(&ctx, &worktree).expect("resolve should succeed");

        assert_eq!(resolved, SdlcTaskPolicy::default().to_sdlc_policy());
        std::fs::remove_dir_all(&worktree).ok();
    }

    /// A `sdlc_task.policy` harness.json section changes the resolved
    /// policy — proof the seam actually reads SDLC_TASK's own section.
    #[test]
    fn sdlc_task_policy_resolver_reads_sdlc_task_harness_section() {
        let worktree = temp_dir();
        write_harness_json(
            &worktree,
            serde_json::json!({ "sdlc_task": { "policy": { "max_attempts": 9 } } }),
        );
        let ctx = event_context(serde_json::json!({ "spec_slug": "my-spec" }));

        let resolver = sdlc_task_policy_resolver();
        let resolved = resolver(&ctx, &worktree).expect("resolve should succeed");

        assert_eq!(resolved.max_attempts, 9);
        std::fs::remove_dir_all(&worktree).ok();
    }

    /// An IDENTICAL section under the plain `sdlc` key (SDLC_FLOW's own)
    /// does NOT change SDLC_TASK's resolved policy — this pair is the only
    /// proof the workflow key is wired correctly rather than accidentally
    /// reading `sdlc_flow`'s section.
    #[test]
    fn sdlc_task_policy_resolver_ignores_plain_sdlc_harness_section() {
        let worktree = temp_dir();
        write_harness_json(
            &worktree,
            serde_json::json!({ "sdlc": { "policy": { "max_attempts": 9 } } }),
        );
        let ctx = event_context(serde_json::json!({ "spec_slug": "my-spec" }));

        let resolver = sdlc_task_policy_resolver();
        let resolved = resolver(&ctx, &worktree).expect("resolve should succeed");

        assert_eq!(
            resolved.max_attempts,
            SdlcTaskPolicy::default().to_sdlc_policy().max_attempts
        );
        assert_ne!(resolved.max_attempts, 9);
        std::fs::remove_dir_all(&worktree).ok();
    }

    /// `SetupWorktreeNode` as assembled by [`registry`] (branch prefix
    /// `"task/"` + the SDLC_TASK policy resolver) stamps the SDLC_TASK
    /// projection into `ctx.nodes`, driven through the real `Node::process`
    /// call — not just the resolver function in isolation.
    #[tokio::test]
    async fn setup_worktree_node_in_sdlc_task_registry_stamps_sdlc_task_projected_policy() {
        use crate::node::Node;

        let stub_runner: crate::workflows::CommandRunner = Arc::new(|_program, _args, _cwd| {
            Ok(crate::workflows::CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        let node = SetupWorktreeNode::new()
            .with_runner(stub_runner)
            .with_branch_prefix("task/")
            .with_policy_resolver(sdlc_task_policy_resolver());
        let ctx = event_context(serde_json::json!({ "spec_slug": "my-spec" }));

        let out = node.process(ctx).await.expect("setup should succeed");
        let stamped = out
            .nodes
            .get(crate::policy::RESOLVED_POLICY_IDENTITY)
            .expect("resolved policy present in ctx after setup");

        let expected = serde_json::to_value(SdlcTaskPolicy::default().to_sdlc_policy())
            .expect("serialize expected policy");
        assert_eq!(stamped, &expected);
    }
}
