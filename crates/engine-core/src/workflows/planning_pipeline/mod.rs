//! `planning_pipeline` — the `PLANNING_PIPELINE` workflow (`EN.19.D`): one
//! dispatchable entry point that runs any contiguous slice of
//! `pre_plan -> plan -> generate_tasks -> dispatch`, pausing for an operator
//! decision between every pair of adjacent requested stages (task 7).
//!
//! # Module layout
//!
//! - `stage_selector` — [`stage_selector::StageSelectorNode`], validates a
//!   dispatched event's `stages` list (task 3).
//! - `policy` — [`policy::ApprovalGatePolicy`], the four-layer-resolved
//!   per-stage `approval`/`approval_channel`/`max_discussion_rounds` knobs
//!   (task 2).
//! - `approval_gate` — [`approval_gate::ApprovalGateNode`], built on
//!   `crate::operator::*` (task 4).
//! - `generate_tasks_for_block` — [`generate_tasks_for_block::GenerateTasksForBlockNode`],
//!   composes `EN.19.C`'s `GenerateTasksNode` (task 5).
//! - `dispatch` — [`dispatch::DispatchNode`], the authored-status guard plus
//!   the existing dispatch path (task 6).
//! - This module (`mod.rs`) — composes `EN.19.A`'s `pre_plan::*` and
//!   `EN.19.B`'s `plan_authoring::*` node sets (by import, never a fork)
//!   with the four modules above into the one `PLANNING_PIPELINE` graph
//!   (task 7).
//!
//! # Graph shape
//!
//! Built **per dispatched `stages` request** by [`schema_for_stages`] /
//! [`registry_for_stages`] — this workflow has no single static schema the
//! way `pre_plan`/`plan_authoring` do, because which nodes even exist in
//! the graph depends on which contiguous stage slice was requested (a
//! `stages: [pre_plan]` run has no gate at all; a full
//! `[pre_plan, plan, generate_tasks, dispatch]` run has three). Callers
//! (task 8's HTTP dispatcher) first resolve the validated, ordered stage
//! list — either by running [`stage_selector::StageSelectorNode`] or by
//! calling [`stage_selector::validate_stages`] directly — then pass that
//! list to [`schema_for_stages`]/[`registry_for_stages`] to build the
//! concrete per-run graph. [`stage_selector::NODE_NAME`] is still declared
//! as the schema's start node (a plain, non-router passthrough whose sole
//! declared connection is the first requested stage's own entry node) so a
//! rejected `stages` list still surfaces through the same node identity
//! task 3's own tests assert against, and so re-validating inside the walk
//! costs nothing.
//!
//! For each requested stage, in order:
//!
//! ```text
//! <stage entry> -> ... -> <stage terminal(s)>
//! ```
//!
//! is merged in verbatim from that stage's own module (`pre_plan::schema()`/
//! `plan_authoring::schema()` for `pre_plan`/`plan`; a single node for
//! `generate_tasks`/`dispatch`). Between two ADJACENT requested stages, this
//! module inserts one `ApprovalGateNode#<stage>` and rewires that stage's
//! terminal(s) to point at it instead of falling straight through — the
//! block record's "ApprovalGateNode inserted between every pair of adjacent
//! requested stages" (`EN.19.D.json`'s `what` field) and task 4's own doc
//! comment ("task 7's graph wiring inserts one instance per adjacent stage
//! pair"). The LAST requested stage's terminal(s) stay real terminals (no
//! gate wraps the end of the run) — a single-stage `stages: [pre_plan]`
//! dispatch therefore produces EXACTLY `pre_plan`'s own graph, with no gate
//! node at all, matching the block record's AC2 ("produces exactly EN.19.A's
//! own PRE_PLAN behavior").
//!
//! ## The gate's own loop — `build_loop`/`LoopSpec`, never a raw cyclic edge
//!
//! Per the block record's 2026-09-15 amendment, the edge out of each
//! `ApprovalGateNode#<stage>` is not a straight line to the next stage —
//! it is a [`crate::loop_combinator::build_loop`] cluster: the gate's
//! declared connection points at the cluster's guard, whose back-edge (via
//! the increment node) re-enters the SAME gate identity
//! (`LoopSpec::body_entry`), capped at the resolved
//! `ApprovalGatePolicy::max_discussion_rounds`, with `exit_to` the next
//! stage's entry node. This is structurally the same shape
//! `proposal_generator::graph`'s review/revise cluster already proves out
//! (`ProposalReviseNode -> {loop guard/increment} -> ProposalReviewNode
//! (continue) | PersistToBrainNode (cap reached)`): the guard/increment
//! nodes are both real `Router`s, so `WorkflowValidator`'s DFS cycle check
//! skips every edge out of them (D42) — the back-edge is a runtime router
//! edge, never a declared non-router cycle.
//!
//! Exactly like `proposal_generator`'s cluster, this cluster's own
//! `exit_predicate` is a pure cap backstop (`false`, never short-circuits on
//! its own) — an actual verdict-driven exit (an `approve` continuing past
//! the gate, a `reject` holding the run in a distinct terminal state, a
//! `discuss` looping back for another round) is `EN.19.E`'s own scope: the
//! `DiscussFurtherNode` seam this cluster's `body_entry` gives it a home to
//! plug into, and the `POST /events/{run_id}/resume` verdict wiring task 8
//! owns. Building the cluster now — rather than a straight-line edge task 8
//! or `EN.19.E` would later have to tear out and replace with a "fourth
//! hand-copy" of the same loop shape (the block record's own words) — is
//! exactly what the amendment asks for: the shape is proven and in place
//! before the node that will actually drive it exists.
//!
//! ## A known composition gap this task does not fix
//!
//! `plan_authoring::check_existing::CheckExistingPlanNode` is a `Router`
//! whose `route()` returns `None` at runtime on a short-circuit (see
//! `plan_authoring`'s own module doc: "a router's un-taken branch is simply
//! not walked, not a dead node") — there is no declared "already staged"
//! terminal identity the way `pre_plan::check_existing::EXISTS_ROUTE` gives
//! `pre_plan`. Composing `plan_authoring::schema()` unmodified therefore
//! means a `plan` stage that short-circuits (a slug already staged) ends
//! the walk right there, never reaching that stage's gate or any stage
//! after it. Fixing this needs an edit inside `plan_authoring` itself
//! (out of scope for `crates/engine-core/src/workflows/planning_pipeline/mod.rs`,
//! this task's only declared file) — flagged here at the source, per
//! `EN.19.B` task 7's own precedent of restating a known gap where the gap
//! actually lives, not only in a doc page.

pub mod approval_gate;
pub mod dispatch;
pub mod generate_tasks_for_block;
pub mod policy;
pub mod stage_selector;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::loop_combinator::{build_loop, ExitPredicate, LoopCluster, LoopSpec};
use crate::node::{NodeExt, NodeRegistry};
use crate::operator::queue::OperatorQueue;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::{plan_authoring, pre_plan};

use approval_gate::ApprovalGateNode;
use dispatch::DispatchNode;
use generate_tasks_for_block::GenerateTasksForBlockNode;
use policy::ApprovalGatePolicy;

/// The `PLANNING_PIPELINE` workflow's declared identity/type name.
pub const WORKFLOW_TYPE: &str = "PLANNING_PIPELINE";

/// One requested stage's own sub-graph: its entry identity (where the
/// previous stage's gate — or [`stage_selector::NODE_NAME`] for the first
/// requested stage — connects in), the terminal identities within it whose
/// declared connection this module rewires (to the next gate, or left empty
/// for the last requested stage), and every `NodeConfig` that stage's own
/// module declares, merged in verbatim.
struct StageGraph {
    entry: String,
    terminals: Vec<String>,
    nodes: HashMap<String, NodeConfig>,
}

fn pre_plan_stage_graph() -> StageGraph {
    StageGraph {
        entry: pre_plan::check_existing::NODE_NAME.to_string(),
        terminals: vec![
            pre_plan::check_existing::EXISTS_ROUTE.to_string(),
            pre_plan::write_notes::NODE_NAME.to_string(),
        ],
        nodes: pre_plan::schema().nodes,
    }
}

fn plan_stage_graph() -> StageGraph {
    StageGraph {
        entry: plan_authoring::check_existing::NODE_NAME.to_string(),
        // See this module's doc comment, "A known composition gap this task
        // does not fix": plan_authoring's short-circuit path has no
        // declared terminal identity to rewire, only the normal-path
        // terminal does.
        terminals: vec![plan_authoring::write_narrative::NODE_NAME.to_string()],
        nodes: plan_authoring::schema().nodes,
    }
}

fn generate_tasks_stage_graph() -> StageGraph {
    let identity = generate_tasks_for_block::NODE_NAME.to_string();
    let mut nodes = HashMap::new();
    nodes.insert(identity.clone(), NodeConfig::new(identity.clone(), vec![]));
    StageGraph {
        entry: identity.clone(),
        terminals: vec![identity],
        nodes,
    }
}

fn dispatch_stage_graph() -> StageGraph {
    let identity = dispatch::NODE_NAME.to_string();
    let mut nodes = HashMap::new();
    nodes.insert(identity.clone(), NodeConfig::new(identity.clone(), vec![]));
    StageGraph {
        entry: identity.clone(),
        terminals: vec![identity],
        nodes,
    }
}

/// Resolve `stage`'s own [`StageGraph`] — the one place this module maps a
/// canonical stage name (`stage_selector::CANONICAL_STAGE_ORDER`) onto the
/// node set that stage composes.
fn stage_graph_for(stage: &str) -> StageGraph {
    match stage {
        "pre_plan" => pre_plan_stage_graph(),
        "plan" => plan_stage_graph(),
        "generate_tasks" => generate_tasks_stage_graph(),
        "dispatch" => dispatch_stage_graph(),
        other => panic!(
            "planning_pipeline::stage_graph_for: unknown stage '{other}' — this is only ever \
             called with an already-validated CANONICAL_STAGE_ORDER member"
        ),
    }
}

/// This gate's `Node::name()`/`ctx.nodes` identity — mirrors
/// `approval_gate`'s own test precedent
/// (`ApprovalGateNode#plan`/`identity_override_relabels_the_stamp_for_multiple_gate_instances`)
/// so multiple gates in one run never collide.
fn gate_identity(stage: &str) -> String {
    format!("{}#{stage}", approval_gate::NODE_NAME)
}

/// The [`LoopSpec`] for `stage`'s gate cluster: body entry is the gate
/// itself (a `discuss` verdict re-enters the SAME gate for another round —
/// `EN.19.E`'s own `DiscussFurtherNode` plugs in ahead of this, not
/// required to change this spec), exit is `next_entry` (the following
/// stage's entry node). Capped at `max_discussion_rounds`; the predicate
/// itself never fires early — mirrors `proposal_generator::graph::revise_loop_spec`'s
/// cap-only backstop verbatim (see this module's own doc comment).
fn gate_loop_spec(stage: &str, next_entry: &str, max_discussion_rounds: u32) -> LoopSpec {
    let never_exits: ExitPredicate = Arc::new(|_ctx: &engine_contract::TaskContext| false);
    LoopSpec::new(
        format!("PlanningPipelineGate{stage}"),
        max_discussion_rounds.max(1),
        never_exits,
        gate_identity(stage),
        next_entry.to_string(),
    )
}

/// Build the declared `WorkflowSchema` for a `PLANNING_PIPELINE` run
/// dispatched with `stages` — already validated and ordered (e.g. by
/// [`stage_selector::validate_stages`]). Returns the same
/// `(reason_code, message)` [`stage_selector::validate_stages`] would on an
/// invalid list, so a caller that skips running
/// [`stage_selector::StageSelectorNode`] as an explicit first step still
/// gets the same named-reason rejection rather than a panic.
pub fn schema_for_stages(
    stages: &[String],
) -> Result<WorkflowSchema, (&'static str, String)> {
    let ordered = stage_selector::validate_stages(stages)?;

    let mut nodes: HashMap<String, NodeConfig> = HashMap::new();

    let first_entry = stage_graph_for(&ordered[0]).entry;
    nodes.insert(
        stage_selector::NODE_NAME.to_string(),
        NodeConfig::new(stage_selector::NODE_NAME, vec![first_entry]),
    );

    for (i, stage) in ordered.iter().enumerate() {
        let graph = stage_graph_for(stage);

        for (identity, config) in &graph.nodes {
            if graph.terminals.contains(identity) {
                continue;
            }
            nodes.insert(identity.clone(), config.clone());
        }

        if i + 1 < ordered.len() {
            let next_entry = stage_graph_for(&ordered[i + 1]).entry;
            let gate = gate_identity(stage);
            for terminal in &graph.terminals {
                nodes.insert(terminal.clone(), NodeConfig::new(terminal.clone(), vec![gate.clone()]));
            }

            // Placeholder max_discussion_rounds for schema shape only — the
            // resolved policy value is threaded through by
            // registry_for_stages; the cluster's DECLARED graph shape
            // (which identities exist and how they connect) is invariant
            // across every policy setting (standing rule 6), so the exact
            // cap value never changes which nodes/edges this schema
            // declares.
            let cluster = build_loop(gate_loop_spec(stage, &next_entry, ApprovalGatePolicy::default().max_discussion_rounds));
            nodes.insert(gate.clone(), NodeConfig::new(gate.clone(), vec![cluster.guard_identity.clone()]));
            nodes.extend(cluster.connections);
        } else {
            for terminal in &graph.terminals {
                nodes.insert(terminal.clone(), NodeConfig::new(terminal.clone(), vec![]));
            }
        }
    }

    Ok(WorkflowSchema::new(
        WORKFLOW_TYPE,
        stage_selector::NODE_NAME,
        nodes,
    ))
}

/// Register `pre_plan`'s own six nodes, one call each of the exact same
/// constructors `pre_plan::registry()` uses — composition, never a fork.
/// `NodeRegistry` exposes no merge/drain API (it is `.claude/workflows/`-
/// adjacent core plumbing this task's declared files never touch), so
/// merging another module's already-built registry wholesale is not
/// possible; re-listing its own constructor calls is the narrowest
/// composition this module's registry-building shape allows without
/// editing `crate::node::NodeRegistry` itself.
fn register_pre_plan_nodes(registry: &mut NodeRegistry) {
    registry.register(Box::new(pre_plan::check_existing::CheckExistingNotesNode::new()));
    registry.register(Box::new(pre_plan::PrePlanNotesAlreadyExistsNode::new()));
    registry.register(Box::new(pre_plan::intake::IntakeIdeaNode::new()));
    registry.register(Box::new(pre_plan::research::ResearchCodebaseNode::new()));
    registry.register(Box::new(pre_plan::secret_guard::SecretGuardNode::new(
        pre_plan::PrePlanPolicy::default(),
    )));
    registry.register(Box::new(pre_plan::write_notes::WriteNotesNode::new()));
}

/// Register `plan_authoring`'s own five nodes — see
/// [`register_pre_plan_nodes`]'s doc comment for why this re-lists
/// constructor calls rather than merging a pre-built `NodeRegistry`.
fn register_plan_authoring_nodes(registry: &mut NodeRegistry) {
    registry.register(Box::new(plan_authoring::check_existing::CheckExistingPlanNode::new()));
    registry.register(Box::new(plan_authoring::gather_context::GatherPlanContextNode::new()));
    registry.register(Box::new(plan_authoring::decompose::DecomposePlanNode::new()));
    registry.register(Box::new(
        plan_authoring::stage_candidate_blocks::StageCandidateBlocksNode::new(),
    ));
    registry.register(Box::new(
        plan_authoring::write_narrative::WritePlanNarrativeNode::new(),
    ));
}

/// Build a `NodeRegistry` matching [`schema_for_stages`]'s declared graph
/// for the same `stages`: every requested stage's own node set registered
/// via the exact constructors its own module's `registry()` uses
/// (composition, never a fork), one [`ApprovalGateNode`] per adjacent stage
/// pair (sharing `queue` and `policy`), and that gate's `build_loop`
/// cluster nodes.
pub fn registry_for_stages(
    stages: &[String],
    queue: Arc<Mutex<OperatorQueue>>,
    policy: &ApprovalGatePolicy,
) -> Result<NodeRegistry, (&'static str, String)> {
    let ordered = stage_selector::validate_stages(stages)?;

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(stage_selector::StageSelectorNode::new()));

    for (i, stage) in ordered.iter().enumerate() {
        match stage.as_str() {
            "pre_plan" => register_pre_plan_nodes(&mut registry),
            "plan" => register_plan_authoring_nodes(&mut registry),
            "generate_tasks" => {
                registry.register(Box::new(GenerateTasksForBlockNode::new()));
            }
            "dispatch" => {
                registry.register(Box::new(DispatchNode::new()));
            }
            other => unreachable!("validate_stages already rejects unknown stage '{other}'"),
        }

        if i + 1 < ordered.len() {
            let next_entry = stage_graph_for(&ordered[i + 1]).entry;
            let gate = ApprovalGateNode::new(stage.clone(), Arc::clone(&queue))
                .with_policy(policy.clone())
                .with_identity(gate_identity(stage));
            registry.register(Box::new(gate));

            let cluster: LoopCluster =
                build_loop(gate_loop_spec(stage, &next_entry, policy.max_discussion_rounds));
            for node in cluster.nodes {
                registry.register(node);
            }
        }
    }

    Ok(registry)
}

/// Build the runnable `PLANNING_PIPELINE` `Workflow` for `stages`:
/// [`registry_for_stages`] paired with [`schema_for_stages`], constructed
/// via `Workflow::new_validated` so assembly fails loudly if the composed
/// graph is not structurally sound.
pub fn workflow_for_stages(
    stages: &[String],
    queue: Arc<Mutex<OperatorQueue>>,
    policy: &ApprovalGatePolicy,
) -> Result<Workflow, String> {
    let schema = schema_for_stages(stages).map_err(|(_, message)| message)?;
    let registry = registry_for_stages(stages, queue, policy).map_err(|(_, message)| message)?;
    Workflow::new_validated(registry, schema)
        .map_err(|err| format!("PLANNING_PIPELINE composed graph failed validation: {err}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::operator::queue::{OperatorQueue, OperatorQueuePolicy};
    use crate::validate::WorkflowValidator;

    use super::*;

    fn stages(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn queue() -> Arc<Mutex<OperatorQueue>> {
        Arc::new(Mutex::new(OperatorQueue::new(OperatorQueuePolicy::default())))
    }

    #[test]
    fn schema_for_stages_rejects_the_same_shapes_validate_stages_does() {
        let err = schema_for_stages(&stages(&["pre_plan", "dispatch"])).expect_err("gap rejected");
        assert_eq!(err.0, stage_selector::reject_reason::GAP);

        let err = schema_for_stages(&stages(&[])).expect_err("empty rejected");
        assert_eq!(err.0, stage_selector::reject_reason::EMPTY);
    }

    #[test]
    fn single_stage_produces_exactly_that_stages_own_graph_with_no_gate() {
        let schema = schema_for_stages(&stages(&["pre_plan"])).expect("valid single-stage slice");

        // No ApprovalGateNode identity anywhere — a single requested stage
        // has no adjacent pair to gate between (AC2: "produces exactly
        // EN.19.A's own PRE_PLAN behavior").
        assert!(schema
            .nodes
            .keys()
            .all(|identity| !identity.starts_with(approval_gate::NODE_NAME)));

        // pre_plan's own two terminals are still terminals here.
        assert!(schema.nodes[pre_plan::write_notes::NODE_NAME]
            .connections
            .is_empty());
        assert!(schema.nodes[pre_plan::check_existing::EXISTS_ROUTE]
            .connections
            .is_empty());

        assert_eq!(schema.start_node, stage_selector::NODE_NAME);
        assert_eq!(
            schema.nodes[stage_selector::NODE_NAME].connections,
            vec![pre_plan::check_existing::NODE_NAME.to_string()]
        );
    }

    #[test]
    fn two_stage_slice_wires_exactly_one_gate_between_them() {
        let schema =
            schema_for_stages(&stages(&["pre_plan", "plan"])).expect("valid two-stage slice");

        let gate = gate_identity("pre_plan");
        assert!(schema.nodes.contains_key(&gate));
        // No gate after the LAST requested stage.
        assert!(!schema.nodes.contains_key(&gate_identity("plan")));

        // Both of pre_plan's terminals now point at the gate's guard, never
        // straight at the next stage.
        for terminal in [
            pre_plan::write_notes::NODE_NAME,
            pre_plan::check_existing::EXISTS_ROUTE,
        ] {
            assert_eq!(
                schema.nodes[terminal].connections,
                vec![gate.clone()],
                "terminal '{terminal}' must route into the gate, not straight to the next stage"
            );
        }

        // plan_authoring's own terminal stays a real terminal (last stage).
        assert!(schema.nodes[plan_authoring::write_narrative::NODE_NAME]
            .connections
            .is_empty());
    }

    #[test]
    fn gate_wires_through_build_loop_never_a_bespoke_cyclic_edge() {
        let schema =
            schema_for_stages(&stages(&["pre_plan", "plan"])).expect("valid two-stage slice");

        let gate = gate_identity("pre_plan");
        let cluster = build_loop(gate_loop_spec(
            "pre_plan",
            plan_authoring::check_existing::NODE_NAME,
            ApprovalGatePolicy::default().max_discussion_rounds,
        ));

        // The gate's own declared edge goes to the cluster's guard — not
        // directly to the next stage.
        assert_eq!(
            schema.nodes[&gate].connections,
            vec![cluster.guard_identity.clone()]
        );
        // Both cluster nodes (guard + increment) are declared in the
        // composed schema.
        assert!(schema.nodes.contains_key(&cluster.guard_identity));
        assert!(schema.nodes.contains_key(&cluster.increment_identity));
        // The guard's declared connections are exactly {increment (loop
        // back into the gate), next stage's entry (exit)}.
        let mut guard_connections = schema.nodes[&cluster.guard_identity].connections.clone();
        guard_connections.sort();
        let mut expected = vec![
            cluster.increment_identity.clone(),
            plan_authoring::check_existing::NODE_NAME.to_string(),
        ];
        expected.sort();
        assert_eq!(guard_connections, expected);
        // The increment node's back-edge re-enters the SAME gate identity
        // (LoopSpec::body_entry), never straight to the next stage.
        assert_eq!(
            schema.nodes[&cluster.increment_identity].connections,
            vec![gate]
        );
    }

    #[test]
    fn full_four_stage_slice_wires_three_gates() {
        let schema = schema_for_stages(&stages(&[
            "pre_plan",
            "plan",
            "generate_tasks",
            "dispatch",
        ]))
        .expect("full slice is valid");

        for stage in ["pre_plan", "plan", "generate_tasks"] {
            assert!(
                schema.nodes.contains_key(&gate_identity(stage)),
                "expected a gate after '{stage}'"
            );
        }
        assert!(!schema.nodes.contains_key(&gate_identity("dispatch")));

        assert!(schema.nodes[dispatch::NODE_NAME].connections.is_empty());
    }

    #[test]
    fn middle_contiguous_slice_starts_at_plan_and_wires_two_gates() {
        let schema = schema_for_stages(&stages(&["plan", "generate_tasks", "dispatch"]))
            .expect("middle-to-end slice is valid");

        assert_eq!(
            schema.start_node,
            stage_selector::NODE_NAME
        );
        assert_eq!(
            schema.nodes[stage_selector::NODE_NAME].connections,
            vec![plan_authoring::check_existing::NODE_NAME.to_string()]
        );
        assert!(schema.nodes.contains_key(&gate_identity("plan")));
        assert!(schema.nodes.contains_key(&gate_identity("generate_tasks")));
        assert!(!schema.nodes.contains_key(&gate_identity("dispatch")));
    }

    #[test]
    fn schema_declares_no_dangling_or_duplicate_identity_for_every_stage_slice() {
        let slices: Vec<Vec<&str>> = vec![
            vec!["pre_plan"],
            vec!["plan"],
            vec!["generate_tasks"],
            vec!["dispatch"],
            vec!["pre_plan", "plan"],
            vec!["plan", "generate_tasks"],
            vec!["generate_tasks", "dispatch"],
            vec!["pre_plan", "plan", "generate_tasks"],
            vec!["plan", "generate_tasks", "dispatch"],
            vec!["pre_plan", "plan", "generate_tasks", "dispatch"],
        ];

        for slice in slices {
            let schema = schema_for_stages(&stages(&slice))
                .unwrap_or_else(|_| panic!("slice {slice:?} should validate"));
            for (identity, config) in &schema.nodes {
                for connection in &config.connections {
                    assert!(
                        schema.nodes.contains_key(connection),
                        "slice {slice:?}: '{identity}' declares a connection to \
                         unregistered/undeclared '{connection}'"
                    );
                }
            }
        }
    }

    #[test]
    fn registry_for_stages_matches_schema_for_every_declared_identity() {
        let stage_list = stages(&["pre_plan", "plan", "generate_tasks", "dispatch"]);
        let schema = schema_for_stages(&stage_list).expect("valid full slice");
        let registry = registry_for_stages(&stage_list, queue(), &ApprovalGatePolicy::default())
            .expect("valid full slice");

        for identity in schema.nodes.keys() {
            assert!(
                registry.contains(identity),
                "declared node '{identity}' is not registered"
            );
        }
    }

    #[test]
    fn registry_for_stages_rejects_the_same_shapes_validate_stages_does() {
        match registry_for_stages(&stages(&[]), queue(), &ApprovalGatePolicy::default()) {
            Err((reason, _message)) => assert_eq!(reason, stage_selector::reject_reason::EMPTY),
            Ok(_) => panic!("empty stages list should be rejected"),
        }
    }

    #[test]
    fn workflow_for_stages_builds_a_validated_workflow_for_every_contiguous_slice() {
        for slice in [
            vec!["pre_plan"],
            vec!["pre_plan", "plan"],
            vec!["plan", "generate_tasks", "dispatch"],
            vec!["pre_plan", "plan", "generate_tasks", "dispatch"],
        ] {
            let stage_list = stages(&slice);
            let workflow = workflow_for_stages(&stage_list, queue(), &ApprovalGatePolicy::default());
            assert!(
                workflow.is_ok(),
                "slice {slice:?} should build a valid Workflow: {:?}",
                workflow.err()
            );
        }
    }

    #[test]
    fn declared_graph_passes_workflow_validator_for_every_contiguous_slice() {
        for slice in [
            vec!["pre_plan"],
            vec!["pre_plan", "plan"],
            vec!["plan", "generate_tasks", "dispatch"],
            vec!["pre_plan", "plan", "generate_tasks", "dispatch"],
        ] {
            let stage_list = stages(&slice);
            let schema = schema_for_stages(&stage_list).expect("valid slice");
            let registry = registry_for_stages(&stage_list, queue(), &ApprovalGatePolicy::default())
                .expect("valid slice");
            WorkflowValidator::validate(&registry, &schema)
                .unwrap_or_else(|err| panic!("slice {slice:?} failed validation: {err}"));
        }
    }
}
