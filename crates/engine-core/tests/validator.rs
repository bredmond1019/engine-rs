//! Integration tests for the router-aware `Workflow` runner + fallible
//! `Workflow::new_validated` constructor (EN.1.B task 4).
//!
//! Covers the declared-acyclic/runtime-cyclic contract (D42): a router's
//! declared connections stay acyclic (and so pass validation), while its
//! `route()` may return an undeclared back-edge that the runner still walks
//! at runtime — the retry path re-runs an earlier node before proceeding to
//! completion. Also covers rejection of a declared cycle among plain nodes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use engine_contract::TaskContext;
use engine_core::{
    dispatch_route, Node, NodeConfig, NodeError, NodeRegistry, Router, ValidationError, Workflow,
    WorkflowSchema,
};

/// The node the retry router sends control back to. Increments a shared
/// counter each time it runs so the test can assert it ran twice (once on
/// the initial pass, once on the retry back-edge).
struct CountingNode {
    identity: &'static str,
    runs: std::sync::Arc<AtomicUsize>,
}

impl Node for CountingNode {
    fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        ctx.nodes.insert(
            self.identity.to_string(),
            serde_json::json!({ "runs": self.runs.load(Ordering::SeqCst) }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// A router that, the first time it is visited, routes back to `"retry-me"`
/// (an undeclared back-edge — its declared `connections` only ever list
/// `"finish"`); on the second visit it routes forward to completion.
struct RetryRouter {
    identity: &'static str,
    visits: std::sync::Arc<AtomicUsize>,
}

impl Node for RetryRouter {
    fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for RetryRouter {
    fn route(&self, _ctx: &TaskContext) -> Option<String> {
        let visit = self.visits.fetch_add(1, Ordering::SeqCst);
        if visit == 0 {
            // Undeclared back-edge: not present in this router's declared
            // `connections` (which only list "finish").
            Some("retry-me".to_string())
        } else {
            Some("finish".to_string())
        }
    }
}

struct TerminalNode;

impl Node for TerminalNode {
    fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert("finish".to_string(), serde_json::json!({ "done": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "finish"
    }
}

/// Builds: retry-me -> router (router's declared connections: ["finish"],
/// acyclic) -> finish. The router's `route()` sends control back to
/// "retry-me" once at runtime — an edge never declared in the schema.
fn retry_workflow() -> (
    NodeRegistry,
    WorkflowSchema,
    std::sync::Arc<AtomicUsize>,
    std::sync::Arc<AtomicUsize>,
) {
    let retry_runs = std::sync::Arc::new(AtomicUsize::new(0));
    let router_visits = std::sync::Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(CountingNode {
        identity: "retry-me",
        runs: retry_runs.clone(),
    }));
    registry.register(Box::new(RetryRouter {
        identity: "router",
        visits: router_visits.clone(),
    }));
    registry.register(Box::new(TerminalNode));

    let mut nodes = HashMap::new();
    nodes.insert(
        "retry-me".to_string(),
        NodeConfig::new("retry-me", vec!["router".to_string()]),
    );
    // Declared connections stay acyclic: router -> finish only. The
    // "router -> retry-me" edge is never declared; it only ever happens via
    // route() at runtime.
    nodes.insert(
        "router".to_string(),
        NodeConfig::new("router", vec!["finish".to_string()]),
    );
    nodes.insert("finish".to_string(), NodeConfig::new("finish", vec![]));

    let schema = WorkflowSchema::new("retry", "retry-me", nodes);

    (registry, schema, retry_runs, router_visits)
}

#[test]
fn router_with_undeclared_back_edge_passes_validation_and_retries_then_completes() {
    let (registry, schema, retry_runs, _router_visits) = retry_workflow();

    let workflow = Workflow::new_validated(registry, schema)
        .expect("declared graph is acyclic and should pass validation");

    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});
    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .expect("workflow should run to completion");

    // The retry node ran twice: once on the initial pass, once via the
    // router's undeclared back-edge.
    assert_eq!(retry_runs.load(Ordering::SeqCst), 2);

    // The walk reached the terminal node after the retry.
    assert!(result.nodes.contains_key("finish"));
    assert_eq!(
        result.nodes.get("finish"),
        Some(&serde_json::json!({ "done": true }))
    );
}

#[test]
fn router_route_dispatch_matches_runner_behavior() {
    // Sanity check that the same dispatch helper the runner uses resolves
    // the expected sequence directly.
    let visits = std::sync::Arc::new(AtomicUsize::new(0));
    let router = RetryRouter {
        identity: "router",
        visits: visits.clone(),
    };
    let ctx = TaskContext {
        event: serde_json::json!({}),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    };

    assert_eq!(dispatch_route(&router, &ctx), Some("retry-me".to_string()));
    assert_eq!(dispatch_route(&router, &ctx), Some("finish".to_string()));
}

struct PlainNode {
    identity: &'static str,
}

impl Node for PlainNode {
    fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// start -> mid -> start: a cycle formed entirely of declared, non-router
/// edges. `new_validated` must reject this before any node runs.
#[test]
fn cyclic_non_router_workflow_is_rejected_by_new_validated() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(PlainNode { identity: "start" }));
    registry.register(Box::new(PlainNode { identity: "mid" }));

    let mut nodes = HashMap::new();
    nodes.insert(
        "start".to_string(),
        NodeConfig::new("start", vec!["mid".to_string()]),
    );
    nodes.insert(
        "mid".to_string(),
        NodeConfig::new("mid", vec!["start".to_string()]),
    );

    let schema = WorkflowSchema::new("cyclic", "start", nodes);

    // `Workflow` intentionally does not implement `Debug`, so match instead
    // of using `expect_err`/`unwrap_err` (which require it).
    match Workflow::new_validated(registry, schema) {
        Err(err) => assert!(matches!(err, ValidationError::Cycle { .. })),
        Ok(_) => panic!("declared cycle among non-router nodes should be rejected"),
    }
}
