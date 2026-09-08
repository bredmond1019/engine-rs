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

/// How a `ParallelNode` reacts when one of its branches returns `Err`.
///
/// The default, [`BranchFailure::FailRun`], is today's behavior byte-for-byte:
/// the first branch error is propagated as the node's own error and every
/// other branch's output — successful or not — is discarded. Opting into
/// [`BranchFailure::Tolerate`] is the adopting workflow's job; nothing in
/// this repo does so today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BranchFailure {
    /// Propagate the first branch `Err` as this node's error, discarding
    /// every branch's output. This is the default and today's behavior.
    #[default]
    FailRun,
    /// Merge every `Ok` branch exactly as `FailRun` would, and additionally
    /// stamp a per-branch outcome record (see [`BranchOutcome`]) onto
    /// `ctx.nodes` under this node's own identity. The node returns `Ok`
    /// even when one or more branches failed.
    Tolerate,
}

/// A single branch's outcome, recorded under the `ParallelNode`'s own
/// identity in `ctx.nodes` when running under [`BranchFailure::Tolerate`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BranchOutcome {
    /// The branch's own `Node::name()`.
    pub branch: String,
    /// `true` if the branch returned `Ok`, `false` if it returned `Err`.
    pub ok: bool,
    /// The branch's error text, present only when `ok` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A node that fans out to a fixed, ordered list of branch nodes, runs them
/// in parallel over a cloned `TaskContext` each, and merges their `nodes` +
/// `node_runs` output back into a single `TaskContext`.
///
/// On key collision between branches, the **later branch in declared order**
/// wins (deterministic last-write-wins) — see the module docs.
pub struct ParallelNode {
    identity: String,
    branches: Vec<Box<dyn Node>>,
    branch_failure: BranchFailure,
}

impl ParallelNode {
    /// Build a `ParallelNode` under `identity`, fanning out to `branches` in
    /// the given order. That declared order is the tie-break order used by
    /// the merge: later branches win on key collision.
    ///
    /// Defaults to [`BranchFailure::FailRun`] — today's behavior.
    pub fn new(identity: impl Into<String>, branches: Vec<Box<dyn Node>>) -> Self {
        Self {
            identity: identity.into(),
            branches,
            branch_failure: BranchFailure::default(),
        }
    }

    /// Set this node's [`BranchFailure`] mode.
    pub fn with_branch_failure(mut self, branch_failure: BranchFailure) -> Self {
        self.branch_failure = branch_failure;
        self
    }

    /// Merge a set of successful branch outputs into `base` in declared
    /// order: a later branch's entry overwrites an earlier branch's entry on
    /// key collision (last-write-wins).
    fn merge_branch_outputs(base: TaskContext, branch_outputs: Vec<TaskContext>) -> TaskContext {
        let mut merged = base;
        let mut merged_nodes: HashMap<String, serde_json::Value> = HashMap::new();
        let mut merged_node_runs: HashMap<String, engine_contract::NodeRun> = HashMap::new();

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
        merged
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

        match self.branch_failure {
            BranchFailure::FailRun => {
                // Propagate the first branch failure, if any, as this node's
                // error — today's behavior, byte-for-byte.
                let mut branch_outputs = Vec::with_capacity(branch_results.len());
                for result in branch_results {
                    match result {
                        Ok(out) => branch_outputs.push(out),
                        Err(err) => return Err(err),
                    }
                }

                let merged = Self::merge_branch_outputs(ctx, branch_outputs);
                Ok(merged)
            }
            BranchFailure::Tolerate => {
                // Merge every Ok branch exactly as FailRun would, and stamp
                // a per-branch outcome record for every branch (ok and err
                // alike) onto ctx.nodes under this node's own identity.
                let mut outcomes = Vec::with_capacity(branch_results.len());
                let mut branch_outputs = Vec::with_capacity(branch_results.len());

                for (node, result) in self.branches.iter().zip(branch_results) {
                    match result {
                        Ok(out) => {
                            outcomes.push(BranchOutcome {
                                branch: node.name().to_string(),
                                ok: true,
                                error: None,
                            });
                            branch_outputs.push(out);
                        }
                        Err(err) => {
                            outcomes.push(BranchOutcome {
                                branch: node.name().to_string(),
                                ok: false,
                                error: Some(err.to_string()),
                            });
                        }
                    }
                }

                let mut merged = Self::merge_branch_outputs(ctx, branch_outputs);
                let outcome_value = serde_json::to_value(&outcomes).map_err(|err| {
                    NodeError::new(format!(
                        "ParallelNode '{}': failed to serialize branch outcomes: {err}",
                        self.identity
                    ))
                })?;
                merged.nodes.insert(self.identity.clone(), outcome_value);

                Ok(merged)
            }
        }
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

    #[tokio::test]
    async fn tolerate_mode_merges_survivors_and_returns_ok() {
        let branches: Vec<Box<dyn Node>> = vec![
            Box::new(WriterBranch {
                identity: "SurvivingBranch",
                write_key: "Survivor",
                value: serde_json::json!({ "ok": true }),
            }),
            Box::new(FailingBranch {
                identity: "FailingBranch",
                message: "simulated failure",
            }),
        ];
        let parallel =
            ParallelNode::new("Fanout", branches).with_branch_failure(BranchFailure::Tolerate);

        let out = parallel
            .process(empty_context())
            .await
            .expect("tolerate mode should return Ok even with a failing branch");

        assert_eq!(
            out.nodes.get("Survivor"),
            Some(&serde_json::json!({ "ok": true }))
        );
    }

    #[tokio::test]
    async fn tolerate_mode_records_per_branch_outcome_under_node_identity() {
        let branches: Vec<Box<dyn Node>> = vec![
            Box::new(WriterBranch {
                identity: "SurvivingBranch",
                write_key: "Survivor",
                value: serde_json::json!({ "ok": true }),
            }),
            Box::new(FailingBranch {
                identity: "FailingBranch",
                message: "simulated failure",
            }),
        ];
        let parallel =
            ParallelNode::new("Fanout", branches).with_branch_failure(BranchFailure::Tolerate);

        let out = parallel
            .process(empty_context())
            .await
            .expect("tolerate mode should return Ok");

        let outcomes: Vec<BranchOutcome> = serde_json::from_value(
            out.nodes
                .get("Fanout")
                .cloned()
                .expect("outcome record should be stamped under the node's own identity"),
        )
        .expect("outcome record should deserialize as Vec<BranchOutcome>");

        assert_eq!(outcomes.len(), 2);

        let surviving = outcomes
            .iter()
            .find(|o| o.branch == "SurvivingBranch")
            .expect("surviving branch outcome present");
        assert!(surviving.ok);
        assert!(surviving.error.is_none());

        let failing = outcomes
            .iter()
            .find(|o| o.branch == "FailingBranch")
            .expect("failing branch outcome present");
        assert!(!failing.ok);
        assert_eq!(failing.error.as_deref(), Some("simulated failure"));
    }

    #[async_trait::async_trait]
    impl Node for FailingBranch {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Err(NodeError::new(self.message.to_string()))
        }

        fn name(&self) -> &str {
            self.identity
        }
    }

    /// A branch node that always fails with a fixed error message, used to
    /// exercise `BranchFailure::Tolerate`.
    struct FailingBranch {
        identity: &'static str,
        message: &'static str,
    }
}
