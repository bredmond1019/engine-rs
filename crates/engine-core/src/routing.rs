//! The `Router` trait — runtime next-node selection for nodes whose
//! successor is not statically fixed by `NodeConfig::connections`.
//!
//! Non-router nodes always walk `connections[0]` (EN.1.A behavior). A
//! `Router` instead picks its next node identity at runtime by inspecting the
//! current `TaskContext`. Per the declared-acyclic/runtime-cyclic contract
//! (D42 / master-plan EN.1.B), `route()` MAY return an identity that is not
//! among the router's declared connections — this is how retry/back-edges
//! are supported without the acyclic validator seeing a cycle.

use engine_contract::TaskContext;

use crate::node::Node;

/// A node that selects its next-node identity at runtime rather than via the
/// statically declared `connections[0]`.
///
/// `route` returns `Some(identity)` to continue to that node, or `None` to
/// stop the walk (no further declared or runtime successor).
pub trait Router: Node {
    /// Choose the next node's identity given the current context. May return
    /// an identity outside the router's declared `connections` (a runtime
    /// back-edge), or `None` to end the walk here.
    fn route(&self, ctx: &TaskContext) -> Option<String>;
}

/// Resolve the runtime next-node identity for a router, given the current
/// `TaskContext`. Thin dispatch helper so callers (the runner, the
/// validator) don't need to call `route` directly.
pub fn dispatch_route(router: &dyn Router, ctx: &TaskContext) -> Option<String> {
    router.route(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeError, NodeRegistry};
    use std::collections::HashMap;

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// A router that routes to "Retry" if `ctx.metadata.retry` is true,
    /// otherwise to "Forward". Demonstrates conditional routing based on
    /// TaskContext state.
    struct ConditionalRouter;

    #[async_trait::async_trait]
    impl Node for ConditionalRouter {
        async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "ConditionalRouter"
        }

        fn as_router(&self) -> Option<&dyn Router> {
            Some(self)
        }
    }

    impl Router for ConditionalRouter {
        fn route(&self, ctx: &TaskContext) -> Option<String> {
            let retry = ctx
                .metadata
                .get("retry")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if retry {
                Some("Retry".to_string())
            } else {
                Some("Forward".to_string())
            }
        }
    }

    /// A plain, non-router node — `as_router()` should default to `None`.
    struct PlainNode;

    #[async_trait::async_trait]
    impl Node for PlainNode {
        async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "PlainNode"
        }
    }

    #[test]
    fn conditional_router_routes_by_context_state() {
        let router = ConditionalRouter;

        let mut retry_ctx = empty_context();
        retry_ctx.metadata = serde_json::json!({ "retry": true });
        assert_eq!(
            dispatch_route(&router, &retry_ctx),
            Some("Retry".to_string())
        );

        let forward_ctx = empty_context();
        assert_eq!(
            dispatch_route(&router, &forward_ctx),
            Some("Forward".to_string())
        );
    }

    #[test]
    fn as_router_detection_plain_vs_router() {
        let plain = PlainNode;
        assert!(plain.as_router().is_none());

        let router = ConditionalRouter;
        assert!(router.as_router().is_some());
        assert_eq!(router.as_router().unwrap().name(), "ConditionalRouter");
    }

    #[test]
    fn route_may_return_identity_outside_declared_connections() {
        // The router's declared connections (as registered in a schema)
        // would only list "Forward"; "Retry" is an undeclared back-edge
        // that route() is still free to return.
        let router = ConditionalRouter;
        let mut ctx = empty_context();
        ctx.metadata = serde_json::json!({ "retry": true });

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(ConditionalRouter));
        assert!(registry.contains("ConditionalRouter"));

        let next = dispatch_route(&router, &ctx);
        assert_eq!(next, Some("Retry".to_string()));
    }
}
