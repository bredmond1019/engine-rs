//! The `Node` trait and node registry — the unit of work executed by the
//! `Workflow` runner (see `crate::workflow`, EN.1.A task 3).
//!
//! A `Node` only transforms the `TaskContext`; it does not touch its own
//! `NodeRun` status/timing — that envelope is framework-owned and stamped by
//! the runner around each call to `process` (contract §6).

use std::collections::HashMap;
use std::fmt;

use engine_contract::TaskContext;

/// Error returned by a node's `process` when it fails. Carries a human-readable
/// message; the runner stores this in the node's `NodeRun.error` field and
/// halts the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeError {
    pub message: String,
}

impl NodeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for NodeError {}

/// A single unit of work in a workflow graph.
///
/// `process` transforms the `TaskContext` and returns the updated context (or
/// a `NodeError` on failure). `name` is the node's identity — a stable
/// type-name string used as the map key in both `TaskContext::nodes` and
/// `TaskContext::node_runs` (contract §1).
pub trait Node: Send + Sync {
    /// Transform the context. The framework-owned envelope (see
    /// `crate::workflow::node_context`) handles `NodeRun` status/timing
    /// around this call — the node itself only does the work.
    fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError>;

    /// The node's identity — its type name, used as the registry/map key.
    fn name(&self) -> &str;
}

/// Maps a node's identity string to its boxed `Node` instance, so the runner
/// can resolve the next node to execute by name.
#[derive(Default)]
pub struct NodeRegistry {
    nodes: HashMap<String, Box<dyn Node>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Register a node under its own identity (`Node::name`).
    pub fn register(&mut self, node: Box<dyn Node>) {
        let key = node.name().to_string();
        self.nodes.insert(key, node);
    }

    /// Look up a node by identity string.
    pub fn get(&self, name: &str) -> Option<&dyn Node> {
        self.nodes.get(name).map(|boxed| boxed.as_ref())
    }

    /// Whether a node with this identity is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    /// Number of registered nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial node that stamps a marker into `TaskContext::nodes` under
    /// its own identity.
    struct MarkerNode;

    impl Node for MarkerNode {
        fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
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

    #[test]
    fn node_process_transforms_task_context() {
        let node = MarkerNode;
        let ctx = empty_context();

        let out = node.process(ctx).expect("process should succeed");

        assert_eq!(
            out.nodes.get("MarkerNode"),
            Some(&serde_json::json!({ "ran": true }))
        );
    }

    #[test]
    fn node_identity_matches_registry_key() {
        let mut registry = NodeRegistry::new();
        let node = MarkerNode;
        let identity = node.name().to_string();

        registry.register(Box::new(node));

        assert!(registry.contains(&identity));
        let looked_up = registry.get(&identity).expect("node should be registered");
        assert_eq!(looked_up.name(), identity);
    }

    #[test]
    fn registry_lookup_missing_returns_none() {
        let registry = NodeRegistry::new();
        assert!(registry.get("Nonexistent").is_none());
    }
}
