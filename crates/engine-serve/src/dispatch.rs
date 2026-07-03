//! Dual-registry dispatch keyed by `workflow_type`.
//!
//! `Dispatcher` holds two registries populated together at registration time:
//! a `workflow_registry` (`workflow_type` -> a factory producing a runnable
//! `engine_core::Workflow`) and a `schema_registry` (`workflow_type` ->
//! `WorkflowSchema`, used by the `GET /workflows/{type}/graph` endpoint).
//! Resolving an unregistered `workflow_type` returns a typed
//! `DispatchError::UnknownWorkflowType`, which the HTTP layer (EN.1.C task 4)
//! maps to a 422 response.

use std::collections::HashMap;
use std::fmt;

use engine_core::{Workflow, WorkflowSchema};

/// A factory that produces a fresh, runnable `Workflow` for a given
/// registration. Boxed so the `Dispatcher` can hold heterogeneous
/// construction logic per `workflow_type`.
pub type WorkflowFactory = Box<dyn Fn() -> Workflow + Send + Sync>;

/// Error returned when resolving a `workflow_type` that has not been
/// registered in both registries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    UnknownWorkflowType(String),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchError::UnknownWorkflowType(workflow_type) => {
                write!(f, "unknown workflow_type '{workflow_type}'")
            }
        }
    }
}

impl std::error::Error for DispatchError {}

/// Dual-registry dispatcher: resolves a `workflow_type` to a runnable
/// `Workflow` (for triggering) or to its `WorkflowSchema` (for the graph
/// endpoint). Both registries are populated together on `register`, so a
/// `workflow_type` is always either present in both or absent from both.
#[derive(Default)]
pub struct Dispatcher {
    workflow_registry: HashMap<String, WorkflowFactory>,
    schema_registry: HashMap<String, WorkflowSchema>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            workflow_registry: HashMap::new(),
            schema_registry: HashMap::new(),
        }
    }

    /// Register a workflow under `schema.workflow_type`, populating both the
    /// `workflow_registry` (via `factory`) and the `schema_registry` (via a
    /// clone of `schema`).
    pub fn register(&mut self, schema: WorkflowSchema, factory: WorkflowFactory) {
        let workflow_type = schema.workflow_type.clone();
        self.workflow_registry
            .insert(workflow_type.clone(), factory);
        self.schema_registry.insert(workflow_type, schema);
    }

    /// Resolve a `workflow_type` to a freshly constructed, runnable
    /// `Workflow`. Returns `DispatchError::UnknownWorkflowType` if it is not
    /// registered.
    pub fn dispatch(&self, workflow_type: &str) -> Result<Workflow, DispatchError> {
        self.workflow_registry
            .get(workflow_type)
            .map(|factory| factory())
            .ok_or_else(|| DispatchError::UnknownWorkflowType(workflow_type.to_string()))
    }

    /// Resolve a `workflow_type` to its declared `WorkflowSchema` (the graph
    /// endpoint's data source). Returns `DispatchError::UnknownWorkflowType`
    /// if it is not registered.
    pub fn resolve_schema(&self, workflow_type: &str) -> Result<&WorkflowSchema, DispatchError> {
        self.schema_registry
            .get(workflow_type)
            .ok_or_else(|| DispatchError::UnknownWorkflowType(workflow_type.to_string()))
    }

    /// The `workflow_type`s currently registered (present in both
    /// registries).
    pub fn registered_types(&self) -> Vec<String> {
        self.workflow_registry.keys().cloned().collect()
    }

    /// Whether a `workflow_type` is registered in both registries.
    pub fn is_registered(&self, workflow_type: &str) -> bool {
        self.workflow_registry.contains_key(workflow_type)
            && self.schema_registry.contains_key(workflow_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::TaskContext;
    use engine_core::{Node, NodeError, NodeRegistry};
    use std::collections::HashMap as StdHashMap;

    struct MarkerNode;

    #[async_trait::async_trait]
    impl Node for MarkerNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    fn fixture_schema(workflow_type: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
    }

    fn fixture_factory() -> WorkflowFactory {
        Box::new(|| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(MarkerNode));
            Workflow::new(registry, fixture_schema("fixture"))
        })
    }

    #[test]
    fn registering_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(fixture_schema("fixture"), fixture_factory());

        assert!(dispatcher.is_registered("fixture"));
        assert_eq!(dispatcher.registered_types(), vec!["fixture".to_string()]);
        assert!(dispatcher.resolve_schema("fixture").is_ok());
    }

    #[tokio::test]
    async fn resolving_known_type_succeeds() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(fixture_schema("fixture"), fixture_factory());

        let workflow = dispatcher.dispatch("fixture");

        assert!(workflow.is_ok());
        let on_progress: engine_core::OnProgress<'_> = Box::new(|_ctx| {});
        let result = workflow
            .unwrap()
            .run(serde_json::json!({}), on_progress)
            .await;
        assert!(result.is_ok());
    }

    #[test]
    fn resolving_unknown_type_returns_unknown_workflow_type_error() {
        let dispatcher = Dispatcher::new();

        let result = dispatcher.dispatch("does-not-exist");

        assert!(matches!(
            result,
            Err(DispatchError::UnknownWorkflowType(ref t)) if t == "does-not-exist"
        ));

        let schema_result = dispatcher.resolve_schema("does-not-exist");
        assert_eq!(
            schema_result,
            Err(DispatchError::UnknownWorkflowType(
                "does-not-exist".to_string()
            ))
        );
        assert!(!dispatcher.is_registered("does-not-exist"));
    }
}
