//! `ActionDispatchNode` — the terminal egress node of `CONTENT_PIPELINE`
//! (`EN.6.A` task 3), appended after `PersistToBrainNode` (task 4 wires the
//! graph edge). Deterministic, not a model node: it builds zero or more
//! `OutboundAction`s from the run's envelope and finished
//! `ContentPipelineOutput`, sends each through the injectable
//! [`ChannelTransport`] seam, and stores every receipt — it never fails the
//! run on a transport error (the Brain write already happened by the time
//! this node runs; egress is best-effort with an audit trail).
//!
//! Two kinds of outbound action:
//! - A `Digest` reply to `envelope.reply_context`, when present (a
//!   `reply_context`-less run — e.g. a web-article/YouTube fetch with no
//!   originating conversation — stays fire-and-forget: no send at all).
//! - A `TriggerWorkflow` action when the inbound event carries a `trigger`
//!   request (`{workflow_type, event}`), read directly off the raw event
//!   rather than through the typed `ContentPipelineInput` schema — `EN.6.A`
//!   task 4 is the task that adds this field to that struct; this node only
//!   needs its shape.
//!
//! Every stored receipt carries the run's `envelope_id` (the correlation
//! key `SourceRouterNode` stamps into `ctx.metadata`), so an inbound message
//! can be traced to its run and any child runs it triggered.
//!
//! Source of truth: `planning/EN.5.A-content-pipeline/architecture.md` §3.3,
//! §5, §7.2.

use std::sync::Arc;

use engine_contract::envelope::{ChannelType, IngressEnvelope, ReplyContext};
use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::channel_transport::{
    channel_transport_live, receipt_from_send_result, ChannelSendReceipt, ChannelTransport,
    OutboundAction, OutboundBody, DEFAULT_EVENTS_URL,
};
use crate::workflows::{get_result, put_result};

use super::digest_render;
use super::schema::ContentPipelineOutput;
use super::source_router::ENVELOPE_ID_METADATA_KEY;

/// The `Node::name()` identity `ActionDispatchNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "ActionDispatchNode";

/// An optional chain/trigger request an inbound event can carry, asking
/// `ActionDispatchNode` to dispatch a follow-on `TriggerWorkflow` action
/// after this run completes. Read directly off the raw `ctx.event` (rather
/// than through `ContentPipelineInput`) because `EN.6.A` task 4 is the task
/// that adds this field to the typed schema — task 3 only needs its shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerRequest {
    workflow_type: String,
    #[serde(default)]
    event: Value,
}

/// One dispatched `OutboundAction` plus its send receipt, as stored on
/// `ctx.nodes[NODE_NAME]`. Carries `envelope_id` alongside the receipt so
/// each send can be traced back to the run (and forward, for a
/// `TriggerWorkflow` action, to any child run it caused).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DispatchedAction {
    envelope_id: String,
    channel_type: ChannelType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply_context: Option<ReplyContext>,
    body: OutboundBody,
    receipt: ChannelSendReceipt,
}

/// Deserialize the inbound envelope from `ctx.event["envelope"]`.
fn parse_envelope(ctx: &TaskContext) -> Result<IngressEnvelope, NodeError> {
    let envelope_value = ctx
        .event
        .get("envelope")
        .cloned()
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: event missing `envelope`")))?;
    serde_json::from_value(envelope_value)
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid envelope: {err}")))
}

/// Deserialize the optional `trigger` chain request from the raw event, if
/// present. `None` (rather than an error) when the key is absent or `null`
/// — most runs never request chaining.
fn parse_trigger_request(ctx: &TaskContext) -> Result<Option<TriggerRequest>, NodeError> {
    match ctx.event.get("trigger") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: invalid `trigger` request: {err}"))
            }),
    }
}

/// Reads `DigestRenderNode`'s stored `ContentPipelineOutput`.
fn read_output(ctx: &TaskContext) -> Result<ContentPipelineOutput, NodeError> {
    let stored = get_result(ctx, digest_render::NODE_NAME).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: no ContentPipelineOutput stored by {}",
            digest_render::NODE_NAME
        ))
    })?;
    serde_json::from_value(stored.clone()).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: invalid stored ContentPipelineOutput: {err}"
        ))
    })
}

/// The run's correlation key: `ctx.metadata[ENVELOPE_ID_METADATA_KEY]` if
/// `SourceRouterNode` stamped it (the normal case), falling back to the
/// envelope's own `envelope_id` otherwise (defensive — e.g. a hand-built
/// `TaskContext` in a unit test that skips `SourceRouterNode`).
fn resolve_envelope_id(ctx: &TaskContext, envelope: &IngressEnvelope) -> String {
    ctx.metadata
        .get(ENVELOPE_ID_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| envelope.envelope_id.clone())
}

/// Build the zero-or-more `OutboundAction`s this run should send: a
/// `Digest` reply when `envelope.reply_context` is present, plus a
/// `TriggerWorkflow` action when the event requests chaining. The
/// `TriggerWorkflow` event carries `envelope_id` (this run's correlation
/// key) so a child run can be traced back to its parent.
fn build_actions(
    envelope: &IngressEnvelope,
    envelope_id: &str,
    output: &ContentPipelineOutput,
    trigger: Option<TriggerRequest>,
) -> Vec<OutboundAction> {
    let mut actions = Vec::new();

    if let Some(reply_context) = envelope.reply_context.clone() {
        actions.push(OutboundAction::new(
            envelope.channel_type,
            Some(reply_context),
            OutboundBody::Digest {
                markdown: output.digest_markdown.clone(),
                html: output.digest_html.clone(),
            },
        ));
    }

    if let Some(TriggerRequest {
        workflow_type,
        event,
    }) = trigger
    {
        let mut data = event;
        if !data.is_object() {
            data = json!({ "event": data });
        }
        data["envelope_id"] = json!(envelope_id);

        actions.push(OutboundAction::new(
            ChannelType::WorkflowTrigger,
            None,
            OutboundBody::TriggerWorkflow {
                workflow_type,
                event: data,
            },
        ));
    }

    actions
}

/// The terminal `CONTENT_PIPELINE` node: sends every outbound action for
/// this run over the injectable `ChannelTransport` seam. Never fails the
/// run — a transport error is recorded as a `delivered: false` receipt.
pub struct ActionDispatchNode {
    transport: Arc<dyn ChannelTransport>,
}

impl ActionDispatchNode {
    /// Construct with the live default `ChannelTransport`
    /// (`channel_transport_live`) targeting [`DEFAULT_EVENTS_URL`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: channel_transport_live(DEFAULT_EVENTS_URL),
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
}

impl Default for ActionDispatchNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for ActionDispatchNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let envelope = parse_envelope(&ctx)?;
        let output = read_output(&ctx)?;
        let trigger = parse_trigger_request(&ctx)?;
        let envelope_id = resolve_envelope_id(&ctx, &envelope);

        let actions = build_actions(&envelope, &envelope_id, &output, trigger);

        let mut dispatched = Vec::with_capacity(actions.len());
        for action in actions {
            let receipt = receipt_from_send_result(self.transport.send(&action).await);
            dispatched.push(DispatchedAction {
                envelope_id: envelope_id.clone(),
                channel_type: action.channel_type,
                reply_context: action.reply_context,
                body: action.body,
                receipt,
            });
        }

        let mut ctx = ctx;
        put_result(&mut ctx, NODE_NAME, json!({ "dispatched": dispatched }));

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

    fn empty_ctx(event: Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({ "envelope_id": "env-1" }),
            node_runs: HashMap::new(),
        }
    }

    fn envelope_json(channel_type: &str, reply_context: Option<Value>) -> Value {
        let mut envelope = json!({
            "envelope_id": "env-1",
            "channel_type": channel_type,
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "url", "url": "https://example.com/a" },
        });
        if let Some(reply_context) = reply_context {
            envelope["reply_context"] = reply_context;
        }
        envelope
    }

    fn slack_reply_context() -> Value {
        json!({
            "thread_id": "t-1",
            "conversation_id": null,
            "channel_token": "c-1",
        })
    }

    fn base_event(channel_type: &str, reply_context: Option<Value>) -> Value {
        json!({ "envelope": envelope_json(channel_type, reply_context) })
    }

    fn output_json() -> Value {
        json!({
            "artifact_id": "artifact-1",
            "source_channel": "slack",
            "summary": "A concise summary.",
            "entities": ["Acme Corp"],
            "digest_markdown": "# Digest\n\nA concise summary.",
            "digest_html": "<h1>Digest</h1>",
            "translated_markdown": null,
        })
    }

    #[tokio::test]
    async fn reply_context_present_sends_a_digest_reply() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut ctx = empty_ctx(base_event("slack", Some(slack_reply_context())));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let calls = stub.calls();
        assert_eq!(calls.len(), 1, "one reply action should have been sent");
        let action = &calls[0];
        assert_eq!(action.channel_type, ChannelType::Slack);
        assert!(action.reply_context.is_some());
        match &action.body {
            OutboundBody::Digest { markdown, html } => {
                assert_eq!(markdown, "# Digest\n\nA concise summary.");
                assert_eq!(html.as_deref(), Some("<h1>Digest</h1>"));
            }
            other => panic!("expected a Digest body, got {other:?}"),
        }

        let stored = &ctx.nodes[NODE_NAME];
        let dispatched = stored["dispatched"].as_array().expect("dispatched array");
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0]["envelope_id"], json!("env-1"));
        assert_eq!(dispatched[0]["receipt"]["delivered"], json!(true));
    }

    #[tokio::test]
    async fn reply_context_absent_sends_nothing_and_still_succeeds() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut ctx = empty_ctx(base_event("web_article", None));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.calls().is_empty(), "no reply_context, no send");
        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["dispatched"], json!([]));
    }

    #[tokio::test]
    async fn transport_failure_is_recorded_as_a_delivered_false_receipt_not_a_run_failure() {
        let stub = Arc::new(StubChannelTransport::failing());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut ctx = empty_ctx(base_event("slack", Some(slack_reply_context())));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let ctx = node
            .process(ctx)
            .await
            .expect("a failed send must not fail the run");

        let stored = &ctx.nodes[NODE_NAME];
        let dispatched = stored["dispatched"].as_array().expect("dispatched array");
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0]["receipt"]["delivered"], json!(false));
        assert_eq!(
            dispatched[0]["receipt"]["detail"],
            json!("stub configured to fail")
        );
    }

    #[tokio::test]
    async fn trigger_request_builds_a_trigger_workflow_action_carrying_envelope_id() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut event = base_event("web_article", None);
        event["trigger"] = json!({
            "workflow_type": "OTHER_WORKFLOW",
            "event": { "foo": "bar" },
        });
        let mut ctx = empty_ctx(event);
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        node.process(ctx).await.expect("process should succeed");

        let calls = stub.calls();
        assert_eq!(calls.len(), 1, "only the trigger action should have sent");
        let action = &calls[0];
        assert_eq!(action.channel_type, ChannelType::WorkflowTrigger);
        assert!(action.reply_context.is_none());
        match &action.body {
            OutboundBody::TriggerWorkflow {
                workflow_type,
                event,
            } => {
                assert_eq!(workflow_type, "OTHER_WORKFLOW");
                assert_eq!(event["foo"], json!("bar"));
                assert_eq!(event["envelope_id"], json!("env-1"));
            }
            other => panic!("expected a TriggerWorkflow body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reply_and_trigger_both_present_sends_both_actions() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut event = base_event("slack", Some(slack_reply_context()));
        event["trigger"] = json!({
            "workflow_type": "OTHER_WORKFLOW",
            "event": {},
        });
        let mut ctx = empty_ctx(event);
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert_eq!(stub.calls().len(), 2);
        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["dispatched"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn envelope_id_falls_back_to_the_envelope_when_metadata_is_unstamped() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut ctx = TaskContext {
            event: base_event("slack", Some(slack_reply_context())),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let stored = &ctx.nodes[NODE_NAME];
        assert_eq!(stored["dispatched"][0]["envelope_id"], json!("env-1"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_output_is_present() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let ctx = empty_ctx(base_event("slack", Some(slack_reply_context())));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no ContentPipelineOutput stored"));
    }

    #[tokio::test]
    async fn process_errors_when_event_is_missing_envelope() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut ctx = empty_ctx(json!({ "not_envelope": "oops" }));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("event missing `envelope`"));
    }

    #[tokio::test]
    async fn process_errors_on_an_invalid_trigger_request() {
        let stub = Arc::new(StubChannelTransport::succeeding());
        let node = ActionDispatchNode::new().with_transport(stub.clone());

        let mut event = base_event("web_article", None);
        event["trigger"] = json!({ "not_a_workflow_type": true });
        let mut ctx = empty_ctx(event);
        put_result(&mut ctx, digest_render::NODE_NAME, output_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid `trigger` request"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = ActionDispatchNode::default();
    }

    #[test]
    fn name_matches_node_name_const() {
        let node = ActionDispatchNode::new();
        assert_eq!(node.name(), NODE_NAME);
    }
}
