//! `DigestRenderNode` — deterministic renderer assembling
//! `ContentPipelineOutput` (EN.5.A task 11).
//!
//! No model call, no network: reads the envelope `SourceRouterNode` stored
//! under its own identity, whichever of `SummarizeNode`/`ReviseNode` most
//! recently stored a `SummaryResult` (mirrors `self_critic::read_summary`'s
//! / `translate::read_summary`'s read-preference precedent — tries
//! `SummarizeNode` first, falls back to `ReviseNode`), and `TranslateNode`'s
//! `TranslationResult` when the translate branch ran (`translate=false`
//! never invokes `TranslateNode` at all, so nothing is stored under its
//! identity on that path).
//!
//! Mints `artifact_id` **deterministically** from `envelope.envelope_id`
//! (UUID v5 over a fixed, run-invariant namespace) rather than
//! `Uuid::new_v4()` — webhook providers retry, and a fresh id per attempt
//! would write a duplicate `LearningArtifact` to the Brain on every retry
//! (2026-07-25 review). The same `envelope_id` always mints the same
//! `artifact_id`; two different `envelope_id`s never collide.
//!
//! Assembles a deterministic markdown digest from the stored summary, key
//! points, and entities, stamps `ContentPipelineOutput` under `NODE_NAME`,
//! and forwards to `PersistToBrainNode`.

use engine_contract::envelope::IngressEnvelope;
use engine_contract::TaskContext;
use serde_json::Value;
use uuid::Uuid;

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::revise;
use super::schema::ContentPipelineOutput;
use super::source_router;
use super::summarize::{self, SummaryResult};
use super::translate;

/// The `Node::name()` identity `DigestRenderNode` runs under, and the
/// `ctx.nodes` key its `ContentPipelineOutput` is stamped onto. Read by
/// `PersistToBrainNode`.
pub const NODE_NAME: &str = "DigestRenderNode";

/// A fixed, run-invariant name fed into [`Uuid::new_v5`] alongside
/// `Uuid::NAMESPACE_URL` to derive a stable namespace for `artifact_id`
/// minting. Arbitrary but constant across every process/run — changing it
/// would change every previously-minted `artifact_id`.
const ARTIFACT_NAMESPACE_SEED: &[u8] = b"engine-rs:content_pipeline:artifact_id";

/// Deterministically derives `artifact_id` from `envelope_id`: the same
/// `envelope_id` always mints the same `artifact_id`, so a retried webhook
/// (same `envelope_id`, fresh graph run) does not write a duplicate
/// `LearningArtifact` to the Brain.
#[must_use]
pub fn derive_artifact_id(envelope_id: &str) -> String {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, ARTIFACT_NAMESPACE_SEED);
    Uuid::new_v5(&namespace, envelope_id.as_bytes()).to_string()
}

/// Reads the envelope `SourceRouterNode` stored under its own identity.
fn read_envelope(ctx: &TaskContext) -> Result<IngressEnvelope, NodeError> {
    let stored = get_result(ctx, source_router::NODE_NAME).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: no envelope stored by {}",
            source_router::NODE_NAME
        ))
    })?;
    serde_json::from_value(
        stored
            .get("envelope")
            .cloned()
            .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: envelope field missing")))?,
    )
    .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid envelope: {err}")))
}

/// Reads whichever of `SummarizeNode`/`ReviseNode` most recently stored a
/// `SummaryResult` — mirrors `self_critic::read_summary`'s /
/// `translate::read_summary`'s read-preference precedent (tries
/// `ReviseNode` first, since it overwrites `SummarizeNode`'s output on the
/// loop's back-edge, falling back to `SummarizeNode`'s when no revise pass
/// ran).
fn read_summary(ctx: &TaskContext) -> Result<SummaryResult, NodeError> {
    let stored = get_result(ctx, revise::NODE_NAME)
        .or_else(|| get_result(ctx, summarize::NODE_NAME))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no summary stored by {} or {}",
                summarize::NODE_NAME,
                revise::NODE_NAME
            ))
        })?;
    serde_json::from_value(stored.clone())
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid stored SummaryResult: {err}")))
}

/// Reads `TranslateNode`'s `translated_markdown` when the translate branch
/// ran; `None` on the `translate=false` path.
fn read_translated_markdown(ctx: &TaskContext) -> Option<String> {
    get_result(ctx, translate::NODE_NAME)
        .and_then(|stored| stored.get("translated_markdown").cloned())
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Renders a deterministic markdown digest from the summary, key points,
/// and entities — no model call.
fn render_markdown(summary: &SummaryResult) -> String {
    let mut markdown = String::from("# Digest\n\n");
    markdown.push_str(&summary.summary);
    markdown.push('\n');

    if !summary.key_points.is_empty() {
        markdown.push_str("\n## Key Points\n");
        for point in &summary.key_points {
            markdown.push_str(&format!("- {point}\n"));
        }
    }

    if !summary.entities.is_empty() {
        markdown.push_str("\n## Entities\n");
        for entity in &summary.entities {
            markdown.push_str(&format!("- {entity}\n"));
        }
    }

    markdown
}

/// The deterministic renderer that assembles `ContentPipelineOutput`.
/// Forwards to `PersistToBrainNode`.
pub struct DigestRenderNode;

#[async_trait::async_trait]
impl Node for DigestRenderNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let envelope = read_envelope(&ctx)?;
        let summary = read_summary(&ctx)?;
        let translated_markdown = read_translated_markdown(&ctx);

        let output = ContentPipelineOutput {
            artifact_id: derive_artifact_id(&envelope.envelope_id),
            source_channel: envelope.channel_type,
            summary: summary.summary.clone(),
            entities: summary.entities.clone(),
            digest_markdown: render_markdown(&summary),
            digest_html: None,
            translated_markdown,
        };

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::to_value(&output).map_err(|err| {
                NodeError::new(format!("failed to serialize ContentPipelineOutput: {err}"))
            })?,
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

    use engine_contract::envelope::ChannelType;
    use serde_json::json;

    use super::*;

    fn ctx_with(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn envelope_json(envelope_id: &str, channel_type: &str) -> serde_json::Value {
        json!({
            "envelope_id": envelope_id,
            "channel_type": channel_type,
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "url", "url": "https://example.com/a" },
        })
    }

    fn ctx_with_envelope_and_summary(envelope_id: &str, channel_type: &str) -> TaskContext {
        let mut ctx = ctx_with(json!({}));
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({ "envelope": envelope_json(envelope_id, channel_type) }),
        );
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({
                "summary": "A concise summary.",
                "entities": ["Acme Corp"],
                "key_points": ["Point one", "Point two"],
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn process_renders_output_without_translation() {
        let node = DigestRenderNode;
        let ctx = ctx_with_envelope_and_summary("env-1", "web_article");

        let ctx = node.process(ctx).await.expect("process should succeed");

        let output: ContentPipelineOutput =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid output");
        assert_eq!(output.summary, "A concise summary.");
        assert_eq!(output.entities, vec!["Acme Corp".to_string()]);
        assert_eq!(output.source_channel, ChannelType::WebArticle);
        assert!(output.digest_markdown.contains("A concise summary."));
        assert!(output.digest_markdown.contains("Point one"));
        assert!(output.digest_markdown.contains("Acme Corp"));
        assert!(output.digest_html.is_none());
        assert!(output.translated_markdown.is_none());
        assert!(!output.artifact_id.is_empty());
    }

    #[tokio::test]
    async fn process_populates_translated_markdown_when_translate_node_ran() {
        let node = DigestRenderNode;
        let mut ctx = ctx_with_envelope_and_summary("env-1", "web_article");
        put_result(
            &mut ctx,
            translate::NODE_NAME,
            json!({ "translated_markdown": "# Resumo\n\nConteudo em portugues." }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let output: ContentPipelineOutput =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid output");
        assert_eq!(
            output.translated_markdown.as_deref(),
            Some("# Resumo\n\nConteudo em portugues.")
        );
    }

    #[tokio::test]
    async fn process_falls_back_to_revise_node_summary() {
        let node = DigestRenderNode;
        let mut ctx = ctx_with(json!({}));
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({ "envelope": envelope_json("env-1", "web_article") }),
        );
        put_result(
            &mut ctx,
            revise::NODE_NAME,
            json!({
                "summary": "A revised summary.",
                "entities": [],
                "key_points": [],
            }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");

        let output: ContentPipelineOutput =
            serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid output");
        assert_eq!(output.summary, "A revised summary.");
    }

    #[test]
    fn derive_artifact_id_is_deterministic_for_the_same_envelope_id() {
        let first = derive_artifact_id("env-1");
        let second = derive_artifact_id("env-1");
        assert_eq!(first, second);
    }

    #[test]
    fn derive_artifact_id_differs_across_envelope_ids() {
        let first = derive_artifact_id("env-1");
        let second = derive_artifact_id("env-2");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn process_errors_when_no_envelope_is_stored() {
        let node = DigestRenderNode;
        let mut ctx = ctx_with(json!({}));
        put_result(
            &mut ctx,
            summarize::NODE_NAME,
            json!({ "summary": "s", "entities": [], "key_points": [] }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no envelope stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_no_summary_is_stored() {
        let node = DigestRenderNode;
        let mut ctx = ctx_with(json!({}));
        put_result(
            &mut ctx,
            source_router::NODE_NAME,
            json!({ "envelope": envelope_json("env-1", "web_article") }),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no summary stored by"));
    }
}
