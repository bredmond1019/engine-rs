//! EN.14.F task 2: the framework write site for `node_invocations`.
//!
//! Drives a small `Workflow` whose one node is dispatched more than once via
//! a runtime back-edge (a `Router` retry loop), then proves the invocation
//! ledger — not `ctx.nodes` — is the thing that survives a retry.

use std::collections::HashMap;

use engine_contract::TaskContext;
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, OnProgress, Router, Workflow};

/// Increments a counter in `ctx.metadata` on every dispatch and routes back
/// to itself twice before handing off to `DoneNode` — three total
/// dispatches, so `ctx.nodes["RetryNode"]` (one slot) undercounts by two.
struct RetryNode;

const RETRIES_BEFORE_DONE: u64 = 2;

#[async_trait::async_trait]
impl Node for RetryNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let count = ctx
            .metadata
            .get("retry_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            + 1;
        ctx.metadata["retry_count"] = serde_json::json!(count);
        ctx.nodes.insert(
            self.name().to_string(),
            serde_json::json!({ "count": count }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "RetryNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for RetryNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let count = ctx
            .metadata
            .get("retry_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if count <= RETRIES_BEFORE_DONE {
            Some("RetryNode".to_string())
        } else {
            Some("DoneNode".to_string())
        }
    }
}

/// A node that always fails, used to exercise the `Err`-branch ledger row.
struct AlwaysFailsNode;

#[async_trait::async_trait]
impl Node for AlwaysFailsNode {
    async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new("always fails"))
    }

    fn name(&self) -> &str {
        "AlwaysFailsNode"
    }
}

struct DoneNode;

#[async_trait::async_trait]
impl Node for DoneNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), serde_json::json!({ "done": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "DoneNode"
    }
}

fn retry_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(RetryNode));
    registry.register(Box::new(DoneNode));
    registry
}

fn retry_schema() -> engine_core::WorkflowSchema {
    let mut nodes = HashMap::new();
    // RetryNode is a router: its declared connection is never walked at
    // runtime (`route()` decides), but the schema still requires one so the
    // node is reachable/registered.
    nodes.insert(
        "RetryNode".to_string(),
        NodeConfig::new("RetryNode", vec!["DoneNode".to_string()]),
    );
    nodes.insert("DoneNode".to_string(), NodeConfig::new("DoneNode", vec![]));

    engine_core::WorkflowSchema::new("retry-loop", "RetryNode", nodes)
}

#[tokio::test]
async fn retried_node_leaves_more_ledger_rows_than_ctx_nodes_has_keys() {
    let workflow = Workflow::new(retry_registry(), retry_schema());
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("workflow should complete");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    let ctx_nodes_count = result.nodes.len();

    // THE PROPERTY THIS BLOCK EXISTS FOR: the ledger holds one row per
    // dispatch (RetryNode dispatched 3 times + DoneNode once = 4), while
    // `ctx.nodes` holds one slot per distinct node identity (2: RetryNode,
    // DoneNode) — a strictly smaller count.
    assert!(
        invocations.len() > ctx_nodes_count,
        "expected ledger ({}) to exceed ctx.nodes ({ctx_nodes_count}) on a retried run",
        invocations.len()
    );

    // POSITIVE CONTROL (required by the block): the naive equality form must
    // be FALSE on this same run — proving the assertion above is actually
    // capable of failing, not vacuously true.
    assert!(
        !(invocations.len() == ctx_nodes_count),
        "equality form must be FALSE on a retried run — otherwise this test could never catch a regression"
    );

    // Sanity: RetryNode dispatched 3 times (count 1, 2, 3), DoneNode once.
    let retry_dispatches = invocations
        .iter()
        .filter(|inv| inv.node == "RetryNode")
        .count();
    assert_eq!(retry_dispatches, 3);
    let done_dispatches = invocations
        .iter()
        .filter(|inv| inv.node == "DoneNode")
        .count();
    assert_eq!(done_dispatches, 1);
    assert_eq!(ctx_nodes_count, 2);
}

#[tokio::test]
async fn a_failed_dispatch_produces_a_failed_ledger_row_carrying_the_error_message() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(AlwaysFailsNode));
    let mut nodes = HashMap::new();
    nodes.insert(
        "AlwaysFailsNode".to_string(),
        NodeConfig::new("AlwaysFailsNode", vec![]),
    );
    let schema = engine_core::WorkflowSchema::new("fails", "AlwaysFailsNode", nodes);
    let workflow = Workflow::new(registry, schema);
    let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("run should return Ok(ctx) even though the node failed");

    let invocations = engine_core::invocations::read_invocations(&result.metadata);
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].status,
        engine_contract::NodeInvocationStatus::Failed
    );
    assert_eq!(invocations[0].error.as_deref(), Some("always fails"));
    assert_eq!(invocations[0].node, "AlwaysFailsNode");
}
