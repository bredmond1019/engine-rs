//! Fixture 3-node linear workflow integration test (EN.1.A task 4).
//!
//! Exercises the public `engine-core` API end-to-end: three trivial fixture
//! `Node` impls, registered and wired into a linear `WorkflowSchema`
//! (start -> node2 -> node3 via `connections[0]`), run through
//! `Workflow::run`. Covers both the full-success walk and a middle-node
//! failure that halts the walk before the third node runs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};

/// A trivial node that stamps a marker into `TaskContext::nodes` under its own
/// identity, so each step's output is independently observable.
struct MarkerNode {
    identity: &'static str,
}

impl Node for MarkerNode {
    fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            self.identity.to_string(),
            serde_json::json!({ "ran": self.identity }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// A node that always fails, used to exercise the halt-on-failure path.
struct FailingNode {
    identity: &'static str,
}

impl Node for FailingNode {
    fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new("node2 exploded"))
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// Builds the linear 3-node schema: start_node -> node2 -> node3 (terminal),
/// wired via `connections[0]` only.
fn linear_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        "start_node".to_string(),
        NodeConfig::new("start_node", vec!["node2".to_string()]),
    );
    nodes.insert(
        "node2".to_string(),
        NodeConfig::new("node2", vec!["node3".to_string()]),
    );
    nodes.insert("node3".to_string(), NodeConfig::new("node3", vec![]));

    WorkflowSchema::new("linear-3", "start_node", nodes)
}

fn success_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(MarkerNode {
        identity: "start_node",
    }));
    registry.register(Box::new(MarkerNode { identity: "node2" }));
    registry.register(Box::new(MarkerNode { identity: "node3" }));
    registry
}

#[test]
fn linear_workflow_runs_end_to_end_through_all_three_nodes() {
    let workflow = Workflow::new(success_registry(), linear_schema());

    let snapshots: Rc<RefCell<Vec<TaskContext>>> = Rc::new(RefCell::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    let on_progress: engine_core::OnProgress<'_> =
        Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

    let result = workflow
        .run(serde_json::json!({ "trigger": "test" }), on_progress)
        .expect("workflow should complete successfully");

    // (a) The produced TaskContext has the expected nodes/node_runs entries.
    assert_eq!(
        result.nodes.get("start_node"),
        Some(&serde_json::json!({ "ran": "start_node" }))
    );
    assert_eq!(
        result.nodes.get("node2"),
        Some(&serde_json::json!({ "ran": "node2" }))
    );
    assert_eq!(
        result.nodes.get("node3"),
        Some(&serde_json::json!({ "ran": "node3" }))
    );
    assert_eq!(result.node_runs.len(), 3);

    // (b) Every node's NodeRun transitioned PENDING -> RUNNING -> SUCCESS with
    // started_at <= completed_at.
    for identity in ["start_node", "node2", "node3"] {
        let run = result
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("node_run for {identity} should be present"));
        assert_eq!(run.status, NodeRunStatus::Success);
        assert!(run.started_at.is_some(), "{identity} started_at unset");
        assert!(run.completed_at.is_some(), "{identity} completed_at unset");
        assert!(
            run.started_at.unwrap() <= run.completed_at.unwrap(),
            "{identity} started_at should be <= completed_at"
        );
        assert!(run.error.is_none(), "{identity} should have no error");
    }

    // (c) All three nodes were seeded PENDING before the first ran — captured
    // via the initial on_progress snapshot.
    let snapshots = snapshots.borrow();
    let initial = snapshots
        .first()
        .expect("on_progress should have been called at least once");
    assert_eq!(initial.node_runs.len(), 3);
    for identity in ["start_node", "node2", "node3"] {
        let run = initial
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("initial snapshot missing {identity}"));
        assert_eq!(run.status, NodeRunStatus::Pending);
        assert!(run.started_at.is_none());
        assert!(run.completed_at.is_none());
    }
    // The initial snapshot must precede any node's output being written.
    assert!(initial.nodes.is_empty());
}

#[test]
fn middle_node_failure_halts_walk_before_third_node_runs() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(MarkerNode {
        identity: "start_node",
    }));
    registry.register(Box::new(FailingNode { identity: "node2" }));
    registry.register(Box::new(MarkerNode { identity: "node3" }));

    let workflow = Workflow::new(registry, linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .expect("run should return Ok with the accumulated context even on node failure");

    // start_node ran to completion.
    let start_run = result
        .node_runs
        .get("start_node")
        .expect("start_node run present");
    assert_eq!(start_run.status, NodeRunStatus::Success);
    assert!(result.nodes.contains_key("start_node"));

    // node2 is FAILED with error set.
    let node2_run = result.node_runs.get("node2").expect("node2 run present");
    assert_eq!(node2_run.status, NodeRunStatus::Failed);
    assert_eq!(node2_run.error.as_deref(), Some("node2 exploded"));
    assert!(node2_run.started_at.is_some());
    assert!(node2_run.completed_at.is_some());
    assert!(!result.nodes.contains_key("node2"));

    // node3 never ran: still PENDING, no output written.
    let node3_run = result.node_runs.get("node3").expect("node3 run present");
    assert_eq!(node3_run.status, NodeRunStatus::Pending);
    assert!(node3_run.started_at.is_none());
    assert!(node3_run.completed_at.is_none());
    assert!(!result.nodes.contains_key("node3"));
}
