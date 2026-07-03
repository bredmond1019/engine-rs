//! `ParallelNode` — fan-out/merge over a fixed, ordered set of branch nodes.
//!
//! `ParallelNode` is itself a `Node`: its `process` deep-copies (`clone`) the
//! incoming `TaskContext` once per branch, runs every branch concurrently via
//! `futures::future::join_all` (polled in-place on the current task, so
//! borrowed `&self.branches` needs neither `Send` nor `'static`), and merges
//! each branch's `nodes` + `node_runs` maps back into the parent.
//!
//! **Merge semantics — deterministic last-write-wins:** branches are merged in
//! their declared order (the order they were passed to
//! [`ParallelNode::new`]); on a key collision the **later branch in that
//! declared order wins**. Disjoint keys from every branch all survive the
//! merge untouched.

use std::collections::HashMap;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};

/// A node that fans out to a fixed, ordered list of branch nodes, runs them
/// in parallel over a cloned `TaskContext` each, and merges their `nodes` +
/// `node_runs` output back into a single `TaskContext`.
///
/// On key collision between branches, the **later branch in declared order**
/// wins (deterministic last-write-wins) — see the module docs.
pub struct ParallelNode {
    identity: String,
    branches: Vec<Box<dyn Node>>,
}

impl ParallelNode {
    /// Build a `ParallelNode` under `identity`, fanning out to `branches` in
    /// the given order. That declared order is the tie-break order used by
    /// the merge: later branches win on key collision.
    pub fn new(identity: impl Into<String>, branches: Vec<Box<dyn Node>>) -> Self {
        Self {
            identity: identity.into(),
            branches,
        }
    }
}

#[async_trait::async_trait]
impl Node for ParallelNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // One cloned TaskContext per branch — branches never observe each
        // other's writes.
        let branch_inputs: Vec<TaskContext> = self.branches.iter().map(|_| ctx.clone()).collect();

        // Run every branch concurrently via `join_all`, which polls the
        // branch futures in-place on the current task — no `Send`/`'static`
        // bound is required, so borrowed `&self.branches` still works.
        let branch_results: Vec<Result<TaskContext, NodeError>> = futures::future::join_all(
            self.branches
                .iter()
                .zip(branch_inputs)
                .map(|(node, input)| node.process(input)),
        )
        .await;

        // Propagate the first branch failure, if any, as this node's error.
        let mut branch_outputs = Vec::with_capacity(branch_results.len());
        for result in branch_results {
            match result {
                Ok(out) => branch_outputs.push(out),
                Err(err) => return Err(err),
            }
        }

        let mut merged = ctx;
        let mut merged_nodes: HashMap<String, serde_json::Value> = HashMap::new();
        let mut merged_node_runs: HashMap<String, engine_contract::NodeRun> = HashMap::new();

        // Merge in declared branch order: a later branch's entry overwrites
        // an earlier branch's entry on key collision (last-write-wins).
        for branch_ctx in branch_outputs {
            for (key, value) in branch_ctx.nodes {
                merged_nodes.insert(key, value);
            }
            for (key, value) in branch_ctx.node_runs {
                merged_node_runs.insert(key, value);
            }
        }

        merged.nodes.extend(merged_nodes);
        merged.node_runs.extend(merged_node_runs);

        Ok(merged)
    }

    fn name(&self) -> &str {
        &self.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::{NodeRun, NodeRunStatus};

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn run(status: NodeRunStatus) -> NodeRun {
        NodeRun {
            status,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        }
    }

    /// A branch node that writes a fixed value under a fixed key into both
    /// `nodes` and `node_runs`, so tests can assert merge outcomes precisely.
    struct WriterBranch {
        identity: &'static str,
        write_key: &'static str,
        value: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl Node for WriterBranch {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.write_key.to_string(), self.value.clone());
            ctx.node_runs
                .insert(self.write_key.to_string(), run(NodeRunStatus::Success));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            self.identity
        }
    }

    #[tokio::test]
    async fn collision_key_resolves_to_later_declared_branch() {
        let branches: Vec<Box<dyn Node>> = vec![
            Box::new(WriterBranch {
                identity: "BranchA",
                write_key: "Shared",
                value: serde_json::json!({ "from": "A" }),
            }),
            Box::new(WriterBranch {
                identity: "BranchB",
                write_key: "Shared",
                value: serde_json::json!({ "from": "B" }),
            }),
        ];
        let parallel = ParallelNode::new("Fanout", branches);

        let out = parallel
            .process(empty_context())
            .await
            .expect("process should succeed");

        // BranchB is declared after BranchA, so it wins the collision.
        assert_eq!(
            out.nodes.get("Shared"),
            Some(&serde_json::json!({ "from": "B" }))
        );
    }

    #[tokio::test]
    async fn disjoint_keys_from_every_branch_all_survive_merge() {
        let branches: Vec<Box<dyn Node>> = vec![
            Box::new(WriterBranch {
                identity: "BranchA",
                write_key: "KeyA",
                value: serde_json::json!({ "from": "A" }),
            }),
            Box::new(WriterBranch {
                identity: "BranchB",
                write_key: "KeyB",
                value: serde_json::json!({ "from": "B" }),
            }),
            Box::new(WriterBranch {
                identity: "BranchC",
                write_key: "KeyC",
                value: serde_json::json!({ "from": "C" }),
            }),
        ];
        let parallel = ParallelNode::new("Fanout", branches);

        let out = parallel
            .process(empty_context())
            .await
            .expect("process should succeed");

        assert_eq!(
            out.nodes.get("KeyA"),
            Some(&serde_json::json!({ "from": "A" }))
        );
        assert_eq!(
            out.nodes.get("KeyB"),
            Some(&serde_json::json!({ "from": "B" }))
        );
        assert_eq!(
            out.nodes.get("KeyC"),
            Some(&serde_json::json!({ "from": "C" }))
        );

        assert_eq!(
            out.node_runs.get("KeyA").map(|r| r.status),
            Some(NodeRunStatus::Success)
        );
        assert_eq!(
            out.node_runs.get("KeyB").map(|r| r.status),
            Some(NodeRunStatus::Success)
        );
        assert_eq!(
            out.node_runs.get("KeyC").map(|r| r.status),
            Some(NodeRunStatus::Success)
        );
    }

    #[tokio::test]
    async fn parallel_node_returns_merged_task_context_and_reports_name() {
        let branches: Vec<Box<dyn Node>> = vec![Box::new(WriterBranch {
            identity: "BranchA",
            write_key: "KeyA",
            value: serde_json::json!({ "from": "A" }),
        })];
        let parallel = ParallelNode::new("Fanout", branches);

        assert_eq!(parallel.name(), "Fanout");

        let out = parallel
            .process(empty_context())
            .await
            .expect("process should succeed");
        assert!(out.nodes.contains_key("KeyA"));
    }
}
