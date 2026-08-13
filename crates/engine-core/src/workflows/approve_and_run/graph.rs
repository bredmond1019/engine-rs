//! Execution against the reviewed payload, plus the declared `APPROVE_AND_RUN`
//! graph (`EN.8.D` task 5).
//!
//! [`ApproveAndRunExecuteNode`] is the single node this micro-workflow
//! declares — both start and terminal, mirroring
//! `workflows::harvest_approve::graph`'s shape verbatim. It reads task 4's
//! authorization off `ctx.event` (built by [`execution_event`]) and, on
//! `authorized: true`, delegates the actual push to the existing
//! [`crate::nodes::harvest_approve::HarvestApproveNode`] over its
//! injectable [`crate::nodes::http_post::HttpPost`] seam — **no second push
//! path, no re-derived body**. The `url`/`payload` it hands `HarvestApproveNode`
//! are copied verbatim from task 4's [`super::verdict::ExecutionAuthorization`],
//! which itself copies them verbatim from the pending-harvest record
//! (`super::verdict`'s doc comment). On `authorized: false` (skip / requeue /
//! routed-to-session) it drives zero POSTs and stamps a `posted: false`
//! result instead.
//!
//! Either way, the resolved [`ApproveAndRunPolicy`] from task 2 is stamped
//! into this node's own `ctx.nodes` result (never a bare `cost_usd` key —
//! `policy_state` guarantees that), so `RunTelemetry`/`PolicyAggregate` can
//! attribute behavior to the setting that caused it.

use std::collections::HashMap;
use std::sync::Arc;

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError, NodeRegistry};
use crate::nodes::harvest_approve::HarvestApproveNode;
use crate::nodes::http_post::HttpPost;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;
use crate::workflows::put_result;

use super::policy::{policy_state, ApproveAndRunPolicy};
use super::verdict::ExecutionAuthorization;

/// The identity `APPROVE_AND_RUN`'s single node is registered under.
pub const APPROVE_AND_RUN_NODE_NAME: &str = "ApproveAndRunExecuteNode";

/// `APPROVE_AND_RUN`'s registered workflow type string.
pub const APPROVE_AND_RUN_WORKFLOW_TYPE: &str = "APPROVE_AND_RUN";

/// Build the `ctx.event` shape [`ApproveAndRunExecuteNode`] expects from an
/// optional task 4 [`ExecutionAuthorization`]:
/// - `Some(auth)` -> `{"authorized": true, "item_id", "url", "payload"}`,
///   the stored `url`/`payload` copied verbatim.
/// - `None` (skip / requeue / routed-to-session — anything task 4 did not
///   authorize) -> `{"authorized": false}`, driving zero POSTs.
#[must_use]
pub fn execution_event(authorization: Option<&ExecutionAuthorization>) -> Value {
    match authorization {
        Some(auth) => json!({
            "authorized": true,
            "item_id": auth.item_id,
            "url": auth.url,
            "payload": auth.payload,
        }),
        None => json!({ "authorized": false }),
    }
}

/// The node that executes task 4's authorization. See the module doc for
/// the full contract.
pub struct ApproveAndRunExecuteNode {
    inner: HarvestApproveNode,
    policy: ApproveAndRunPolicy,
}

impl ApproveAndRunExecuteNode {
    /// Construct with the live `HttpPost` seam and the built-in-default
    /// [`ApproveAndRunPolicy`] — the behavior-stable default (standing rule 6).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: HarvestApproveNode::new(),
            policy: ApproveAndRunPolicy::default(),
        }
    }

    /// Override the injectable `HttpPost` seam. Tests inject a
    /// `StubHttpPost` so the gated suite never contacts a live endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: Arc<dyn HttpPost>) -> Self {
        self.inner = self.inner.with_http_post(http_post);
        self
    }

    /// Override the resolved [`ApproveAndRunPolicy`] this node stamps into
    /// its `ctx.nodes` result.
    #[must_use]
    pub fn with_policy(mut self, policy: ApproveAndRunPolicy) -> Self {
        self.policy = policy;
        self
    }
}

impl Default for ApproveAndRunExecuteNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ApproveAndRunExecuteNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = ctx.event.clone();
        let authorized = event
            .get("authorized")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if !authorized {
            let mut ctx = ctx;
            put_result(
                &mut ctx,
                self.name(),
                json!({
                    "executed": false,
                    "posted": false,
                    "policy": policy_state(&self.policy),
                }),
            );
            return Ok(ctx);
        }

        let artifact_id = event
            .get("item_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let url = event.get("url").cloned().unwrap_or(Value::Null);
        let payload = event.get("payload").cloned().unwrap_or(Value::Null);

        // Delegate the actual push to `HarvestApproveNode` over an event
        // shaped as the pending-harvest record it already knows how to
        // read — the stored `url`/`payload`, verbatim, never re-derived.
        // No second push path.
        let inner_event = json!({
            "artifact_id": artifact_id,
            "url": url,
            "payload": payload,
        });
        let inner_ctx = TaskContext {
            event: inner_event,
            ..ctx.clone()
        };
        let inner_ctx = self.inner.process(inner_ctx).await?;

        let mut result = inner_ctx
            .nodes
            .get(self.inner.name())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(obj) = result.as_object_mut() {
            obj.insert("policy".to_string(), policy_state(&self.policy));
        }

        let mut ctx = inner_ctx;
        put_result(&mut ctx, self.name(), result);
        Ok(ctx)
    }

    fn name(&self) -> &str {
        APPROVE_AND_RUN_NODE_NAME
    }
}

/// Build the declared `WorkflowSchema` for `APPROVE_AND_RUN`: a single
/// node, both start and terminal, with no forward connection — mirrors
/// `workflows::harvest_approve::graph::schema` verbatim.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        APPROVE_AND_RUN_NODE_NAME.to_string(),
        NodeConfig::new(APPROVE_AND_RUN_NODE_NAME, vec![]),
    );
    WorkflowSchema::new(
        APPROVE_AND_RUN_WORKFLOW_TYPE,
        APPROVE_AND_RUN_NODE_NAME,
        nodes,
    )
}

/// Build a fresh `NodeRegistry` for `APPROVE_AND_RUN` with the live
/// `HttpPost` seam and the built-in-default policy: one
/// [`ApproveAndRunExecuteNode`], registered under
/// [`APPROVE_AND_RUN_NODE_NAME`] (its default `Node::name()` identity).
#[must_use]
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ApproveAndRunExecuteNode::new()));
    registry
}

/// Build a `NodeRegistry` for `APPROVE_AND_RUN` with an injected `HttpPost`
/// seam and a resolved policy — what `engine-serve`'s registration (task 6)
/// and hermetic tests use instead of [`registry`]'s live-transport default.
#[must_use]
pub fn registry_with(http_post: Arc<dyn HttpPost>, policy: ApproveAndRunPolicy) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        ApproveAndRunExecuteNode::new()
            .with_http_post(http_post)
            .with_policy(policy),
    ));
    registry
}

/// Build the runnable `APPROVE_AND_RUN` `Workflow`: [`registry`] paired
/// with [`schema`], constructed via `Workflow::new_validated` so assembly
/// fails loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("APPROVE_AND_RUN declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::http_post::StubHttpPost;
    use crate::validate::WorkflowValidator;

    fn ctx_with_event(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        }
    }

    fn authorization() -> ExecutionAuthorization {
        ExecutionAuthorization {
            item_id: "item-1".to_string(),
            url: "https://synapse.example/ingest/learning-artifact".to_string(),
            payload: json!({"title": "some artifact", "body": "content"}),
        }
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, APPROVE_AND_RUN_WORKFLOW_TYPE);
    }

    #[test]
    fn registry_contains_exactly_one_node_under_expected_identity() {
        let registry = registry();
        assert!(registry.contains(APPROVE_AND_RUN_NODE_NAME));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let _workflow = workflow();
    }

    #[test]
    fn schema_declares_exactly_one_node_that_is_both_start_and_terminal() {
        let schema = schema();
        assert_eq!(schema.nodes.len(), 1);
        let config = schema
            .nodes
            .get(&schema.start_node)
            .expect("start node should be declared");
        assert!(
            config.connections.is_empty(),
            "the single node should have no forward connections (terminal)"
        );
    }

    #[tokio::test]
    async fn approved_authorization_posts_the_stored_url_and_byte_identical_body() {
        let auth = authorization();
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = ApproveAndRunExecuteNode::new()
            .with_http_post(Arc::new(stub.clone()))
            .with_policy(ApproveAndRunPolicy::default());

        let ctx = ctx_with_event(execution_event(Some(&auth)));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let (url, body) = stub.last_call().expect("exactly one POST should occur");
        assert_eq!(url, auth.url);
        assert_eq!(body, auth.payload, "the body must be byte-identical");

        let result = &ctx.nodes[APPROVE_AND_RUN_NODE_NAME];
        assert_eq!(result["posted"], json!(true));
        assert!(result.get("policy").is_some());
        let no_cost_key = ["cost", "usd"].join("_");
        assert!(result.get(&no_cost_key).is_none());
    }

    #[tokio::test]
    async fn non_authorized_verdict_drives_zero_posts() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = ApproveAndRunExecuteNode::new().with_http_post(Arc::new(stub.clone()));

        let ctx = ctx_with_event(execution_event(None));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(
            stub.last_call().is_none(),
            "a non-authorized verdict must never POST"
        );
        let result = &ctx.nodes[APPROVE_AND_RUN_NODE_NAME];
        assert_eq!(result["posted"], json!(false));
        assert_eq!(result["executed"], json!(false));
        assert!(result.get("policy").is_some());
    }

    #[test]
    fn no_node_writes_a_cost_usd_key() {
        let forbidden_key = format!("{:?}", ["cost", "usd"].join("_"));
        let src = include_str!("graph.rs");
        let occurrences = src.matches(&forbidden_key).count();
        assert_eq!(
            occurrences, 0,
            "ApproveAndRunExecuteNode must never stamp a cost_usd key into ctx.nodes"
        );
    }
}
