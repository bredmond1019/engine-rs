//! `WorkflowValidator` — structural correctness guard for a declared
//! `WorkflowSchema` against a `NodeRegistry`.
//!
//! Per the declared-acyclic/runtime-cyclic contract (D42 / master-plan
//! EN.1.B), a workflow may contain runtime cycles introduced by a `Router`'s
//! `route()` (retry/back-edges) — those are never declared as `connections`
//! and so never appear to this validator. The validator only ever inspects
//! the **declared** graph:
//!
//! - **Reachability (BFS):** every node declared in `WorkflowSchema::nodes`
//!   must be reachable from `start_node` by walking declared `connections`.
//! - **Cycle detection (DFS):** a cycle formed purely of **non-router**
//!   declared edges is rejected. Edges declared *out of* a router node (i.e.
//!   any of a router's `connections`) are skipped by the cycle walk, since a
//!   router's real next node is chosen at runtime by `route()`, not by
//!   walking `connections`.
//! - **Fan-out arity:** only a router may declare more than one connection;
//!   a non-router node with `connections.len() > 1` is rejected.
//!
//! Router classification comes from the registry's `Node::as_router()` hook
//! (added in EN.1.B task 1) — a node whose registered instance's
//! `as_router()` returns `Some` is treated as a router for both the cycle
//! skip and the fan-out arity check.

use std::collections::HashSet;

use crate::node::NodeRegistry;
use crate::schema::WorkflowSchema;

/// A structural defect found by [`WorkflowValidator::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A node declared in the schema is not registered under its identity.
    UnregisteredNode { identity: String },
    /// A node declared in the schema is not reachable from `start_node` by
    /// walking declared connections.
    UnreachableNode { identity: String },
    /// A non-router node declared more than one connection.
    NonRouterFanOut {
        identity: String,
        connections: usize,
    },
    /// A cycle formed purely of non-router declared edges.
    Cycle { path: Vec<String> },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::UnregisteredNode { identity } => {
                write!(f, "node '{identity}' is declared but not registered")
            }
            ValidationError::UnreachableNode { identity } => {
                write!(
                    f,
                    "node '{identity}' is declared but not reachable from the start node"
                )
            }
            ValidationError::NonRouterFanOut {
                identity,
                connections,
            } => write!(
                f,
                "node '{identity}' declares {connections} connections but is not a router (only routers may fan out)"
            ),
            ValidationError::Cycle { path } => {
                write!(f, "cycle detected among non-router declared edges: {}", path.join(" -> "))
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Structural correctness guard: BFS reachability + DFS cycle detection +
/// fan-out arity, all evaluated over the schema's **declared** connections.
pub struct WorkflowValidator;

impl WorkflowValidator {
    /// Validate `schema` against `registry`.
    ///
    /// Returns the first defect found, in this order: fan-out arity,
    /// reachability, then cycles. (Order is deterministic for test
    /// reproducibility; a schema may have more than one defect.)
    pub fn validate(
        registry: &NodeRegistry,
        schema: &WorkflowSchema,
    ) -> Result<(), ValidationError> {
        // Fan-out arity: only routers may declare > 1 connection.
        for (identity, config) in &schema.nodes {
            if config.connections.len() > 1 && !Self::is_router(registry, identity) {
                return Err(ValidationError::NonRouterFanOut {
                    identity: identity.clone(),
                    connections: config.connections.len(),
                });
            }
        }

        Self::check_reachability(schema)?;
        Self::check_cycles(registry, schema)?;

        Ok(())
    }

    /// Whether the registry has this identity registered and its instance's
    /// `as_router()` returns `Some`. An unregistered identity is treated as
    /// non-router (its `UnregisteredNode`/`UnreachableNode` status is caught
    /// elsewhere).
    fn is_router(registry: &NodeRegistry, identity: &str) -> bool {
        registry
            .get(identity)
            .map(|node| node.as_router().is_some())
            .unwrap_or(false)
    }

    /// BFS from `start_node` over declared connections; every node declared
    /// in the schema must be visited.
    fn check_reachability(schema: &WorkflowSchema) -> Result<(), ValidationError> {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = vec![schema.start_node.as_str()];
        visited.insert(schema.start_node.as_str());

        while let Some(identity) = queue.pop() {
            if let Some(config) = schema.nodes.get(identity) {
                for next in &config.connections {
                    if visited.insert(next.as_str()) {
                        queue.push(next.as_str());
                    }
                }
            }
        }

        // Deterministic order for reproducible error reporting.
        let mut declared: Vec<&String> = schema.nodes.keys().collect();
        declared.sort();

        for identity in declared {
            if !visited.contains(identity.as_str()) {
                return Err(ValidationError::UnreachableNode {
                    identity: identity.clone(),
                });
            }
        }

        Ok(())
    }

    /// DFS cycle detection over declared connections, skipping edges out of
    /// router nodes (a router's declared `connections` are not the edges it
    /// actually walks at runtime — `route()` is).
    fn check_cycles(
        registry: &NodeRegistry,
        schema: &WorkflowSchema,
    ) -> Result<(), ValidationError> {
        let mut marks: std::collections::HashMap<&str, DfsMark> = std::collections::HashMap::new();

        // Deterministic iteration order for reproducible error reporting.
        let mut declared: Vec<&String> = schema.nodes.keys().collect();
        declared.sort();

        for start in declared {
            if !marks.contains_key(start.as_str()) {
                let mut stack: Vec<String> = Vec::new();
                Self::dfs_visit(registry, schema, start, &mut marks, &mut stack)?;
            }
        }

        Ok(())
    }

    fn dfs_visit<'a>(
        registry: &NodeRegistry,
        schema: &'a WorkflowSchema,
        identity: &'a str,
        marks: &mut std::collections::HashMap<&'a str, DfsMark>,
        stack: &mut Vec<String>,
    ) -> Result<(), ValidationError> {
        marks.insert(identity, DfsMark::InProgress);
        stack.push(identity.to_string());

        // Routers' declared connections are not the edges walked at
        // runtime — skip them entirely for cycle detection.
        if !Self::is_router(registry, identity) {
            if let Some(config) = schema.nodes.get(identity) {
                for next in &config.connections {
                    match marks.get(next.as_str()) {
                        Some(DfsMark::InProgress) => {
                            let mut path = stack.clone();
                            path.push(next.clone());
                            return Err(ValidationError::Cycle { path });
                        }
                        Some(DfsMark::Done) => continue,
                        None => {
                            Self::dfs_visit(registry, schema, next.as_str(), marks, stack)?;
                        }
                    }
                }
            }
        }

        stack.pop();
        marks.insert(identity, DfsMark::Done);
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DfsMark {
    InProgress,
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, NodeError, NodeRegistry};
    use crate::routing::Router;
    use crate::schema::NodeConfig;
    use engine_contract::TaskContext;
    use std::collections::HashMap;

    struct PlainNode {
        identity: &'static str,
    }

    #[async_trait::async_trait]
    impl Node for PlainNode {
        async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Ok(ctx)
        }

        fn name(&self) -> &str {
            self.identity
        }
    }

    struct RouterNode {
        identity: &'static str,
    }

    #[async_trait::async_trait]
    impl Node for RouterNode {
        async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Ok(ctx)
        }

        fn name(&self) -> &str {
            self.identity
        }

        fn as_router(&self) -> Option<&dyn Router> {
            Some(self)
        }
    }

    impl Router for RouterNode {
        fn route(&self, _ctx: &TaskContext) -> Option<String> {
            None
        }
    }

    fn registry_with(nodes: Vec<Box<dyn Node>>) -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        for node in nodes {
            registry.register(node);
        }
        registry
    }

    /// start -> mid -> end (linear, all plain nodes). Should validate.
    fn valid_linear_schema() -> (NodeRegistry, WorkflowSchema) {
        let registry = registry_with(vec![
            Box::new(PlainNode { identity: "start" }),
            Box::new(PlainNode { identity: "mid" }),
            Box::new(PlainNode { identity: "end" }),
        ]);

        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            NodeConfig::new("start", vec!["mid".to_string()]),
        );
        nodes.insert(
            "mid".to_string(),
            NodeConfig::new("mid", vec!["end".to_string()]),
        );
        nodes.insert("end".to_string(), NodeConfig::new("end", vec![]));

        (registry, WorkflowSchema::new("linear", "start", nodes))
    }

    #[test]
    fn valid_linear_schema_passes() {
        let (registry, schema) = valid_linear_schema();
        assert_eq!(WorkflowValidator::validate(&registry, &schema), Ok(()));
    }

    /// start (router, fans out to A and B) -> A, B (both terminal). Routers
    /// may declare > 1 connection; should validate.
    #[test]
    fn valid_router_fan_out_schema_passes() {
        let registry = registry_with(vec![
            Box::new(RouterNode { identity: "start" }),
            Box::new(PlainNode { identity: "A" }),
            Box::new(PlainNode { identity: "B" }),
        ]);

        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            NodeConfig::new("start", vec!["A".to_string(), "B".to_string()]),
        );
        nodes.insert("A".to_string(), NodeConfig::new("A", vec![]));
        nodes.insert("B".to_string(), NodeConfig::new("B", vec![]));

        let schema = WorkflowSchema::new("router-fanout", "start", nodes);
        assert_eq!(WorkflowValidator::validate(&registry, &schema), Ok(()));
    }

    #[test]
    fn unreachable_node_is_rejected() {
        let (registry, mut schema) = valid_linear_schema();
        // Add a node declared in the schema but never wired in as a
        // connection from any reachable node.
        schema
            .nodes
            .insert("orphan".to_string(), NodeConfig::new("orphan", vec![]));

        let err = WorkflowValidator::validate(&registry, &schema).unwrap_err();
        assert_eq!(
            err,
            ValidationError::UnreachableNode {
                identity: "orphan".to_string()
            }
        );
    }

    #[test]
    fn non_router_declaring_multiple_connections_is_rejected() {
        let registry = registry_with(vec![
            Box::new(PlainNode { identity: "start" }),
            Box::new(PlainNode { identity: "A" }),
            Box::new(PlainNode { identity: "B" }),
        ]);

        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            NodeConfig::new("start", vec!["A".to_string(), "B".to_string()]),
        );
        nodes.insert("A".to_string(), NodeConfig::new("A", vec![]));
        nodes.insert("B".to_string(), NodeConfig::new("B", vec![]));

        let schema = WorkflowSchema::new("bad-fanout", "start", nodes);

        let err = WorkflowValidator::validate(&registry, &schema).unwrap_err();
        assert_eq!(
            err,
            ValidationError::NonRouterFanOut {
                identity: "start".to_string(),
                connections: 2
            }
        );
    }

    /// start -> mid -> start: a cycle formed entirely of non-router
    /// declared edges. Should be rejected.
    #[test]
    fn non_router_cycle_is_rejected() {
        let registry = registry_with(vec![
            Box::new(PlainNode { identity: "start" }),
            Box::new(PlainNode { identity: "mid" }),
        ]);

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

        let err = WorkflowValidator::validate(&registry, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::Cycle { .. }));
    }

    /// A router with a declared back-edge (start -> router -> start) is NOT
    /// a cycle defect, because the cycle walk skips edges out of routers.
    #[test]
    fn router_declared_back_edge_is_not_a_cycle() {
        let registry = registry_with(vec![
            Box::new(PlainNode { identity: "start" }),
            Box::new(RouterNode { identity: "router" }),
        ]);

        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            NodeConfig::new("start", vec!["router".to_string()]),
        );
        nodes.insert(
            "router".to_string(),
            NodeConfig::new("router", vec!["start".to_string()]),
        );

        let schema = WorkflowSchema::new("router-back-edge", "start", nodes);

        assert_eq!(WorkflowValidator::validate(&registry, &schema), Ok(()));
    }
}
