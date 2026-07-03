//! The `Workflow` pointer-walk runner: seeds every node PENDING, walks
//! node-to-node via `WorkflowSchema::next_after` (`connections[0]`) for plain
//! nodes or via `Router::route(ctx)` (which may return an undeclared runtime
//! back-edge) for routers, and stamps each `NodeRun` RUNNING then
//! SUCCESS/FAILED + timing around the framework-owned `node_context` envelope.
//!
//! The `on_progress` callback is the injected persistence seam (contract-facing
//! Postgres wiring lands in EN.1.C) — this block only defines its signature and
//! invokes it at node boundaries.

use std::collections::HashMap;

use chrono::Utc;
use engine_contract::{NodeRun, NodeRunStatus, TaskContext};

use crate::node::NodeRegistry;
use crate::schema::WorkflowSchema;
use crate::validate::{ValidationError, WorkflowValidator};

/// The injected persistence seam, invoked with a snapshot of the `TaskContext`
/// at each node boundary (initial seed, and after every node transition).
/// This block defines the signature only — EN.1.C wires it to Postgres.
pub type OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>;

/// A runnable workflow: the node registry (identity -> `Node` impl) paired
/// with the declarative `WorkflowSchema` describing the graph shape.
pub struct Workflow {
    registry: NodeRegistry,
    schema: WorkflowSchema,
}

/// Error returned by `Workflow::run` for conditions outside a node's own
/// `NodeError` (e.g. an unresolvable start node or a dangling connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowError {
    pub message: String,
}

impl WorkflowError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WorkflowError {}

impl Workflow {
    pub fn new(registry: NodeRegistry, schema: WorkflowSchema) -> Self {
        Self { registry, schema }
    }

    /// Run `WorkflowValidator::validate` against `registry`/`schema` before
    /// constructing. Use this constructor when the declared graph must be
    /// guaranteed structurally sound (BFS reachability, DFS cycle check that
    /// skips router edges, and router-only fan-out) before any node runs.
    ///
    /// `Workflow::new` stays infallible and unvalidated so EN.1.A callers and
    /// tests keep compiling unchanged.
    pub fn new_validated(
        registry: NodeRegistry,
        schema: WorkflowSchema,
    ) -> Result<Self, ValidationError> {
        WorkflowValidator::validate(&registry, &schema)?;
        Ok(Self { registry, schema })
    }

    /// Run the workflow to completion (or first failure).
    ///
    /// `event` seeds `TaskContext::event`; all nodes declared in the schema are
    /// seeded PENDING in `node_runs` before the walk starts, and the initial
    /// snapshot is emitted via `on_progress` before the first node runs. The
    /// pointer-walk starts at the schema's start node. For a non-router node
    /// the walk follows `connections[0]`; for a router (`Node::as_router()`
    /// returns `Some`) the walk instead calls `Router::route(&ctx)` to choose
    /// the next identity at runtime — which may be an identity outside the
    /// router's declared `connections` (a retry/back-edge). A router returning
    /// `None` from `route` ends the walk. A node returning `Err` is stamped
    /// FAILED and halts the walk (the accumulated `TaskContext` is still
    /// returned).
    pub async fn run(
        &self,
        event: serde_json::Value,
        mut on_progress: OnProgress<'_>,
    ) -> Result<TaskContext, WorkflowError> {
        let mut ctx = TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };

        // Seed every declared node PENDING before anything runs.
        for identity in self.schema.nodes.keys() {
            ctx.node_runs.insert(
                identity.clone(),
                NodeRun {
                    status: NodeRunStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    error: None,
                    input: None,
                    usage: None,
                },
            );
        }
        on_progress(&ctx);

        let mut current = Some(self.schema.start_node.clone());

        while let Some(identity) = current {
            let node = self.registry.get(&identity).ok_or_else(|| {
                WorkflowError::new(format!("no node registered for identity '{identity}'"))
            })?;

            // Routers choose their next identity at runtime via `route(ctx)`
            // (possibly an undeclared back-edge); plain nodes keep walking
            // the statically declared `connections[0]`. Resolve this before
            // `node_context` consumes `ctx` so the router sees the context
            // as it stood on entry to this node.
            let router_next = node
                .as_router()
                .map(|router| crate::routing::dispatch_route(router, &ctx));

            let (next_ctx, failed) = node_context(node, ctx, &mut on_progress).await;
            ctx = next_ctx;

            if failed {
                break;
            }

            current = match router_next {
                Some(next) => next,
                None => self.schema.next_after(&identity).map(str::to_string),
            };
        }

        Ok(ctx)
    }
}

/// The framework-owned envelope around a single node's `process` call: stamps
/// `NodeRun` RUNNING + `started_at` on entry, then SUCCESS + `completed_at` on
/// `Ok` or FAILED + `completed_at` + `error` on `Err`, invoking `on_progress`
/// after each transition. Returns the updated `TaskContext` and whether the
/// node failed (so the caller knows to halt the walk).
async fn node_context(
    node: &dyn crate::node::Node,
    mut ctx: TaskContext,
    on_progress: &mut OnProgress<'_>,
) -> (TaskContext, bool) {
    let identity = node.name().to_string();

    ctx.node_runs
        .entry(identity.clone())
        .and_modify(|run| {
            run.status = NodeRunStatus::Running;
            run.started_at = Some(Utc::now());
        })
        .or_insert_with(|| NodeRun {
            status: NodeRunStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        });
    on_progress(&ctx);

    // `Node::process` only hands the context back on `Ok`; keep a pre-call
    // snapshot so the FAILED transition still has a `TaskContext` to stamp
    // and return on `Err`.
    let pre_call_ctx = ctx.clone();

    match node.process(ctx).await {
        Ok(mut ok_ctx) => {
            if let Some(run) = ok_ctx.node_runs.get_mut(&identity) {
                run.status = NodeRunStatus::Success;
                run.completed_at = Some(Utc::now());
            }
            on_progress(&ok_ctx);
            (ok_ctx, false)
        }
        Err(err) => {
            let mut err_ctx = pre_call_ctx;
            if let Some(run) = err_ctx.node_runs.get_mut(&identity) {
                run.status = NodeRunStatus::Failed;
                run.completed_at = Some(Utc::now());
                run.error = Some(err.message.clone());
            }
            on_progress(&err_ctx);
            (err_ctx, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, NodeError};

    struct SuccessNode;

    #[async_trait::async_trait]
    impl Node for SuccessNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SuccessNode"
        }
    }

    struct FailNode;

    #[async_trait::async_trait]
    impl Node for FailNode {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Err(NodeError::new("boom"))
        }

        fn name(&self) -> &str {
            "FailNode"
        }
    }

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn node_context_stamps_success_transition() {
        let node = SuccessNode;
        let mut ctx = empty_context();
        ctx.node_runs.insert(
            "SuccessNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let mut on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        let (out, failed) = node_context(&node, ctx, &mut on_progress).await;
        drop(on_progress);

        assert!(!failed);
        let run = out.node_runs.get("SuccessNode").expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
        assert!(run.started_at.is_some());
        assert!(run.completed_at.is_some());
        assert!(run.started_at.unwrap() <= run.completed_at.unwrap());
        assert!(run.error.is_none());

        // Two on_progress calls: entering RUNNING, then exiting SUCCESS.
        let snapshots = snapshots.borrow();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(
            snapshots[0].node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Running
        );
        assert_eq!(
            snapshots[1].node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Success
        );
    }

    #[tokio::test]
    async fn node_context_stamps_failure_transition() {
        let node = FailNode;
        let mut ctx = empty_context();
        ctx.node_runs.insert(
            "FailNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );

        let mut on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let (out, failed) = node_context(&node, ctx, &mut on_progress).await;

        assert!(failed);
        let run = out.node_runs.get("FailNode").expect("run present");
        assert_eq!(run.status, NodeRunStatus::Failed);
        assert!(run.started_at.is_some());
        assert!(run.completed_at.is_some());
        assert_eq!(run.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn run_seeds_all_nodes_pending_before_first_run() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SuccessNode", nodes);

        let workflow = Workflow::new(registry, schema);

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        let result = workflow.run(serde_json::json!({}), on_progress).await;

        assert!(result.is_ok());
        // First snapshot is the initial PENDING seed, before any node runs.
        let snapshots = snapshots.borrow();
        let first = &snapshots[0];
        assert_eq!(
            first.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );
        assert!(first
            .node_runs
            .get("SuccessNode")
            .unwrap()
            .started_at
            .is_none());
    }

    #[tokio::test]
    async fn run_halts_walk_on_failure() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(FailNode));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "FailNode".to_string(),
            crate::schema::NodeConfig::new("FailNode", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "FailNode", nodes);

        let workflow = Workflow::new(registry, schema);
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let result = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .unwrap();

        assert_eq!(
            result.node_runs.get("FailNode").unwrap().status,
            NodeRunStatus::Failed
        );
        assert_eq!(
            result.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );
        assert!(!result.nodes.contains_key("SuccessNode"));
    }
}
