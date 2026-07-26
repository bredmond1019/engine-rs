//! Bounded-loop combinator (`EN.5.E` task 2) — builds the
//! `{guard router, increment node, back-edge}` cluster from a [`LoopSpec`]
//! so the retry-counter idiom (forced by `Router::route`'s inability to
//! mutate `TaskContext` directly — only the node that owns `process` can do
//! that) is written once instead of a third time.
//!
//! Modeled on the hand-written loop in
//! `crate::workflows::sdlc_flow::{graph, task_loop}`
//! (`TriageRouterNode`/`ReviewRouterNode` -> `IncrementAttemptNode` ->
//! `ImplementTaskNode`) — read that as the reference idiom; this module
//! generalizes it into a reusable builder. `sdlc_flow` itself is
//! intentionally left unretrofitted (out of scope for this block).
//!
//! ## Shape
//!
//! [`build_loop`] takes a [`LoopSpec`] and returns a [`LoopCluster`]: two
//! boxed nodes (a guard router and an increment node) plus the declared
//! `NodeConfig` connections for both, ready to be merged into a
//! `NodeRegistry` / `WorkflowSchema`.
//!
//! - The **guard** is a real `Router` (`as_router()` returns `Some`): on
//!   each visit it reads the increment node's iteration count off
//!   `ctx.nodes` (via [`LoopSpec::increment_identity`]) and either routes to
//!   the increment node (continue) or to `exit_to` (cap reached, or the
//!   spec's `exit_predicate` is satisfied). The guard's own `process` is a
//!   pure passthrough — it never mutates `TaskContext`.
//! - The **increment** node owns the counter: it is the only node in the
//!   cluster whose `process` mutates `TaskContext`, stamping the bumped
//!   count under its own identity in `ctx.nodes`. It is *also* a `Router`
//!   (`route` unconditionally returns `body_entry`) purely so the back-edge
//!   it forms is a **runtime** router edge, never a declared non-router
//!   edge — per the declared-acyclic/runtime-cyclic contract (D42), this is
//!   what lets `WorkflowValidator::validate`'s DFS cycle check skip the
//!   edge instead of rejecting the cluster as cyclic.
//! - Both nodes get their identity from [`LoopSpec::identity_prefix`] via
//!   `NodeExt::with_identity` (`EN.5.E` task 1), so two clusters built from
//!   specs with distinct prefixes coexist in one registry without
//!   collision.
//!
//! Iteration counting: the guard treats "iterations so far" as the
//! increment node's stored count plus one (the body run currently in
//! flight, which has not yet incremented the counter). So with
//! `max_iterations = 3`, the body runs exactly 3 times before the guard's
//! cap check forces an exit — the third run's guard visit sees
//! `count = 2`, computes `iterations_so_far = 3 >= 3`, and exits instead of
//! routing to the increment node a third time.

use std::collections::HashMap;
use std::sync::Arc;

use engine_contract::TaskContext;

use crate::node::{Node, NodeError, NodeExt};
use crate::routing::Router;
use crate::schema::NodeConfig;

/// A predicate over the current `TaskContext`, evaluated by the guard on
/// every visit. Returning `true` exits the loop even before
/// `max_iterations` is reached.
pub type ExitPredicate = Arc<dyn Fn(&TaskContext) -> bool + Send + Sync>;

/// Declarative spec for a bounded-loop cluster.
///
/// `identity_prefix` derives the cluster's two node identities
/// (`"{prefix}Guard"` and `"{prefix}Increment"`) so distinct specs never
/// collide in one `NodeRegistry`.
#[derive(Clone)]
pub struct LoopSpec {
    /// Prefix used to derive this cluster's node identities.
    pub identity_prefix: String,
    /// Hard cap on how many times the body may run before the guard forces
    /// an exit regardless of `exit_predicate`.
    pub max_iterations: u32,
    /// Evaluated by the guard on every visit; `true` exits the loop even
    /// before `max_iterations` is reached.
    pub exit_predicate: ExitPredicate,
    /// The identity of the body's entry node — where the back-edge
    /// re-enters the loop on each iteration.
    pub body_entry: String,
    /// The identity to route to once the loop exits (cap reached, or the
    /// predicate was satisfied).
    pub exit_to: String,
}

impl LoopSpec {
    pub fn new(
        identity_prefix: impl Into<String>,
        max_iterations: u32,
        exit_predicate: ExitPredicate,
        body_entry: impl Into<String>,
        exit_to: impl Into<String>,
    ) -> Self {
        Self {
            identity_prefix: identity_prefix.into(),
            max_iterations,
            exit_predicate,
            body_entry: body_entry.into(),
            exit_to: exit_to.into(),
        }
    }

    /// This cluster's guard router identity: `"{prefix}Guard"`.
    #[must_use]
    pub fn guard_identity(&self) -> String {
        format!("{}Guard", self.identity_prefix)
    }

    /// This cluster's increment node identity: `"{prefix}Increment"`.
    #[must_use]
    pub fn increment_identity(&self) -> String {
        format!("{}Increment", self.identity_prefix)
    }
}

/// The `ctx.nodes` key the increment node stamps its running count under,
/// nested inside the node's own `ctx.nodes` entry: `{"iterations": <u32>}`.
const ITERATIONS_KEY: &str = "iterations";

/// Base (pre-identity-override) type name for the guard router — never used
/// as a registry key directly; [`build_loop`] always wraps it via
/// `with_identity`.
const GUARD_BASE_IDENTITY: &str = "LoopGuardRouterNode";

/// Base (pre-identity-override) type name for the increment node — see
/// [`GUARD_BASE_IDENTITY`].
const INCREMENT_BASE_IDENTITY: &str = "LoopIncrementNode";

/// The guard router: reads the increment node's stored count and either
/// continues the loop (routes to the increment node) or exits it. Never
/// mutates `TaskContext` — `Router::route`'s signature only borrows it.
struct LoopGuardRouterNode {
    increment_identity: String,
    max_iterations: u32,
    exit_predicate: ExitPredicate,
    exit_to: String,
}

#[async_trait::async_trait]
impl Node for LoopGuardRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        // Pure passthrough: the guard only ever chooses a next identity via
        // `route`; it does not itself transform the context.
        Ok(ctx)
    }

    fn name(&self) -> &str {
        GUARD_BASE_IDENTITY
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for LoopGuardRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let completed = ctx
            .nodes
            .get(&self.increment_identity)
            .and_then(|v| v.get(ITERATIONS_KEY))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        // The body run currently in flight has not yet been counted by the
        // increment node, so it is `completed + 1`.
        let iterations_so_far = completed.saturating_add(1);

        if iterations_so_far >= self.max_iterations || (self.exit_predicate)(ctx) {
            Some(self.exit_to.clone())
        } else {
            Some(self.increment_identity.clone())
        }
    }
}

/// The increment node: owns the loop counter (the only node in the cluster
/// whose `process` mutates `TaskContext`), and is also a `Router` whose
/// `route` unconditionally continues to `body_entry` — the back-edge — so
/// the DFS cycle check in `WorkflowValidator::validate` skips it (router
/// edges are runtime-chosen, never declared-cyclic).
struct LoopIncrementNode {
    /// This node's own resolved (post-`with_identity`) identity — used to
    /// read/write its `ctx.nodes` entry directly, decoupled from
    /// `Identified`'s auto-relabeling (which is designed for two *distinct*
    /// node instances, not one instance whose state must accumulate across
    /// repeated visits within a single run).
    self_identity: String,
    body_entry: String,
}

#[async_trait::async_trait]
impl Node for LoopIncrementNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let current = ctx
            .nodes
            .get(&self.self_identity)
            .and_then(|v| v.get(ITERATIONS_KEY))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let bumped = current + 1;
        ctx.nodes.insert(
            self.self_identity.clone(),
            serde_json::json!({ ITERATIONS_KEY: bumped }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        INCREMENT_BASE_IDENTITY
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for LoopIncrementNode {
    fn route(&self, _ctx: &TaskContext) -> Option<String> {
        // Unconditional back-edge: the guard already decided to continue
        // the loop before routing here.
        Some(self.body_entry.clone())
    }
}

/// The assembled output of [`build_loop`]: the cluster's boxed nodes (ready
/// for `NodeRegistry::register`) plus the `NodeConfig` connections to merge
/// into a `WorkflowSchema::nodes` map.
pub struct LoopCluster {
    /// This cluster's guard router identity — route the node preceding the
    /// loop's exit check here.
    pub guard_identity: String,
    /// This cluster's increment node identity.
    pub increment_identity: String,
    /// The cluster's two nodes, ready to register.
    pub nodes: Vec<Box<dyn Node>>,
    /// Declared connections for both cluster nodes, keyed by identity —
    /// merge into `WorkflowSchema::nodes`.
    pub connections: HashMap<String, NodeConfig>,
}

/// Build the `{guard router, increment node, back-edge}` cluster described
/// by `spec`. Register the returned [`LoopCluster::nodes`] into a
/// `NodeRegistry` and merge [`LoopCluster::connections`] into a
/// `WorkflowSchema::nodes` map; wire whatever precedes the loop body to
/// route into `spec.body_entry`, and have the body's own exit point route
/// to [`LoopCluster::guard_identity`].
#[must_use]
pub fn build_loop(spec: LoopSpec) -> LoopCluster {
    let guard_identity = spec.guard_identity();
    let increment_identity = spec.increment_identity();

    let guard = LoopGuardRouterNode {
        increment_identity: increment_identity.clone(),
        max_iterations: spec.max_iterations,
        exit_predicate: Arc::clone(&spec.exit_predicate),
        exit_to: spec.exit_to.clone(),
    }
    .with_identity(guard_identity.clone());

    let increment = LoopIncrementNode {
        self_identity: increment_identity.clone(),
        body_entry: spec.body_entry.clone(),
    }
    .with_identity(increment_identity.clone());

    let mut connections = HashMap::new();
    connections.insert(
        guard_identity.clone(),
        NodeConfig::new(
            guard_identity.clone(),
            vec![increment_identity.clone(), spec.exit_to.clone()],
        ),
    );
    connections.insert(
        increment_identity.clone(),
        NodeConfig::new(increment_identity.clone(), vec![spec.body_entry.clone()]),
    );

    LoopCluster {
        guard_identity,
        increment_identity,
        nodes: vec![Box::new(guard), Box::new(increment)],
        connections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeRegistry;
    use crate::schema::WorkflowSchema;
    use crate::validate::WorkflowValidator;
    use crate::workflow::Workflow;
    use std::collections::HashMap as StdHashMap;

    /// A trivial body node that bumps its own run count on every visit —
    /// the fixture "body" the loop cluster wraps in tests.
    struct BodyCounterNode {
        identity: String,
    }

    #[async_trait::async_trait]
    impl Node for BodyCounterNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            let current = ctx
                .nodes
                .get(&self.identity)
                .and_then(|v| v.get("count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            ctx.nodes.insert(
                self.identity.clone(),
                serde_json::json!({ "count": current + 1 }),
            );
            Ok(ctx)
        }

        fn name(&self) -> &str {
            &self.identity
        }
    }

    /// A trivial terminal node reached once the loop exits.
    struct ExitNode;

    #[async_trait::async_trait]
    impl Node for ExitNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert("Exit".to_string(), serde_json::json!({ "exited": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "Exit"
        }
    }

    /// Assembles a runnable single-cluster workflow: `Body -> {cluster} ->
    /// Exit`, with the body wired as the cluster's `body_entry` and `Exit`
    /// as `exit_to`.
    fn build_fixture_workflow(
        prefix: &str,
        max_iterations: u32,
        exit_predicate: ExitPredicate,
    ) -> Workflow {
        let body_identity = "Body".to_string();
        let exit_identity = "Exit".to_string();

        let spec = LoopSpec::new(
            prefix,
            max_iterations,
            exit_predicate,
            body_identity.clone(),
            exit_identity.clone(),
        );
        let cluster = build_loop(spec);

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(BodyCounterNode {
            identity: body_identity.clone(),
        }));
        registry.register(Box::new(ExitNode));
        for node in cluster.nodes {
            registry.register(node);
        }

        let mut nodes: StdHashMap<String, NodeConfig> = StdHashMap::new();
        nodes.insert(
            body_identity.clone(),
            NodeConfig::new(body_identity.clone(), vec![cluster.guard_identity.clone()]),
        );
        nodes.insert(
            exit_identity.clone(),
            NodeConfig::new(exit_identity, vec![]),
        );
        nodes.extend(cluster.connections);

        let schema = WorkflowSchema::new("LOOP_FIXTURE", body_identity, nodes);

        Workflow::new_validated(registry, schema).expect("assembled cluster should validate")
    }

    fn never_exits() -> ExitPredicate {
        Arc::new(|_ctx: &TaskContext| false)
    }

    #[tokio::test]
    async fn body_runs_exactly_max_iterations_times_when_predicate_never_fires() {
        let workflow = build_fixture_workflow("Test", 3, never_exits());

        let ctx = workflow
            .run(serde_json::json!({}), Box::new(|_| {}))
            .await
            .expect("run should succeed");

        assert_eq!(
            ctx.nodes.get("Body").and_then(|v| v.get("count")),
            Some(&serde_json::json!(3))
        );
        assert_eq!(
            ctx.nodes.get("Exit"),
            Some(&serde_json::json!({ "exited": true }))
        );
    }

    #[tokio::test]
    async fn exit_predicate_short_circuits_before_the_cap() {
        // Exit as soon as the body has run twice, well below the cap of 5.
        let predicate: ExitPredicate = Arc::new(|ctx: &TaskContext| {
            ctx.nodes
                .get("Body")
                .and_then(|v| v.get("count"))
                .and_then(|v| v.as_u64())
                .map(|count| count >= 2)
                .unwrap_or(false)
        });
        let workflow = build_fixture_workflow("Test", 5, predicate);

        let ctx = workflow
            .run(serde_json::json!({}), Box::new(|_| {}))
            .await
            .expect("run should succeed");

        assert_eq!(
            ctx.nodes.get("Body").and_then(|v| v.get("count")),
            Some(&serde_json::json!(2))
        );
        assert_eq!(
            ctx.nodes.get("Exit"),
            Some(&serde_json::json!({ "exited": true }))
        );
    }

    #[tokio::test]
    async fn counter_is_readable_off_ctx_nodes_under_the_increment_identity() {
        let workflow = build_fixture_workflow("Test", 3, never_exits());

        let ctx = workflow
            .run(serde_json::json!({}), Box::new(|_| {}))
            .await
            .expect("run should succeed");

        // 3 body runs => 2 continues (increments); the 3rd guard visit hits
        // the cap and exits without a further increment.
        assert_eq!(
            ctx.nodes.get("TestIncrement"),
            Some(&serde_json::json!({ "iterations": 2 }))
        );
    }

    #[test]
    fn assembled_cluster_passes_validator() {
        let spec = LoopSpec::new(
            "Test",
            3,
            never_exits(),
            "Body".to_string(),
            "Exit".to_string(),
        );
        let cluster = build_loop(spec);

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(BodyCounterNode {
            identity: "Body".to_string(),
        }));
        registry.register(Box::new(ExitNode));
        let guard_identity = cluster.guard_identity.clone();
        for node in cluster.nodes {
            registry.register(node);
        }

        let mut nodes: StdHashMap<String, NodeConfig> = StdHashMap::new();
        nodes.insert(
            "Body".to_string(),
            NodeConfig::new("Body".to_string(), vec![guard_identity]),
        );
        nodes.insert(
            "Exit".to_string(),
            NodeConfig::new("Exit".to_string(), vec![]),
        );
        nodes.extend(cluster.connections);

        let schema = WorkflowSchema::new("LOOP_FIXTURE", "Body".to_string(), nodes);

        WorkflowValidator::validate(&registry, &schema)
            .expect("assembled cluster should validate clean");
    }

    #[test]
    fn two_clusters_coexist_in_one_registry_under_distinct_identities() {
        let spec_a = LoopSpec::new(
            "A",
            3,
            never_exits(),
            "Body".to_string(),
            "Exit".to_string(),
        );
        let spec_b = LoopSpec::new(
            "B",
            5,
            never_exits(),
            "Body".to_string(),
            "Exit".to_string(),
        );

        let cluster_a = build_loop(spec_a);
        let cluster_b = build_loop(spec_b);

        assert_eq!(cluster_a.guard_identity, "AGuard");
        assert_eq!(cluster_a.increment_identity, "AIncrement");
        assert_eq!(cluster_b.guard_identity, "BGuard");
        assert_eq!(cluster_b.increment_identity, "BIncrement");

        let mut registry = NodeRegistry::new();
        for node in cluster_a.nodes {
            registry.register(node);
        }
        for node in cluster_b.nodes {
            registry.register(node);
        }

        assert_eq!(registry.len(), 4);
        assert!(registry.contains("AGuard"));
        assert!(registry.contains("AIncrement"));
        assert!(registry.contains("BGuard"));
        assert!(registry.contains("BIncrement"));
    }
}
