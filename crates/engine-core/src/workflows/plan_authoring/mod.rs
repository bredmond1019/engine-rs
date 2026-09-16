//! `plan_authoring` — the `PLAN_AUTHORING` sub-workflow (`EN.19.B`): a
//! Node-composed port of `.claude/commands/plan.md`'s single-repo path
//! that turns a `notes.md`/`sequence.md` pre-plan folder into a rendered
//! `plan.md` narrative plus schema-valid candidate block records staged
//! for human review — it never calls `mev create-block --write` itself
//! (see this block's own `why`/`out_of_scope` in
//! `planning/blocks/EN.19.B.json`).
//!
//! # Stage-not-register
//!
//! Every candidate block this workflow produces lands under
//! `$BRAIN_ROOT/planning/open-work/pre-plan/<slug>/candidate-blocks/` as
//! schema-validated JSON — **not** in `planning/blocks/`, and no node here
//! ever shells or links against `mev create-block`. Registering a staged
//! candidate for real is a deliberate, separate, human-reviewed action
//! this block hands off to (see `EN.19.B.json`'s `out_of_scope`): a
//! single-model decomposition with no red-team or initiative-wide
//! consistency pass is a known, accepted quality gap, which is exactly why
//! nothing here is allowed to write into `state.json` or `planning/blocks/`
//! on its own.
//!
//! # Module layout
//!
//! Each leaf module is owned by a task in `planning/EN.19.B/tasks.json`:
//! - `check_existing` — [`CheckExistingPlanNode`], short-circuits a repeat
//!   run against an already-staged slug unless `force_regenerate: true`
//!   (task 1). Also owns the shared [`check_existing::PlanAuthoringFs`]
//!   seam and [`check_existing::pre_plan_dir`] helper both this module's
//!   file-touching nodes use.
//! - `gather_context` — [`gather_context::GatherPlanContextNode`], reads
//!   `CLAUDE.md`/`planning/context.md`/`planning/state.json` plus the
//!   pre-plan folder (task 1).
//! - `decompose` — [`decompose::DecomposePlanNode`], the one model-calling
//!   stage (task 2).
//! - `stage_candidate_blocks` — [`stage_candidate_blocks::StageCandidateBlocksNode`],
//!   mints a candidate id per proposed block, validates it against
//!   `.claude/workflows/block.schema.json`'s required-field/enum contract,
//!   and writes each to `candidate-blocks/<ID>.json` — never `mev
//!   create-block`, never `state.json` (task 3).
//! - `write_narrative` — [`write_narrative::WritePlanNarrativeNode`],
//!   renders `plan.md` from `StageCandidateBlocksNode`'s staged records,
//!   matching `.claude/commands/plan.md`'s Output Format (task 4).
//!
//! `WORKFLOW_TYPE`, the assembled `WorkflowSchema`/`NodeRegistry`, and the
//! `register_plan_authoring` wiring into `crates/engine-serve/src/workflows.rs`
//! all land in task 5 — this module exports the task-1/2/3/4 nodes today.
//!
//! # Declared graph shape (task 5)
//!
//! ```text
//! CheckExistingPlanNode -> { <short-circuit terminal> (Router::route -> None)
//!                           | GatherPlanContextNode -> DecomposePlanNode
//!                             -> StageCandidateBlocksNode -> WritePlanNarrativeNode }
//! ```
//!
//! `CheckExistingPlanNode` is a [`crate::routing::Router`]: its declared
//! `connections` name only the continue edge
//! (`check_existing::CONTINUE_TARGET`, i.e. `GatherPlanContextNode`), and its
//! `Router::route` returns `None` at runtime to end the walk right there
//! when `short_circuit` is true — the terminal case has no node of its own
//! (mirrors `content_pipeline::graph`'s own router-terminal precedent: a
//! router's un-taken branch is simply not walked, not a dead node).
//! `WritePlanNarrativeNode` is this graph's sole terminal — it declares no
//! outbound connection.
//!
//! Unlike `content_pipeline`/`research_agent`, this workflow has no
//! `policy`/`profiles` module of its own yet (no node here is
//! `ModelTier`-rewireable at runtime — `DecomposePlanNode`'s
//! [`decompose::DecomposePlanNode::with_transport`]/`with_meta_transport`
//! exist per standing rule 11 but nothing in this crate calls them yet), so
//! [`registry`] returns one fixed, real-transport registry — mirroring
//! `terminal_probe::graph`/`recall::graph`'s "model-free" registration
//! shape (`crates/engine-serve/src/workflows.rs`'s `register_terminal_probe`/
//! `register_recall`) rather than `content_pipeline::graph`'s
//! `registry_for_policy`. `planning/harness.json`'s own `plan_authoring`
//! section is documentation/consistency only for now, exactly like
//! `pre_plan`'s own section says of itself.
//!
//! # On the deferred `ExistsGuardNode` extraction
//!
//! `EN.19.B.json`'s 3rd amendment asks that [`check_existing::CheckExistingPlanNode`]'s
//! "exists()-check a path, short-circuit unless `force_regenerate`" shape be
//! factored out into a shared generic alongside `EN.19.A`'s
//! `CheckExistingNotesNode`, extracted *before* this node is written, not
//! after. As of this task, `EN.19.A` is BLOCKED (`planning/status.md`,
//! 2026-09-15 entry) and `crates/engine-core/src/workflows/pre_plan/` does
//! not exist anywhere in this tree — there is no second concrete instance
//! yet to extract a generic *from*. Building a generic off one instance
//! would be speculative abstraction, not reuse, so `check_existing.rs` is
//! written directly. The extraction remains the right call once `EN.19.A`
//! actually lands; whichever of the two blocks lands second should migrate
//! onto a shared `ExistsGuardNode` rather than leaving two hand-copies in
//! the tree.

pub mod check_existing;
pub mod decompose;
pub mod gather_context;
pub mod stage_candidate_blocks;
pub mod write_narrative;

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use check_existing::CheckExistingPlanNode;
use decompose::DecomposePlanNode;
use gather_context::GatherPlanContextNode;
use stage_candidate_blocks::StageCandidateBlocksNode;
use write_narrative::WritePlanNarrativeNode;

/// The `PLAN_AUTHORING` workflow's declared identity/type name, used both to
/// register the workflow (`engine-serve`) and as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "PLAN_AUTHORING";

/// Build the declared `WorkflowSchema` for the `PLAN_AUTHORING` workflow —
/// see this module's doc comment for the declared graph shape.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        check_existing::NODE_NAME.to_string(),
        NodeConfig::new(
            check_existing::NODE_NAME,
            vec![check_existing::CONTINUE_TARGET.to_string()],
        ),
    );
    nodes.insert(
        gather_context::NODE_NAME.to_string(),
        NodeConfig::new(
            gather_context::NODE_NAME,
            vec![decompose::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        decompose::NODE_NAME.to_string(),
        NodeConfig::new(
            decompose::NODE_NAME,
            vec![stage_candidate_blocks::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        stage_candidate_blocks::NODE_NAME.to_string(),
        NodeConfig::new(
            stage_candidate_blocks::NODE_NAME,
            vec![write_narrative::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        write_narrative::NODE_NAME.to_string(),
        NodeConfig::new(write_narrative::NODE_NAME, vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, check_existing::NODE_NAME, nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, each with its default (real-transport/real-filesystem)
/// configuration. Tests build their own registry with stubbed
/// filesystem/transport seams instead of calling this directly.
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(CheckExistingPlanNode::new()));
    registry.register(Box::new(GatherPlanContextNode::new()));
    registry.register(Box::new(DecomposePlanNode::new()));
    registry.register(Box::new(StageCandidateBlocksNode::new()));
    registry.register(Box::new(WritePlanNarrativeNode::new()));
    registry
}

/// Build the runnable `PLAN_AUTHORING` `Workflow`: [`registry`] paired with
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
        .expect("PLAN_AUTHORING declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod graph_tests {
    use super::*;
    use crate::node::Node;
    use crate::validate::WorkflowValidator;

    const ALL_NODE_IDENTITIES: [&str; 5] = [
        "CheckExistingPlanNode",
        "GatherPlanContextNode",
        "DecomposePlanNode",
        "StageCandidateBlocksNode",
        "WritePlanNarrativeNode",
    ];

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn start_node_is_check_existing_plan() {
        assert_eq!(schema().start_node, "CheckExistingPlanNode");
    }

    #[test]
    fn workflow_type_is_plan_authoring() {
        assert_eq!(schema().workflow_type, WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_every_declared_node() {
        let registry = registry();
        for identity in ALL_NODE_IDENTITIES {
            assert!(
                registry.contains(identity),
                "expected registry to contain '{identity}'"
            );
        }
        assert_eq!(registry.len(), ALL_NODE_IDENTITIES.len());
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
    fn check_existing_plan_declares_the_continue_edge_only() {
        let schema = schema();
        assert_eq!(
            schema.nodes["CheckExistingPlanNode"].connections,
            vec!["GatherPlanContextNode".to_string()]
        );
    }

    #[test]
    fn write_narrative_is_terminal() {
        let schema = schema();
        assert!(schema.nodes["WritePlanNarrativeNode"]
            .connections
            .is_empty());
    }

    #[test]
    fn check_existing_plan_is_registered_as_a_router() {
        let node = CheckExistingPlanNode::new();
        assert!(node.as_router().is_some());
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }
}
