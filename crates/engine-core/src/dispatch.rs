//! Dual-registry dispatch keyed by `workflow_type`.
//!
//! Moved down from `engine-serve` in `EN.5.E` task 3: nothing here is
//! HTTP-specific, so sub-workflow composition (a node dispatching another
//! registered workflow in-process, from inside its own `process`) can be an
//! in-process call within `engine-core` rather than a loopback HTTP request.
//! `engine_serve::dispatch` re-exports this module's public types so
//! `bastion`'s path dependency and the existing HTTP-layer import sites keep
//! resolving unchanged.
//!
//! `Dispatcher` holds two registries populated together at registration time:
//! a `workflow_registry` (`workflow_type` -> a factory producing a runnable
//! [`crate::Workflow`]) and a `schema_registry` (`workflow_type` ->
//! `WorkflowSchema`, used by the `GET /workflows/{type}/graph` endpoint).
//! Resolving an unregistered `workflow_type` returns a typed
//! `DispatchError::UnknownWorkflowType`, which the HTTP layer (EN.1.C task 4)
//! maps to a 422 response.
//!
//! `WorkflowFactory` (EN.5.D task 5) receives the triggering event payload so
//! a registration can resolve its own policy (the four-layer
//! `builtin < harness < profile < event` precedence) and assemble the
//! policy-dependent `registry_for_policy` before the `Workflow` is built —
//! something the old zero-argument factory could not do. A factory that
//! fails to resolve policy (an unknown profile name, a malformed `policy`
//! override in the event payload) returns `Err(String)`, surfaced through
//! `dispatch_with_event` as `DispatchError::PolicyResolutionFailed`, distinct
//! from `UnknownWorkflowType` so the HTTP layer (EN.5.D task 7) can map the
//! two to different status codes.

use std::collections::HashMap;
use std::fmt;

use crate::{Workflow, WorkflowSchema};

/// A factory that produces a fresh, runnable `Workflow` for a given
/// registration, given the triggering event payload (`data` from
/// `POST /events/`). Boxed so the `Dispatcher` can hold heterogeneous
/// construction logic per `workflow_type`. Returns `Err(String)` when the
/// registration's own policy resolution fails against `event` (e.g. an
/// unknown profile name).
pub type WorkflowFactory =
    Box<dyn Fn(&serde_json::Value) -> Result<Workflow, String> + Send + Sync>;

/// Error returned when resolving a `workflow_type`, either because it has
/// not been registered in both registries, or because its factory failed to
/// resolve policy against the triggering event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    UnknownWorkflowType(String),
    /// A registered `workflow_type`'s factory failed to resolve policy
    /// against the triggering event (e.g. an unknown profile name, a
    /// malformed `policy` override) — carries the factory's message.
    PolicyResolutionFailed(String),
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchError::UnknownWorkflowType(workflow_type) => {
                write!(f, "unknown workflow_type '{workflow_type}'")
            }
            DispatchError::PolicyResolutionFailed(message) => {
                write!(f, "policy resolution failed: {message}")
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
    /// `Workflow`, using an empty event payload (`{}`) for the factory's
    /// policy resolution. Returns `DispatchError::UnknownWorkflowType` if it
    /// is not registered, or `DispatchError::PolicyResolutionFailed` if the
    /// factory cannot resolve policy against the empty payload. Prefer
    /// [`Dispatcher::dispatch_with_event`] whenever an actual triggering
    /// event payload is available.
    pub fn dispatch(&self, workflow_type: &str) -> Result<Workflow, DispatchError> {
        self.dispatch_with_event(workflow_type, &serde_json::Value::Null)
    }

    /// Resolve a `workflow_type` to a freshly constructed, runnable
    /// `Workflow`, feeding the triggering `event` payload (`POST /events/`'s
    /// `data`) to the registration's factory so it can resolve its own
    /// policy. Returns `DispatchError::UnknownWorkflowType` if `workflow_type`
    /// is not registered, or `DispatchError::PolicyResolutionFailed` if the
    /// factory's policy resolution fails against `event` (carrying the
    /// factory's message, e.g. naming an unknown profile).
    pub fn dispatch_with_event(
        &self,
        workflow_type: &str,
        event: &serde_json::Value,
    ) -> Result<Workflow, DispatchError> {
        let factory = self
            .workflow_registry
            .get(workflow_type)
            .ok_or_else(|| DispatchError::UnknownWorkflowType(workflow_type.to_string()))?;
        factory(event).map_err(DispatchError::PolicyResolutionFailed)
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
    use crate::{Node, NodeError, NodeRegistry};
    use engine_contract::TaskContext;
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
            crate::NodeConfig::new("MarkerNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "MarkerNode", nodes)
    }

    fn fixture_factory() -> WorkflowFactory {
        Box::new(|_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(MarkerNode));
            Ok(Workflow::new(registry, fixture_schema("fixture")))
        })
    }

    /// A factory whose "policy resolution" always fails, to exercise
    /// `DispatchError::PolicyResolutionFailed`.
    fn failing_factory(message: &'static str) -> WorkflowFactory {
        Box::new(move |_event: &serde_json::Value| Err(message.to_string()))
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
        let on_progress: crate::OnProgress<'_> = Box::new(|_ctx| {});
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

    #[test]
    fn dispatch_with_event_passes_the_event_payload_to_the_factory() {
        let mut dispatcher = Dispatcher::new();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen_in_factory = seen.clone();
        let factory: WorkflowFactory = Box::new(move |event: &serde_json::Value| {
            *seen_in_factory.lock().unwrap() = Some(event.clone());
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(MarkerNode));
            Ok(Workflow::new(registry, fixture_schema("fixture")))
        });
        dispatcher.register(fixture_schema("fixture"), factory);

        let event = serde_json::json!({ "profile": "local-judgment" });
        let result = dispatcher.dispatch_with_event("fixture", &event);

        assert!(result.is_ok());
        assert_eq!(*seen.lock().unwrap(), Some(event));
    }

    #[test]
    fn dispatch_with_event_unknown_type_returns_unknown_workflow_type() {
        let dispatcher = Dispatcher::new();

        let result = dispatcher.dispatch_with_event("does-not-exist", &serde_json::json!({}));

        assert!(matches!(
            result,
            Err(DispatchError::UnknownWorkflowType(ref t)) if t == "does-not-exist"
        ));
    }

    #[test]
    fn dispatch_with_event_surfaces_policy_resolution_failure() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            fixture_schema("fixture"),
            failing_factory("unknown profile 'not-a-real-profile'"),
        );

        let result = dispatcher.dispatch_with_event(
            "fixture",
            &serde_json::json!({ "profile": "not-a-real-profile" }),
        );

        assert!(matches!(
            result,
            Err(DispatchError::PolicyResolutionFailed(ref m))
                if m == "unknown profile 'not-a-real-profile'"
        ));
    }

    #[test]
    fn dispatch_surfaces_policy_resolution_failure_via_default_dispatch() {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(fixture_schema("fixture"), failing_factory("bad policy"));

        let result = dispatcher.dispatch("fixture");

        assert!(matches!(
            result,
            Err(DispatchError::PolicyResolutionFailed(ref m)) if m == "bad policy"
        ));
    }

    #[test]
    fn dispatch_error_display_distinguishes_the_two_variants() {
        let unknown = DispatchError::UnknownWorkflowType("foo".to_string());
        let policy_failed = DispatchError::PolicyResolutionFailed("bad profile".to_string());

        assert!(unknown.to_string().contains("unknown workflow_type"));
        assert!(policy_failed
            .to_string()
            .contains("policy resolution failed"));
        assert_ne!(unknown, policy_failed);
    }
}
