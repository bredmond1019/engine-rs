//! Integration tests for `ParallelNode` fan-out/merge (EN.1.B task 2).
//!
//! Confirms the documented deterministic last-write-wins merge: on a
//! `nodes`/`node_runs` key collision between branches, the branch declared
//! later in `ParallelNode::new`'s branch list wins; disjoint keys from every
//! branch all survive the merge.

use std::collections::HashMap;

use engine_contract::{NodeRun, NodeRunStatus, TaskContext};
use engine_core::{Node, NodeError, ParallelNode};

fn empty_context() -> TaskContext {
    TaskContext {
        event: serde_json::json!({}),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

fn success_run() -> NodeRun {
    NodeRun {
        status: NodeRunStatus::Success,
        started_at: None,
        completed_at: None,
        error: None,
        input: None,
        usage: None,
    }
}

/// A branch node that stamps a fixed value under a fixed key into both
/// `nodes` and `node_runs`.
struct WriterBranch {
    identity: &'static str,
    write_key: &'static str,
    value: serde_json::Value,
}

impl Node for WriterBranch {
    fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.write_key.to_string(), self.value.clone());
        ctx.node_runs
            .insert(self.write_key.to_string(), success_run());
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

#[test]
fn fanout_merge_resolves_key_collision_deterministically() {
    let branches: Vec<Box<dyn Node>> = vec![
        Box::new(WriterBranch {
            identity: "BranchOne",
            write_key: "Shared",
            value: serde_json::json!({ "winner": false }),
        }),
        Box::new(WriterBranch {
            identity: "BranchTwo",
            write_key: "Shared",
            value: serde_json::json!({ "winner": true }),
        }),
    ];
    let fanout = ParallelNode::new("Fanout", branches);

    let out = fanout
        .process(empty_context())
        .expect("fan-out/merge should succeed");

    // BranchTwo is declared after BranchOne in the branch list, so per the
    // documented deterministic last-write-wins order, it wins the collision
    // on both maps.
    assert_eq!(
        out.nodes.get("Shared"),
        Some(&serde_json::json!({ "winner": true }))
    );
    assert_eq!(
        out.node_runs.get("Shared").map(|r| r.status),
        Some(NodeRunStatus::Success)
    );

    // Run it again to confirm the winner is reproducible, not incidental.
    let out2 = fanout
        .process(empty_context())
        .expect("fan-out/merge should succeed");
    assert_eq!(
        out2.nodes.get("Shared"),
        Some(&serde_json::json!({ "winner": true }))
    );
}

#[test]
fn fanout_merge_preserves_all_disjoint_branch_keys() {
    let branches: Vec<Box<dyn Node>> = vec![
        Box::new(WriterBranch {
            identity: "BranchA",
            write_key: "AlphaKey",
            value: serde_json::json!({ "from": "alpha" }),
        }),
        Box::new(WriterBranch {
            identity: "BranchB",
            write_key: "BetaKey",
            value: serde_json::json!({ "from": "beta" }),
        }),
        Box::new(WriterBranch {
            identity: "BranchC",
            write_key: "GammaKey",
            value: serde_json::json!({ "from": "gamma" }),
        }),
    ];
    let fanout = ParallelNode::new("Fanout", branches);

    let out = fanout
        .process(empty_context())
        .expect("fan-out/merge should succeed");

    assert_eq!(
        out.nodes.get("AlphaKey"),
        Some(&serde_json::json!({ "from": "alpha" }))
    );
    assert_eq!(
        out.nodes.get("BetaKey"),
        Some(&serde_json::json!({ "from": "beta" }))
    );
    assert_eq!(
        out.nodes.get("GammaKey"),
        Some(&serde_json::json!({ "from": "gamma" }))
    );

    for key in ["AlphaKey", "BetaKey", "GammaKey"] {
        assert_eq!(
            out.node_runs.get(key).map(|r| r.status),
            Some(NodeRunStatus::Success),
            "expected node_run for {key}"
        );
    }
}
