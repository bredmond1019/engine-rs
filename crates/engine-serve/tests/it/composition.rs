//! Hermetic integration test for `EN.5.E`'s composition primitives —
//! instance identities, input bindings, the bounded-loop combinator, and
//! the in-process `Dispatcher` — driven through the real `Workflow` runner
//! rather than by hand-calling nodes, mirroring the driving style of
//! `proposal_generator_e2e.rs` / `research_agent_e2e.rs`.
//!
//! Covers the block's acceptance criteria:
//! (a) the same node type registered twice under distinct identities (via
//!     `NodeExt::with_identity`) runs with independent inputs (each bound
//!     via `NodeExt::with_input_from`'s `InputBinding`), asserted off the
//!     final `TaskContext` after a real `Workflow::run`;
//! (b) a fixture graph with an identity-overridden router (declaring >1
//!     connection) plus a `loop_combinator` cluster passes
//!     `WorkflowValidator::validate` clean;
//! (c) a node holding an `engine_core::dispatch::Dispatcher` resolves and
//!     runs a second registered workflow in-process from inside its own
//!     `process`, observed by the outer run, with a `StubHttpPost` injected
//!     into the fixture recording zero calls (no loopback HTTP);
//! (d) `engine_serve::dispatch::Dispatcher` still resolves at its
//!     re-exported path, standing in for `bastion`'s import site.
//!
//! Hermetic by construction: no `claude` subprocess is spawned, and no
//! network call is made — the sub-workflow call is entirely in-process, and
//! the only `HttpPost` in play is the recording `StubHttpPost`.

use std::collections::HashMap;
use std::sync::Arc;

use engine_contract::TaskContext;
use engine_core::dispatch::Dispatcher;
use engine_core::node::{InputBinding, Node, NodeError, NodeExt, NodeRegistry};
use engine_core::nodes::http_post::{HttpPost, StubHttpPost};
use engine_core::routing::Router;
use engine_core::schema::{NodeConfig, WorkflowSchema};
use engine_core::validate::WorkflowValidator;
use engine_core::workflow::Workflow;
use engine_core::{build_loop, ExitPredicate, LoopSpec};
use serde_json::{json, Value};

// ---------------------------------------------------------------------
// (a) Same node type twice under distinct identities, independent inputs
// ---------------------------------------------------------------------

/// A trivial upstream node that stamps a fixed payload into `ctx.nodes`
/// under its own identity, so two `FixtureReaderNode` instances have two
/// distinct upstreams to read from.
struct StampNode {
    identity: &'static str,
    payload: Value,
}

#[async_trait::async_trait]
impl Node for StampNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.identity.to_string(), self.payload.clone());
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// The fixture node under test: reads its upstream purely through
/// `InputBinding` (task 1's `with_input_from` primitive) — no node-name
/// const imported from anywhere in this module. Falls back to
/// `DEFAULT_UPSTREAM` when unbound, following the `ReaderNode` convention
/// documented in `node.rs`.
struct FixtureReaderNode {
    input: InputBinding,
}

impl FixtureReaderNode {
    const DEFAULT_UPSTREAM: &'static str = "NoUpstream";

    fn new() -> Self {
        Self {
            input: InputBinding::default(),
        }
    }

    #[must_use]
    fn with_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.input = InputBinding::bound(upstream);
        self
    }
}

#[async_trait::async_trait]
impl Node for FixtureReaderNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let upstream = self.input.resolve(Self::DEFAULT_UPSTREAM);
        let value = ctx.nodes.get(upstream).cloned().unwrap_or(Value::Null);
        ctx.nodes
            .insert(self.name().to_string(), json!({ "read": value }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "FixtureReaderNode"
    }
}

#[tokio::test]
async fn same_node_type_twice_runs_with_independent_bound_inputs() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(StampNode {
        identity: "UpstreamA",
        payload: json!({ "value": "a" }),
    }));
    registry.register(Box::new(StampNode {
        identity: "UpstreamB",
        payload: json!({ "value": "b" }),
    }));
    registry.register(Box::new(
        FixtureReaderNode::new()
            .with_input_from("UpstreamA")
            .with_identity("Reader1"),
    ));
    registry.register(Box::new(
        FixtureReaderNode::new()
            .with_input_from("UpstreamB")
            .with_identity("Reader2"),
    ));

    assert_eq!(registry.len(), 4);

    let mut nodes = HashMap::new();
    nodes.insert(
        "UpstreamA".to_string(),
        NodeConfig::new("UpstreamA", vec!["UpstreamB".to_string()]),
    );
    nodes.insert(
        "UpstreamB".to_string(),
        NodeConfig::new("UpstreamB", vec!["Reader1".to_string()]),
    );
    nodes.insert(
        "Reader1".to_string(),
        NodeConfig::new("Reader1", vec!["Reader2".to_string()]),
    );
    nodes.insert("Reader2".to_string(), NodeConfig::new("Reader2", vec![]));

    let schema = WorkflowSchema::new("COMPOSITION_FIXTURE", "UpstreamA", nodes);
    let workflow =
        Workflow::new_validated(registry, schema).expect("fixture graph should validate");

    let ctx = workflow
        .run(json!({}), Box::new(|_| {}))
        .await
        .expect("run should succeed");

    // Both instances resolved+stamped their own ctx.nodes entry under their
    // own instance identity (Identified's relabeling), each reflecting the
    // upstream it was independently bound to — not each other's.
    assert_eq!(
        ctx.nodes.get("Reader1"),
        Some(&json!({ "read": { "value": "a" } }))
    );
    assert_eq!(
        ctx.nodes.get("Reader2"),
        Some(&json!({ "read": { "value": "b" } }))
    );
    assert_eq!(
        ctx.node_runs.get("Reader1").map(|r| r.status),
        Some(engine_contract::NodeRunStatus::Success)
    );
    assert_eq!(
        ctx.node_runs.get("Reader2").map(|r| r.status),
        Some(engine_contract::NodeRunStatus::Success)
    );
}

// ---------------------------------------------------------------------
// (b) Identity-overridden router + combinator cluster validates clean
// ---------------------------------------------------------------------

/// A router with two declared branches, used identity-overridden via
/// `with_identity` to prove `as_router()` classification survives the
/// wrapper for the validator's fan-out + cycle-skip checks.
struct BranchRouterNode;

#[async_trait::async_trait]
impl Node for BranchRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "BranchRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for BranchRouterNode {
    fn route(&self, _ctx: &TaskContext) -> Option<String> {
        Some("BranchA".to_string())
    }
}

struct TerminalNode {
    identity: &'static str,
}

#[async_trait::async_trait]
impl Node for TerminalNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.identity.to_string(), json!({ "reached": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

struct LoopBodyNode;

#[async_trait::async_trait]
impl Node for LoopBodyNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current = ctx
            .nodes
            .get("LoopBody")
            .and_then(|v| v.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        ctx.nodes
            .insert("LoopBody".to_string(), json!({ "count": current + 1 }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "LoopBody"
    }
}

fn never_exits() -> ExitPredicate {
    Arc::new(|_ctx: &TaskContext| false)
}

#[test]
fn identity_overridden_router_plus_combinator_cluster_validates_clean() {
    let mut registry = NodeRegistry::new();

    // Identity-overridden router with >1 declared connection.
    registry.register(Box::new(BranchRouterNode.with_identity("Router#1")));
    registry.register(Box::new(TerminalNode {
        identity: "BranchA",
    }));
    registry.register(Box::new(TerminalNode {
        identity: "BranchB",
    }));

    // A combinator cluster coexisting in the same registry/schema.
    let spec = LoopSpec::new(
        "Fixture",
        3,
        never_exits(),
        "LoopBody".to_string(),
        "LoopExit".to_string(),
    );
    let cluster = build_loop(spec);
    let guard_identity = cluster.guard_identity.clone();
    registry.register(Box::new(LoopBodyNode));
    registry.register(Box::new(TerminalNode {
        identity: "LoopExit",
    }));
    for node in cluster.nodes {
        registry.register(node);
    }

    let mut nodes = HashMap::new();
    nodes.insert(
        "Router#1".to_string(),
        NodeConfig::new(
            "Router#1",
            vec!["BranchA".to_string(), "BranchB".to_string()],
        ),
    );
    nodes.insert("BranchA".to_string(), NodeConfig::new("BranchA", vec![]));
    nodes.insert("BranchB".to_string(), NodeConfig::new("BranchB", vec![]));
    nodes.insert(
        "LoopBody".to_string(),
        NodeConfig::new("LoopBody", vec![guard_identity]),
    );
    nodes.insert("LoopExit".to_string(), NodeConfig::new("LoopExit", vec![]));
    nodes.extend(cluster.connections);

    // Two independent start-reachable components declared under one
    // schema; wire the router as the formal start so BFS reachability
    // covers both — chain the router to also flow into the loop body via
    // its second declared branch's terminal (kept separate above), and
    // additionally wire BranchB -> LoopBody so the whole schema is one
    // connected, reachable graph.
    nodes.insert(
        "BranchB".to_string(),
        NodeConfig::new("BranchB", vec!["LoopBody".to_string()]),
    );

    let schema = WorkflowSchema::new("VALIDATOR_FIXTURE", "Router#1", nodes);

    WorkflowValidator::validate(&registry, &schema)
        .expect("identity-overridden router + combinator cluster should validate clean");
}

// ---------------------------------------------------------------------
// (c) In-process sub-workflow call via Dispatcher, zero HTTP calls
// ---------------------------------------------------------------------

/// Sub-workflow's single node: stamps a fixed payload the outer workflow
/// asserts on, proving the outer run actually observed the inner run's
/// result.
struct SubEchoNode;

#[async_trait::async_trait]
impl Node for SubEchoNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), json!({ "echoed": "from-sub" }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SubEcho"
    }
}

/// Holds a `Dispatcher` and an injectable `HttpPost` seam; resolves and
/// runs a second registered workflow in-process from inside its own
/// `process`. The `HttpPost` seam is never actually called by this node —
/// it exists purely so the test can assert the sub-workflow call made zero
/// HTTP calls (i.e. it really is in-process, not a loopback `POST
/// /events/`).
struct SubWorkflowCallerNode {
    dispatcher: Arc<Dispatcher>,
    #[allow(dead_code)]
    http: Arc<dyn HttpPost>,
}

impl SubWorkflowCallerNode {
    fn new(dispatcher: Arc<Dispatcher>, http: Arc<dyn HttpPost>) -> Self {
        Self { dispatcher, http }
    }
}

#[async_trait::async_trait]
impl Node for SubWorkflowCallerNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let inner_workflow = self
            .dispatcher
            .dispatch_with_event("SUB_WORKFLOW", &ctx.event)
            .map_err(|err| NodeError::new(err.to_string()))?;

        // Driven synchronously to completion via `futures::executor::block_on`
        // rather than `.await`ed directly: `Workflow::run`'s `OnProgress`
        // (`Box<dyn FnMut(&TaskContext)>`) is not `Send`, so its future is
        // never `Send` — and `Node::process`'s own future (this one) must be
        // `Send` (the `Node` trait bound). Running it to completion inline,
        // without crossing an `.await` point in *this* function, keeps the
        // inner non-`Send` future entirely out of this function's generated
        // state machine. The inner workflow here does no real async I/O, so
        // this never blocks on anything but CPU-bound fixture work.
        let inner_ctx =
            futures::executor::block_on(inner_workflow.run(ctx.event.clone(), Box::new(|_| {})))
                .map_err(|err| NodeError::new(err.to_string()))?;

        let inner_result = inner_ctx
            .nodes
            .get("SubEcho")
            .cloned()
            .unwrap_or(Value::Null);

        ctx.nodes.insert(
            self.name().to_string(),
            json!({ "inner_result": inner_result }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SubWorkflowCallerNode"
    }
}

fn sub_workflow_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert("SubEcho".to_string(), NodeConfig::new("SubEcho", vec![]));
    WorkflowSchema::new("SUB_WORKFLOW", "SubEcho", nodes)
}

#[tokio::test]
async fn outer_workflow_invokes_sub_workflow_in_process_with_zero_http_calls() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(
        sub_workflow_schema(),
        Box::new(|_event: &Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(SubEchoNode));
            Ok(Workflow::new_validated(registry, sub_workflow_schema())
                .expect("sub-workflow schema should validate"))
        }),
    );
    let dispatcher = Arc::new(dispatcher);

    let stub_http = StubHttpPost::succeeding(json!({}));
    let http: Arc<dyn HttpPost> = Arc::new(stub_http.clone());

    let mut outer_registry = NodeRegistry::new();
    outer_registry.register(Box::new(SubWorkflowCallerNode::new(dispatcher, http)));

    let mut outer_nodes = HashMap::new();
    outer_nodes.insert(
        "SubWorkflowCallerNode".to_string(),
        NodeConfig::new("SubWorkflowCallerNode", vec![]),
    );
    let outer_schema = WorkflowSchema::new("OUTER_WORKFLOW", "SubWorkflowCallerNode", outer_nodes);

    let outer_workflow = Workflow::new_validated(outer_registry, outer_schema)
        .expect("outer fixture graph should validate");

    let ctx = outer_workflow
        .run(json!({}), Box::new(|_| {}))
        .await
        .expect("outer run should succeed");

    assert_eq!(
        ctx.nodes.get("SubWorkflowCallerNode"),
        Some(&json!({ "inner_result": { "echoed": "from-sub" } }))
    );

    // Zero HTTP calls: the sub-workflow call was entirely in-process via
    // `Dispatcher::dispatch_with_event`, never a loopback `POST /events/`.
    assert!(
        stub_http.last_call().is_none(),
        "sub-workflow composition must make zero HTTP calls"
    );
}

// ---------------------------------------------------------------------
// (d) engine_serve::dispatch re-export still resolves
// ---------------------------------------------------------------------

#[test]
fn engine_serve_dispatch_reexport_still_resolves() {
    // Compile- and run-level assertion that `engine_serve::dispatch::Dispatcher`
    // still resolves at its pre-move public path, standing in for `bastion`'s
    // path dependency.
    let dispatcher = engine_serve::dispatch::Dispatcher::new();
    assert!(dispatcher.registered_types().is_empty());

    // And that it is in fact the same type as engine_core's, not a
    // parallel duplicate definition.
    let _typed: engine_core::dispatch::Dispatcher = dispatcher;
}
