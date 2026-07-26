//! `SourceRouterNode` — impls `Node` + `Router`. Routes on the parsed
//! `IngressEnvelope`'s `SourcePayload` **kind**, not `channel_type`
//! (EN.5.A task 4 — see `planning/EN.5.A-content-pipeline/architecture.md`
//! §5). There are only three acquisition shapes, so every `ChannelType`
//! (including every one Phase 6 adds) collapses onto three branches:
//!
//! - `SourcePayload::Url`             -> `FetchArticleNode`
//! - `SourcePayload::VideoId`         -> `FetchTranscriptNode`
//! - everything else (`ChannelMessage`, `TaskContextRef`,
//!   `WorkflowTrigger`)              -> `NormalizeChannelContentNode`
//!
//! `TaskContextRef` and `WorkflowTrigger` payloads route to
//! `NormalizeChannelContentNode` like any other text-shaped source — this
//! replaces the earlier channel-type match that hard-failed on
//! `ResearchAgent`/`WorkflowTrigger` naming the unowned `EN.6.E` block (see
//! the 2026-07-25 tasks.md amendment).
//!
//! A `Router::route` takes `&TaskContext` and cannot mutate it (`routing.rs`
//! contract), so validation and policy resolution happen in `process()`:
//! it parses + validates `ContentPipelineInput`, resolves the run policy
//! through `profiles::resolve_policy_for_run_from` against
//! `PolicyConfigSource::Builtin` (EN.5.D's worktree-independent path — a
//! channel envelope has no repo checkout to derive a worktree from), and
//! stores the normalized envelope + resolved policy snapshot under
//! `NODE_NAME` so `route()` and downstream nodes (`CriticRouterNode` reads
//! the stored loop bounds) can read them back. It also stamps `envelope_id`
//! into `ctx.metadata` as the run's correlation key.

use engine_contract::envelope::{IngressEnvelope, SourcePayload};
use engine_contract::TaskContext;

use crate::node::{InputBinding, Node, NodeError};
use crate::policy::PolicyConfigSource;
use crate::routing::Router;
use crate::workflows::{get_result, put_result};

use super::profiles::resolve_policy_for_run_from;
use super::schema::ContentPipelineInput;

/// The `Node::name()` identity `SourceRouterNode` is registered under.
pub const NODE_NAME: &str = "SourceRouterNode";

/// `ctx.metadata` key the resolved `envelope_id` is stamped under —
/// downstream nodes' correlation key for this run.
pub const ENVELOPE_ID_METADATA_KEY: &str = "envelope_id";

/// The downstream identity a `SourcePayload::Url` source routes to — bound
/// via [`InputBinding`] rather than a literal written inline in `route`
/// (mirrors `proposal_generator::review_router`'s convention; `fetch_article`
/// is a task-5 module and does not yet export a `NODE_NAME` const).
fn fetch_article_target() -> InputBinding {
    InputBinding::bound("FetchArticleNode")
}

/// The downstream identity a `SourcePayload::VideoId` source routes to.
fn fetch_transcript_target() -> InputBinding {
    InputBinding::bound("FetchTranscriptNode")
}

/// The downstream identity every other `SourcePayload` kind
/// (`ChannelMessage`, `TaskContextRef`, `WorkflowTrigger`) routes to.
fn normalize_channel_content_target() -> InputBinding {
    InputBinding::bound("NormalizeChannelContentNode")
}

/// Resolve a bound identity. The fallback is never observed here — every
/// binding above is constructed via [`InputBinding::bound`] — it exists
/// only to satisfy [`InputBinding::resolve`]'s signature.
fn resolve_bound(binding: &InputBinding) -> &str {
    binding.resolve("")
}

/// Parses + validates the inbound `CONTENT_PIPELINE` event, resolves the
/// run policy, and routes on the envelope's `SourcePayload` kind.
pub struct SourceRouterNode;

#[async_trait::async_trait]
impl Node for SourceRouterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let input: ContentPipelineInput = serde_json::from_value(ctx.event.clone())
            .map_err(|err| NodeError::new(format!("invalid CONTENT_PIPELINE event: {err}")))?;

        // Channel envelopes have no repo checkout to derive a worktree
        // from, so this resolves against `PolicyConfigSource::Builtin`
        // (EN.5.D) rather than `resolve_policy_for_run(ctx, &worktree)`.
        let policy = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)?;

        let envelope_id = input.envelope.envelope_id.clone();

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                "envelope": input.envelope,
                "policy": policy,
            }),
        );

        if !ctx.metadata.is_object() {
            ctx.metadata = serde_json::json!({});
        }
        ctx.metadata[ENVELOPE_ID_METADATA_KEY] = serde_json::Value::String(envelope_id);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for SourceRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let stored = get_result(ctx, NODE_NAME)?;
        let envelope: IngressEnvelope =
            serde_json::from_value(stored.get("envelope")?.clone()).ok()?;

        let target = match envelope.source {
            SourcePayload::Url { .. } => fetch_article_target(),
            SourcePayload::VideoId { .. } => fetch_transcript_target(),
            SourcePayload::ChannelMessage { .. }
            | SourcePayload::TaskContextRef { .. }
            | SourcePayload::WorkflowTrigger { .. } => normalize_channel_content_target(),
        };

        Some(resolve_bound(&target).to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event_with(channel_type: &str, source: serde_json::Value) -> serde_json::Value {
        json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": channel_type,
                "timestamp": "2026-07-25T00:00:00Z",
                "source": source,
            }
        })
    }

    fn ctx_with_event(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: std::collections::HashMap::new(),
            metadata: json!({}),
            node_runs: std::collections::HashMap::new(),
        }
    }

    async fn processed(event: serde_json::Value) -> TaskContext {
        let node = SourceRouterNode;
        node.process(ctx_with_event(event))
            .await
            .expect("process should succeed")
    }

    #[tokio::test]
    async fn url_payload_routes_to_fetch_article() {
        let ctx = processed(event_with(
            "web_article",
            json!({ "kind": "url", "url": "https://example.com/a" }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(router.route(&ctx), Some("FetchArticleNode".to_string()));
    }

    #[tokio::test]
    async fn video_id_payload_routes_to_fetch_transcript() {
        let ctx = processed(event_with(
            "youtube_transcript",
            json!({ "kind": "video_id", "video_id": "abc123" }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(router.route(&ctx), Some("FetchTranscriptNode".to_string()));
    }

    #[tokio::test]
    async fn channel_message_payload_routes_to_normalize() {
        let ctx = processed(event_with(
            "slack",
            json!({ "kind": "channel_message", "text": "hi", "attachments": [] }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(
            router.route(&ctx),
            Some("NormalizeChannelContentNode".to_string())
        );
    }

    #[tokio::test]
    async fn task_context_ref_payload_routes_to_normalize() {
        let ctx = processed(event_with(
            "research_agent",
            json!({ "kind": "task_context_ref", "workflow_type": "RESEARCH_AGENT" }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(
            router.route(&ctx),
            Some("NormalizeChannelContentNode".to_string())
        );
    }

    #[tokio::test]
    async fn workflow_trigger_payload_routes_to_normalize() {
        let ctx = processed(event_with(
            "workflow_trigger",
            json!({ "kind": "workflow_trigger", "workflow_type": "SDLC_FLOW", "event": {} }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(
            router.route(&ctx),
            Some("NormalizeChannelContentNode".to_string())
        );
    }

    #[tokio::test]
    async fn different_channel_types_with_same_payload_kind_take_the_same_branch() {
        let slack_ctx = processed(event_with(
            "slack",
            json!({ "kind": "channel_message", "text": "hi", "attachments": [] }),
        ))
        .await;
        let telegram_ctx = processed(event_with(
            "telegram",
            json!({ "kind": "channel_message", "text": "hi", "attachments": [] }),
        ))
        .await;
        let router = SourceRouterNode;
        assert_eq!(router.route(&slack_ctx), router.route(&telegram_ctx));
        assert_eq!(
            router.route(&slack_ctx),
            Some("NormalizeChannelContentNode".to_string())
        );
    }

    #[tokio::test]
    async fn envelope_and_policy_are_stored_under_node_name() {
        let ctx = processed(event_with(
            "web_article",
            json!({ "kind": "url", "url": "https://example.com/a" }),
        ))
        .await;
        let stored = get_result(&ctx, NODE_NAME).expect("result stored");
        let envelope = stored.get("envelope").expect("envelope stored");
        assert_eq!(
            envelope.get("envelope_id").and_then(|v| v.as_str()),
            Some("env-1")
        );
        let stored_policy = stored.get("policy").expect("policy stored");
        assert_eq!(
            stored_policy
                .get("max_critic_iterations")
                .and_then(|v| v.as_u64()),
            Some(3)
        );
    }

    #[tokio::test]
    async fn envelope_id_is_stamped_into_metadata() {
        let ctx = processed(event_with(
            "web_article",
            json!({ "kind": "url", "url": "https://example.com/a" }),
        ))
        .await;
        assert_eq!(
            ctx.metadata
                .get(ENVELOPE_ID_METADATA_KEY)
                .and_then(|v| v.as_str()),
            Some("env-1")
        );
    }

    #[tokio::test]
    async fn process_rejects_invalid_event() {
        let node = SourceRouterNode;
        let ctx = ctx_with_event(json!({ "not_an_envelope": true }));
        let err = node.process(ctx).await.expect_err("should reject");
        assert!(err.message.contains("invalid CONTENT_PIPELINE event"));
    }

    #[tokio::test]
    async fn process_rejects_out_of_range_policy_override() {
        let node = SourceRouterNode;
        let mut event = event_with(
            "web_article",
            json!({ "kind": "url", "url": "https://example.com/a" }),
        );
        event["policy"] = json!({ "max_critic_iterations": 999 });
        let ctx = ctx_with_event(event);
        let err = node.process(ctx).await.expect_err("should reject");
        assert!(err.message.contains("max_critic_iterations"));
    }

    #[test]
    fn as_router_is_some() {
        let router = SourceRouterNode;
        assert!(router.as_router().is_some());
    }

    #[test]
    fn targets_are_bound_input_bindings() {
        assert!(fetch_article_target().is_bound());
        assert!(fetch_transcript_target().is_bound());
        assert!(normalize_channel_content_target().is_bound());
    }

    #[test]
    fn resolve_bound_reads_through_the_binding_not_a_literal_default() {
        assert_eq!(
            InputBinding::bound("FetchArticleNode").resolve("nonsense-fallback"),
            "FetchArticleNode"
        );
    }
}
