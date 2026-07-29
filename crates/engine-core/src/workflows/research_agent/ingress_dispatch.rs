//! `ResearchIngressDispatchNode` (`EN.6.E` task 3) — the terminal node that
//! wraps a finished `RESEARCH_AGENT` run's research output as an
//! `IngressEnvelope` and sends one `TriggerWorkflow` `OutboundAction`
//! through the existing `ChannelTransport` egress seam (`EN.6.A`), closing
//! the self-feeding loop into `CONTENT_PIPELINE` — default-off.
//!
//! Modeled directly on
//! `crate::workflows::content_pipeline::action_dispatch::ActionDispatchNode`
//! for the seam-calling / best-effort-receipt shape, and on
//! `crate::nodes::materialize_doc::MaterializeDocNode`'s `with_enabled` +
//! in-place no-op for the policy-knob shape (`CLAUDE.md` standing rule 6):
//! the node stays in the declared graph at every setting and never fails
//! the run on a transport error.
//!
//! **Policy resolution:** a served run resolves `ResearchAgentPolicy` once
//! at dispatch time and seeds it into `ctx.nodes[RESOLVED_POLICY_IDENTITY]`
//! (`engine-serve`'s `register_research_agent`) — the same stamp
//! `CompanyResearchNode`/`ProspectingResearchNode` read via
//! `crate::policy::resolved_policy_strict`. This node reads it the same
//! way, so a served run's `policy`/`profile` override is honoured even
//! though `RESEARCH_AGENT` has no dedicated setup node. When no such stamp
//! is present (a narrow unit test driving this node in isolation), it falls
//! back to the `enabled`/`target_workflow_type` values set on the node
//! itself (`with_enabled`/`with_target_workflow_type`, both defaulting to
//! [`super::policy::IngressDispatch::default`]).
//!
//! **Envelope id determinism:** reuses
//! `ctx.metadata["envelope_id"]` when present, otherwise derives from the
//! first path `MaterializeDocNode` stamped onto `ctx.nodes` — never
//! `Uuid::new_v4()`, so the same input dispatched twice produces the same
//! `envelope_id` (a requirement of the e2e suite in task 5).
//!
//! Source of truth: `planning/EN.6.E-research-ingress-dispatch/tasks.md`.

use std::sync::Arc;

use chrono::Utc;
use engine_contract::envelope::{ChannelType, IngressEnvelope, SourcePayload};
use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::channel_transport::{
    channel_transport_live, receipt_from_send_result, ChannelTransport, OutboundAction,
    OutboundBody, DEFAULT_EVENTS_URL,
};
use crate::nodes::materialize_doc;
use crate::workflows::{get_result, put_result};

use super::policy::{IngressDispatch, ResearchAgentPolicy};

/// The `Node::name()` identity `ResearchIngressDispatchNode` runs under,
/// and the `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "ResearchIngressDispatchNode";

/// The two research terminal nodes, in the order this node prefers reading
/// their stored output — mirrors the `with_source_nodes` preference
/// `MaterializeDocNode`/`MergeContactsNode` are configured with in
/// `graph.rs` (exactly one of the two runs per event, so this is really a
/// "whichever one ran" lookup, not a genuine precedence).
const RESEARCH_SOURCE_NODES: [&str; 2] = ["CompanyResearchNode", "ProspectingResearchNode"];

/// The terminal `RESEARCH_AGENT` node: sends an ingress-tail
/// `TriggerWorkflow` action for this run's research output over the
/// injectable [`ChannelTransport`] seam, unless the resolved
/// `ingress_dispatch` knob is disabled (the behavior-stable default).
pub struct ResearchIngressDispatchNode {
    transport: Arc<dyn ChannelTransport>,
    enabled: bool,
    target_workflow_type: String,
}

impl ResearchIngressDispatchNode {
    /// Construct with the live default `ChannelTransport`
    /// (`channel_transport_live`) targeting [`DEFAULT_EVENTS_URL`], and the
    /// behavior-stable `IngressDispatch` default (`enabled: false`,
    /// `target_workflow_type: "CONTENT_PIPELINE"`) as the fallback used
    /// when no resolved policy is stamped on the run's `ctx`.
    #[must_use]
    pub fn new() -> Self {
        let defaults = IngressDispatch::default();
        Self {
            transport: channel_transport_live(DEFAULT_EVENTS_URL),
            enabled: defaults.enabled,
            target_workflow_type: defaults.target_workflow_type,
        }
    }

    /// Override the `ChannelTransport` seam. Tests inject a
    /// `StubChannelTransport` so the gated suite never sends over a real
    /// channel or hits a live `/events/` endpoint.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn ChannelTransport>) -> Self {
        self.transport = transport;
        self
    }

    /// Override the fallback `enabled` value used when the run's `ctx`
    /// carries no stamped `ResolvedPolicy` (a resolved policy stamp always
    /// wins when present).
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Override the fallback `target_workflow_type` value used when the
    /// run's `ctx` carries no stamped `ResolvedPolicy`.
    #[must_use]
    pub fn with_target_workflow_type(mut self, target_workflow_type: impl Into<String>) -> Self {
        self.target_workflow_type = target_workflow_type.into();
        self
    }

    /// Resolve the effective `IngressDispatch` knob for this run: the
    /// stamped `ResolvedPolicy` (`crate::policy::RESOLVED_POLICY_IDENTITY`)
    /// when present — the same stamp the two terminal research nodes read
    /// via `crate::policy::resolved_policy_strict` — falling back to this
    /// node's own `enabled`/`target_workflow_type` fields only when no stamp
    /// is present at all (a narrow unit test driving this node in
    /// isolation). A stamp that *is* present but fails to deserialize (a
    /// corrupted or mismatched `ResolvedPolicy`) propagates as a hard
    /// error, same as `CompanyResearchNode`/`ProspectingResearchNode` — a
    /// silent fallback there would mask a serialization regression as a
    /// quiet no-op instead of failing loudly.
    fn resolve_dispatch(&self, ctx: &TaskContext) -> Result<IngressDispatch, NodeError> {
        match crate::policy::resolved_policy_strict::<ResearchAgentPolicy>(ctx) {
            Ok(policy) => Ok(policy.ingress_dispatch),
            Err(err) if err.message.contains("no resolved policy stamped") => Ok(IngressDispatch {
                enabled: self.enabled,
                target_workflow_type: self.target_workflow_type.clone(),
            }),
            Err(err) => Err(err),
        }
    }
}

impl Default for ResearchIngressDispatchNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the finished research output stored by whichever of
/// `CompanyResearchNode`/`ProspectingResearchNode` ran this run, via
/// [`get_result`].
fn read_research_output(ctx: &TaskContext) -> Result<Value, NodeError> {
    for upstream in RESEARCH_SOURCE_NODES {
        if let Some(stored) = get_result(ctx, upstream) {
            return Ok(stored.clone());
        }
    }
    Err(NodeError::new(format!(
        "{NODE_NAME}: no research output stored by {}",
        RESEARCH_SOURCE_NODES.join(" or ")
    )))
}

/// Resolve this run's correlation key deterministically (never
/// `Uuid::new_v4()`): `ctx.metadata["envelope_id"]` when present, otherwise
/// derived from the first path `MaterializeDocNode` stamped onto
/// `ctx.nodes`. Identical input yields an identical `envelope_id`.
fn resolve_envelope_id(ctx: &TaskContext) -> Result<String, NodeError> {
    if let Some(id) = ctx.metadata.get("envelope_id").and_then(Value::as_str) {
        return Ok(id.to_string());
    }

    let materialized_path = get_result(ctx, materialize_doc::NODE_NAME)
        .and_then(|value| value.get("paths"))
        .and_then(Value::as_array)
        .and_then(|paths| paths.first())
        .and_then(Value::as_str);

    match materialized_path {
        Some(path) => Ok(format!("research-agent:{path}")),
        None => Err(NodeError::new(format!(
            "{NODE_NAME}: cannot derive a deterministic envelope_id — no \
             ctx.metadata[\"envelope_id\"] and no path stamped by {}",
            materialize_doc::NODE_NAME
        ))),
    }
}

#[async_trait::async_trait]
impl Node for ResearchIngressDispatchNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let dispatch = self.resolve_dispatch(&ctx)?;

        if !dispatch.enabled {
            let mut ctx = ctx;
            put_result(
                &mut ctx,
                NODE_NAME,
                json!({
                    "skipped": true,
                    "enabled": false,
                    "target_workflow_type": dispatch.target_workflow_type,
                }),
            );
            return Ok(ctx);
        }

        let research_output = read_research_output(&ctx)?;

        // A materialize that legitimately planned zero actions (e.g.
        // re-researching a company whose opportunity doc is already up to
        // date) stamps no path and leaves no `envelope_id` to derive from.
        // That is this run succeeding with nothing new to dispatch, not a
        // failure — skip the dispatch in place rather than failing the run
        // via `?`, matching the disabled branch's no-op shape.
        let envelope_id = match resolve_envelope_id(&ctx) {
            Ok(id) => id,
            Err(_) => {
                let mut ctx = ctx;
                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "skipped": true,
                        "enabled": true,
                        "target_workflow_type": dispatch.target_workflow_type,
                        "reason": "no_envelope_id_to_derive_from",
                    }),
                );
                return Ok(ctx);
            }
        };
        let chain_depth = ctx
            .event
            .get("chain_depth")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        let envelope = IngressEnvelope {
            envelope_id: envelope_id.clone(),
            channel_type: ChannelType::ResearchAgent,
            sender_id: None,
            reply_context: None,
            timestamp: Utc::now().to_rfc3339(),
            source: SourcePayload::TaskContextRef {
                workflow_type: "RESEARCH_AGENT".to_string(),
                event_id: None,
                inline: Some(research_output),
            },
            raw_payload: Value::Null,
        };

        let event = json!({
            "envelope": envelope,
            "chain_depth": chain_depth,
        });

        let action = OutboundAction {
            channel_type: ChannelType::WorkflowTrigger,
            reply_context: None,
            body: OutboundBody::TriggerWorkflow {
                workflow_type: dispatch.target_workflow_type.clone(),
                event,
            },
        };

        let receipt = receipt_from_send_result(self.transport.send(&action).await);

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "skipped": false,
                "enabled": true,
                "target_workflow_type": dispatch.target_workflow_type,
                "envelope_id": envelope_id,
                "receipt": receipt,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use crate::nodes::channel_transport::StubChannelTransport;

    use super::*;

    fn base_ctx(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn research_output() -> Value {
        json!({
            "company_name": "Acme Corp",
            "summary": "A concise brief.",
            "contacts": [],
        })
    }

    fn ctx_with_research_output(event: Value) -> TaskContext {
        let mut ctx = base_ctx(event);
        put_result(&mut ctx, "CompanyResearchNode", research_output());
        put_result(
            &mut ctx,
            materialize_doc::NODE_NAME,
            json!({ "paths": ["opportunities/acme-corp.md"] }),
        );
        ctx
    }

    #[tokio::test]
    async fn no_op_materialize_skips_dispatch_without_failing_the_run() {
        // MaterializeDocNode legitimately stamps an empty `paths` list when
        // it finds nothing new to write (e.g. re-researching a company
        // whose opportunity doc is already up to date) — this must be a
        // soft no-op here too, not a hard failure of the whole run.
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true);

        let mut ctx = base_ctx(json!({}));
        put_result(&mut ctx, "CompanyResearchNode", research_output());
        put_result(&mut ctx, materialize_doc::NODE_NAME, json!({ "paths": [] }));

        let ctx = node
            .process(ctx)
            .await
            .expect("a no-op materialize must not fail the run");

        assert!(
            stub.calls().is_empty(),
            "no path to derive an envelope_id from should send nothing"
        );
        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["skipped"], json!(true));
        assert_eq!(stored["enabled"], json!(true));
    }

    #[tokio::test]
    async fn corrupted_resolved_policy_stamp_fails_the_run() {
        // Unlike a missing stamp (an acceptable, narrow-unit-test fallback),
        // a stamp that is present but fails to deserialize must propagate,
        // matching CompanyResearchNode/ProspectingResearchNode's behavior.
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new().with_transport(stub.clone());

        let mut ctx = ctx_with_research_output(json!({}));
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            json!({ "not": "a valid ResearchAgentPolicy" }),
        );

        let err = node
            .process(ctx)
            .await
            .expect_err("a corrupted resolved-policy stamp must fail the run");
        assert!(err.message.contains("failed to parse resolved policy"));
        assert!(stub.calls().is_empty());
    }

    #[tokio::test]
    async fn disabled_node_makes_zero_sends_and_stamps_skipped() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(false);

        let ctx = ctx_with_research_output(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.calls().is_empty(), "disabled node should send nothing");
        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["skipped"], json!(true));
        assert_eq!(stored["enabled"], json!(false));
    }

    #[tokio::test]
    async fn enabled_node_sends_exactly_one_trigger_workflow_action() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true)
            .with_target_workflow_type("CONTENT_PIPELINE");

        let ctx = ctx_with_research_output(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let calls = stub.calls();
        assert_eq!(calls.len(), 1, "exactly one action should have been sent");
        let action = &calls[0];
        assert_eq!(action.channel_type, ChannelType::WorkflowTrigger);
        match &action.body {
            OutboundBody::TriggerWorkflow {
                workflow_type,
                event,
            } => {
                assert_eq!(workflow_type, "CONTENT_PIPELINE");
                let input: crate::workflows::content_pipeline::schema::ContentPipelineInput =
                    serde_json::from_value(event.clone())
                        .expect("event should deserialize as ContentPipelineInput");
                assert_eq!(input.envelope.channel_type, ChannelType::ResearchAgent);
                match input.envelope.source {
                    SourcePayload::TaskContextRef {
                        workflow_type,
                        inline,
                        ..
                    } => {
                        assert_eq!(workflow_type, "RESEARCH_AGENT");
                        assert_eq!(inline, Some(research_output()));
                    }
                    other => panic!("expected TaskContextRef, got {other:?}"),
                }
            }
            other => panic!("expected a TriggerWorkflow body, got {other:?}"),
        }

        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["skipped"], json!(false));
        assert_eq!(stored["enabled"], json!(true));
    }

    #[tokio::test]
    async fn chain_depth_is_propagated_not_reset() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true);

        let ctx = ctx_with_research_output(json!({ "chain_depth": 3 }));
        node.process(ctx).await.expect("process should succeed");

        let action = stub.last_call().expect("one call recorded");
        match action.body {
            OutboundBody::TriggerWorkflow { event, .. } => {
                assert_eq!(event["chain_depth"], json!(3));
            }
            other => panic!("expected TriggerWorkflow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failing_transport_leaves_the_run_successful_with_delivered_false() {
        let stub = Arc::new(StubChannelTransport::failing());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true);

        let ctx = ctx_with_research_output(json!({}));
        let ctx = node
            .process(ctx)
            .await
            .expect("a failed send must not fail the run");

        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["receipt"]["delivered"], json!(false));
    }

    #[tokio::test]
    async fn same_input_dispatched_twice_yields_the_same_envelope_id() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true);

        let ctx1 = ctx_with_research_output(json!({}));
        let ctx1 = node
            .process(ctx1)
            .await
            .expect("first process should succeed");

        let ctx2 = ctx_with_research_output(json!({}));
        let ctx2 = node
            .process(ctx2)
            .await
            .expect("second process should succeed");

        assert_eq!(
            ctx1.nodes[NODE_NAME]["envelope_id"],
            ctx2.nodes[NODE_NAME]["envelope_id"]
        );
    }

    #[tokio::test]
    async fn envelope_id_prefers_metadata_when_stamped() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true);

        let mut ctx = ctx_with_research_output(json!({}));
        ctx.metadata = json!({ "envelope_id": "env-from-metadata" });
        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            ctx.nodes[NODE_NAME]["envelope_id"],
            json!("env-from-metadata")
        );
    }

    #[tokio::test]
    async fn resolved_knob_values_are_stamped_in_ctx_nodes() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(true)
            .with_target_workflow_type("CUSTOM_PIPELINE");

        let ctx = ctx_with_research_output(json!({}));
        let ctx = node.process(ctx).await.expect("process should succeed");

        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["enabled"], json!(true));
        assert_eq!(stored["target_workflow_type"], json!("CUSTOM_PIPELINE"));
    }

    #[tokio::test]
    async fn stamped_resolved_policy_overrides_node_level_enabled_fallback() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        // Node-level fallback says disabled, but the stamped resolved
        // policy says enabled — the stamp must win.
        let node = ResearchIngressDispatchNode::new()
            .with_transport(stub.clone())
            .with_enabled(false);

        let mut ctx = ctx_with_research_output(json!({}));
        let policy = ResearchAgentPolicy {
            ingress_dispatch: IngressDispatch {
                enabled: true,
                target_workflow_type: "CONTENT_PIPELINE".to_string(),
            },
            ..ResearchAgentPolicy::default()
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(policy).expect("policy serializes"),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            stub.calls().len(),
            1,
            "stamped policy should win over fallback"
        );
        assert_eq!(ctx.nodes[NODE_NAME]["enabled"], json!(true));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = ResearchIngressDispatchNode::default();
    }

    #[test]
    fn name_matches_node_name_const() {
        let node = ResearchIngressDispatchNode::new();
        assert_eq!(node.name(), NODE_NAME);
    }
}
