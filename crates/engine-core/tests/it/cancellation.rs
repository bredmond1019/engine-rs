//! Fixture 3-node linear workflow integration test for cancellation
//! (EN.2.B task 3).
//!
//! A `CancelingNode` triggers a shared `CancellationToken` from inside its
//! own `process` call, simulating an external abort (task 5's HTTP handler)
//! landing mid-run. `Workflow::run_with` checks the token at the node
//! boundary *before* dispatching the next node, so the walk halts before
//! `node2` runs, `node2`/`node3` stay `Pending`, and the cancelled marker
//! lands in `TaskContext::metadata` per D6
//! (`planning/decisions/D6-cancellation-and-budget-semantics.md`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::{
    CancellationToken, Node, NodeConfig, NodeError, NodeRegistry, RunOptions, Workflow,
    WorkflowSchema, CANCELLATION_METADATA_KEY,
};

/// A trivial node that stamps a marker into `TaskContext::nodes`, so a run's
/// output is independently observable per step.
struct MarkerNode {
    identity: &'static str,
}

#[async_trait::async_trait]
impl Node for MarkerNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
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

/// A node that triggers cancellation on the shared token as a side effect of
/// running, then completes successfully itself — simulating an abort request
/// landing while this node was in flight.
struct CancelingNode {
    identity: &'static str,
    token: CancellationToken,
}

#[async_trait::async_trait]
impl Node for CancelingNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.token.cancel();
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

/// start_node -> node2 -> node3 (terminal), wired via `connections[0]` only.
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

#[tokio::test]
async fn cancellation_mid_run_halts_at_next_boundary_and_stamps_metadata() {
    let token = CancellationToken::new();

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(CancelingNode {
        identity: "start_node",
        token: token.clone(),
    }));
    registry.register(Box::new(MarkerNode { identity: "node2" }));
    registry.register(Box::new(MarkerNode { identity: "node3" }));

    let workflow = Workflow::new(registry, linear_schema());

    let snapshots: Rc<RefCell<Vec<TaskContext>>> = Rc::new(RefCell::new(Vec::new()));
    let snapshots_handle = snapshots.clone();
    let on_progress: engine_core::OnProgress<'_> =
        Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

    let result = workflow
        .run_with(
            serde_json::json!({}),
            on_progress,
            RunOptions {
                cancellation_token: Some(token.clone()),
                budget: None,
                pause_signal: None,
            },
        )
        .await
        .expect("a cancelled run returns Ok, not Err");

    // start_node ran to completion (it's the one that triggered the cancel).
    let start_run = result
        .node_runs
        .get("start_node")
        .expect("start_node run present");
    assert_eq!(start_run.status, NodeRunStatus::Success);
    assert!(result.nodes.contains_key("start_node"));

    // node2/node3 never dispatched: still Pending, no output written.
    for identity in ["node2", "node3"] {
        let run = result
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("{identity} run present"));
        assert_eq!(run.status, NodeRunStatus::Pending, "{identity} status");
        assert!(run.started_at.is_none(), "{identity} started_at");
        assert!(!result.nodes.contains_key(identity), "{identity} output");
    }

    // The cancelled marker is recorded in TaskContext::metadata (D6) — not a
    // NodeRunStatus variant.
    let cancellation = &result.metadata[CANCELLATION_METADATA_KEY];
    assert_eq!(cancellation["cancelled"], serde_json::json!(true));
    assert!(cancellation["at"].is_string());

    // The last on_progress snapshot reflects the halted state too.
    let snapshots = snapshots.borrow();
    let last = snapshots.last().expect("at least one snapshot");
    assert_eq!(
        last.metadata[CANCELLATION_METADATA_KEY]["cancelled"],
        serde_json::json!(true)
    );
}

#[tokio::test]
async fn no_cancellation_token_behaves_like_uninterrupted_run() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(MarkerNode {
        identity: "start_node",
    }));
    registry.register(Box::new(MarkerNode { identity: "node2" }));
    registry.register(Box::new(MarkerNode { identity: "node3" }));

    let workflow = Workflow::new(registry, linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run_with(serde_json::json!({}), on_progress, RunOptions::default())
        .await
        .expect("run should complete");

    for identity in ["start_node", "node2", "node3"] {
        let run = result.node_runs.get(identity).expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
    }
    assert!(result.metadata.get(CANCELLATION_METADATA_KEY).is_none());
}
