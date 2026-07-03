//! `WorkflowSchema` and `NodeConfig` — the declarative description of a
//! workflow graph: which node starts the walk, and each node's outbound
//! `connections` (next-node identities).
//!
//! Only `connections[0]` is walked by the `Workflow` runner in this block —
//! router/parallel branching over the remaining entries is EN.1.B.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Declarative configuration for a single node in a workflow graph.
///
/// `identity` is the node's registry key (matches `Node::name`). `connections`
/// lists the identities of candidate next nodes in the graph; for this block
/// the runner only ever follows `connections[0]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfig {
    pub identity: String,
    #[serde(default)]
    pub connections: Vec<String>,
}

impl NodeConfig {
    pub fn new(identity: impl Into<String>, connections: Vec<String>) -> Self {
        Self {
            identity: identity.into(),
            connections,
        }
    }

    /// The first (and, for this block, only) outbound connection — the next
    /// node identity to walk to, if any.
    pub fn next(&self) -> Option<&str> {
        self.connections.first().map(String::as_str)
    }
}

/// The declarative description of a workflow graph: its type name, the
/// identity of the node the pointer-walk starts at, and the per-node
/// `NodeConfig` entries keyed by node identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSchema {
    pub workflow_type: String,
    pub start_node: String,
    pub nodes: HashMap<String, NodeConfig>,
}

impl WorkflowSchema {
    pub fn new(
        workflow_type: impl Into<String>,
        start_node: impl Into<String>,
        nodes: HashMap<String, NodeConfig>,
    ) -> Self {
        Self {
            workflow_type: workflow_type.into(),
            start_node: start_node.into(),
            nodes,
        }
    }

    /// Resolve the `NodeConfig` for the declared start node.
    pub fn start(&self) -> Option<&NodeConfig> {
        self.nodes.get(&self.start_node)
    }

    /// Resolve a node's declared `connections[0]` next-node identity.
    pub fn next_after(&self, identity: &str) -> Option<&str> {
        self.nodes.get(identity).and_then(NodeConfig::next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a linear 3-node schema: start -> node2 -> node3 (node3 terminal).
    fn linear_schema() -> WorkflowSchema {
        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            NodeConfig::new("start", vec!["node2".to_string()]),
        );
        nodes.insert(
            "node2".to_string(),
            NodeConfig::new("node2", vec!["node3".to_string()]),
        );
        nodes.insert("node3".to_string(), NodeConfig::new("node3", vec![]));

        WorkflowSchema::new("linear-3", "start", nodes)
    }

    #[test]
    fn resolves_start_node() {
        let schema = linear_schema();

        let start = schema.start().expect("start node should resolve");

        assert_eq!(start.identity, "start");
    }

    #[test]
    fn resolves_connections_zero_next_node() {
        let schema = linear_schema();

        assert_eq!(schema.next_after("start"), Some("node2"));
        assert_eq!(schema.next_after("node2"), Some("node3"));
        assert_eq!(schema.next_after("node3"), None);
    }

    #[test]
    fn missing_node_lookup_returns_none() {
        let schema = linear_schema();

        assert!(schema.next_after("nonexistent").is_none());
        assert_eq!(schema.workflow_type, "linear-3");
    }
}
