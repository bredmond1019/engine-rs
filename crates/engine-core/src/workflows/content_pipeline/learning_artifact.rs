//! Shared `LearningArtifact` payload builder + `LearningArtifactPayloadNode`
//! (`EN.7.D` task 3).
//!
//! `build_learning_artifact_payload` is extracted, byte-for-byte, from the
//! payload construction that used to live inline in
//! `persist_to_brain::PersistToBrainNode::process` (task 4 makes that node
//! delegate here instead of building it twice). It reads `DigestRenderNode`'s
//! stored `ContentPipelineOutput`, the converged `source_ref` (the same
//! `FetchArticleNode` -> `FetchTranscriptNode` -> `NormalizeChannelContentNode`
//! read-preference `persist_to_brain`/`summarize` use), and the event's
//! `target_lang`, and emits exactly the seven-field shape
//! `okf_core::LearningArtifact::from_payload` consumes: `{artifact_id,
//! channel_type, source_ref, summary, digest_markdown, entities, language}`.
//!
//! `LearningArtifactPayloadNode` is the pipeline-local adapter that stamps
//! that payload flat as its own `ctx.nodes` result, so a downstream
//! `MaterializeDocNode::with_source_node(NODE_NAME)` can read the artifact
//! directly (mirrors `crate::nodes::materialize_doc`'s `with_source_node`
//! contract). Forward-only: no disk, no network.

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::schema::{ContentPipelineInput, ContentPipelineOutput};
use super::{digest_render, fetch_article, fetch_transcript, normalize_channel_content};

/// The `Node::name()` identity `LearningArtifactPayloadNode` runs under, and
/// the `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "LearningArtifactPayloadNode";

/// The `language` value emitted when the digest was never translated.
const DEFAULT_LANGUAGE: &str = "en";

/// Deserialize the inbound `CONTENT_PIPELINE` event from `ctx.event` — only
/// `target_lang` is needed here, when the digest was translated.
fn parse_event(ctx: &TaskContext) -> Result<ContentPipelineInput, NodeError> {
    serde_json::from_value(ctx.event.clone()).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: invalid CONTENT_PIPELINE event: {err}"
        ))
    })
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

/// Reads whichever of the three task-5 converge nodes ran and pulls its
/// `source_ref` — mirrors `persist_to_brain::read_source_ref`'s
/// read-preference (`FetchArticleNode` -> `FetchTranscriptNode` ->
/// `NormalizeChannelContentNode`).
fn read_source_ref(ctx: &TaskContext) -> Result<String, NodeError> {
    let stored = get_result(ctx, fetch_article::NODE_NAME)
        .or_else(|| get_result(ctx, fetch_transcript::NODE_NAME))
        .or_else(|| get_result(ctx, normalize_channel_content::NODE_NAME))
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no content stored by {}, {}, or {}",
                fetch_article::NODE_NAME,
                fetch_transcript::NODE_NAME,
                normalize_channel_content::NODE_NAME
            ))
        })?;
    stored
        .get("source_ref")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: content missing `source_ref`")))
}

/// Derive the OKF `title` field from a digest's own leading Markdown
/// heading: the first line of `digest_markdown`, if it starts (after
/// trimming leading whitespace) with one or more `#` characters, has those
/// `#` characters and any following whitespace stripped and is returned as
/// the title. Falls back to `artifact_id` when `digest_markdown` is empty or
/// its first line is not a heading, so the returned title is never empty —
/// `okf_core::str_field` defaults a missing/empty key to `""`, and an empty
/// `title` would still fail OKF validation while looking wired.
fn derive_title(digest_markdown: &str, artifact_id: &str) -> String {
    let first_line = digest_markdown.lines().next().unwrap_or("").trim_start();

    if let Some(stripped) = first_line.strip_prefix('#') {
        let heading = stripped.trim_start_matches('#').trim();
        if !heading.is_empty() {
            return heading.to_string();
        }
    }

    artifact_id.to_string()
}

/// Build the `LearningArtifact` payload
/// `{artifact_id, channel_type, source_ref, summary, digest_markdown,
/// entities, language, title, description}` — the seven-field shape
/// `okf_core::LearningArtifact::from_payload` has always consumed, plus the
/// `title`/`description` keys it now also reads (`OK.ticket.
/// learning-artifact-missing-title-description-task1`, commit `fe8da8a`) —
/// from the run's `TaskContext`. Shared by `LearningArtifactPayloadNode` and
/// `PersistToBrainNode` so the two consumers can never drift.
///
/// `language` is the event's `target_lang` when `translated_markdown` is
/// present on the digest output, [`DEFAULT_LANGUAGE`] (`"en"`) otherwise (the
/// digest was never translated, so it's still in its original language).
///
/// `title` is [`derive_title`]'s result over `output.digest_markdown` and
/// `output.artifact_id`. `description` is `output.summary` verbatim — OKF
/// frontmatter's `description` field and CONTENT_PIPELINE's `summary` are
/// the same string carried under two keys, not two sources of truth.
pub fn build_learning_artifact_payload(ctx: &TaskContext) -> Result<Value, NodeError> {
    let event = parse_event(ctx)?;
    let output = read_output(ctx)?;
    let source_ref = read_source_ref(ctx)?;

    let language = if output.translated_markdown.is_some() {
        event.target_lang.clone()
    } else {
        DEFAULT_LANGUAGE.to_string()
    };

    let channel_type = serde_json::to_value(output.source_channel).map_err(|err| {
        NodeError::new(format!(
            "{NODE_NAME}: failed to serialize channel_type: {err}"
        ))
    })?;

    let title = derive_title(&output.digest_markdown, &output.artifact_id);

    Ok(json!({
        "artifact_id": output.artifact_id,
        "channel_type": channel_type,
        "source_ref": source_ref,
        "summary": output.summary,
        "digest_markdown": output.digest_markdown,
        "entities": output.entities,
        "language": language,
        "title": title,
        "description": output.summary,
    }))
}

/// The pipeline-local adapter: builds the `LearningArtifact` payload via
/// [`build_learning_artifact_payload`] and stamps it flat as its own
/// `ctx.nodes` result, so a downstream
/// `MaterializeDocNode::with_source_node(NODE_NAME)` reads the artifact
/// directly. Forward-only — no disk, no network.
#[derive(Default)]
pub struct LearningArtifactPayloadNode;

impl LearningArtifactPayloadNode {
    /// Construct the node.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Node for LearningArtifactPayloadNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let payload = build_learning_artifact_payload(&ctx)?;

        let mut ctx = ctx;
        put_result(&mut ctx, NODE_NAME, payload);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use serde_json::json;

    use super::*;

    fn empty_ctx(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn base_event(translate: bool) -> serde_json::Value {
        json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" },
            },
            "translate": translate,
        })
    }

    fn output_json(translated: bool) -> serde_json::Value {
        json!({
            "artifact_id": "artifact-1",
            "source_channel": "web_article",
            "summary": "A concise summary.",
            "entities": ["Acme Corp"],
            "digest_markdown": "# Digest\n\nA concise summary.",
            "digest_html": null,
            "translated_markdown": if translated {
                serde_json::Value::String("# Resumo".to_string())
            } else {
                serde_json::Value::Null
            },
        })
    }

    fn content_json() -> serde_json::Value {
        json!({
            "title": "Example Title",
            "text": "Example body text.",
            "source_ref": "https://example.com/a",
        })
    }

    /// Like [`output_json`], but with a caller-supplied `digest_markdown` so
    /// the title-derivation cases below can exercise
    /// `build_learning_artifact_payload` end to end rather than only
    /// [`derive_title`] in isolation.
    fn output_json_with_digest(digest_markdown: &str) -> serde_json::Value {
        json!({
            "artifact_id": "artifact-1",
            "source_channel": "web_article",
            "summary": "A concise summary.",
            "entities": ["Acme Corp"],
            "digest_markdown": digest_markdown,
            "digest_html": null,
            "translated_markdown": null,
        })
    }

    #[tokio::test]
    async fn builder_emits_the_exact_expected_payload_for_an_untranslated_run() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");

        let object = payload.as_object().expect("payload is an object");
        assert_eq!(
            object.keys().cloned().collect::<BTreeSet<_>>(),
            [
                "artifact_id",
                "channel_type",
                "source_ref",
                "summary",
                "digest_markdown",
                "entities",
                "language",
                "title",
                "description",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        assert_eq!(payload["artifact_id"], json!("artifact-1"));
        assert_eq!(payload["channel_type"], json!("web_article"));
        assert_eq!(payload["source_ref"], json!("https://example.com/a"));
        assert_eq!(payload["summary"], json!("A concise summary."));
        assert_eq!(
            payload["digest_markdown"],
            json!("# Digest\n\nA concise summary.")
        );
        assert_eq!(payload["entities"], json!(["Acme Corp"]));
        assert_eq!(payload["language"], json!("en"));
        assert_eq!(payload["title"], json!("Digest"));
        assert_eq!(payload["description"], json!("A concise summary."));
    }

    #[tokio::test]
    async fn builder_uses_target_lang_when_translated_markdown_is_present() {
        let mut event = base_event(true);
        event["target_lang"] = json!("pt-BR");
        let mut ctx = empty_ctx(event);
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(true));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        assert_eq!(payload["language"], json!("pt-BR"));
    }

    #[tokio::test]
    async fn builder_errors_when_no_upstream_output_is_present() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = build_learning_artifact_payload(&ctx).expect_err("should fail");
        assert!(err.message.contains("LearningArtifactPayloadNode"));
        assert!(err.message.contains("no ContentPipelineOutput stored"));
    }

    #[tokio::test]
    async fn builder_errors_when_no_source_ref_content_is_present() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));

        let err = build_learning_artifact_payload(&ctx).expect_err("should fail");
        assert!(err.message.contains("LearningArtifactPayloadNode"));
        assert!(err.message.contains("no content stored by"));
    }

    #[tokio::test]
    async fn builder_errors_when_event_is_invalid() {
        let mut ctx = TaskContext {
            event: json!({ "not_envelope": "oops" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = build_learning_artifact_payload(&ctx).expect_err("should fail");
        assert!(err.message.contains("invalid CONTENT_PIPELINE event"));
    }

    #[tokio::test]
    async fn node_stamps_the_payload_flat_under_its_own_name() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let node = LearningArtifactPayloadNode::new();
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["artifact_id"], json!("artifact-1"));
        assert_eq!(result["source_ref"], json!("https://example.com/a"));
        assert_eq!(result["language"], json!("en"));
    }

    #[tokio::test]
    async fn node_surfaces_builder_errors_as_prefixed_node_errors() {
        let ctx = empty_ctx(base_event(false));

        let node = LearningArtifactPayloadNode;
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("LearningArtifactPayloadNode"));
    }

    #[test]
    fn derive_title_uses_the_leading_heading_when_present() {
        assert_eq!(
            derive_title("# Real Title\n\nSome body text.", "artifact-1"),
            "Real Title"
        );
    }

    #[test]
    fn derive_title_strips_extra_leading_hashes_and_whitespace() {
        assert_eq!(
            derive_title("##   Spaced Title  \n\nbody", "artifact-1"),
            "Spaced Title"
        );
    }

    #[test]
    fn derive_title_falls_back_to_artifact_id_when_no_leading_heading() {
        assert_eq!(
            derive_title("Just prose, no heading here.", "artifact-1"),
            "artifact-1"
        );
    }

    #[test]
    fn derive_title_falls_back_to_artifact_id_when_digest_is_empty() {
        assert_eq!(derive_title("", "artifact-1"), "artifact-1");
    }

    // -- Task 2: extend the suite for title/description, end to end through
    // `build_learning_artifact_payload` (not only `derive_title` in
    // isolation), plus the okf-core round trip. Per D68, these were red
    // before task 1 landed `title`/`description` on the payload and okf-core
    // commit `fe8da8a` taught `LearningArtifact` to read/emit them; task 1
    // already applied in this working tree, so these assert the green state
    // directly rather than re-demonstrating the prior failure.

    #[tokio::test]
    async fn builder_derives_title_from_the_digests_leading_heading() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(
            &mut ctx,
            digest_render::NODE_NAME,
            output_json_with_digest("# Real Title\n\nSome body text."),
        );
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        assert_eq!(payload["title"], json!("Real Title"));
    }

    #[tokio::test]
    async fn builder_falls_back_to_artifact_id_when_digest_has_no_leading_heading() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(
            &mut ctx,
            digest_render::NODE_NAME,
            output_json_with_digest("Just prose, no heading here."),
        );
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        assert_eq!(payload["title"], json!("artifact-1"));
    }

    #[tokio::test]
    async fn builder_falls_back_to_artifact_id_when_digest_is_empty() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(
            &mut ctx,
            digest_render::NODE_NAME,
            output_json_with_digest(""),
        );
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        assert_eq!(payload["title"], json!("artifact-1"));
    }

    #[tokio::test]
    async fn builder_description_equals_summary_verbatim() {
        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        assert_eq!(payload["description"], payload["summary"]);
        assert_eq!(payload["description"], json!("A concise summary."));
    }

    #[tokio::test]
    async fn payload_round_trips_through_okf_core_learning_artifact_frontmatter() {
        use okf_core::{BrainDocModel, LearningArtifact};

        let mut ctx = empty_ctx(base_event(false));
        put_result(
            &mut ctx,
            digest_render::NODE_NAME,
            output_json_with_digest("# Real Title\n\nSome body text."),
        );
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let payload = build_learning_artifact_payload(&ctx).expect("should build");
        let artifact = LearningArtifact::from_payload(&payload);
        let fields = artifact.frontmatter();

        let title = fields
            .iter()
            .find(|(key, _)| key == "title")
            .map(|(_, value)| value.clone());
        let description = fields
            .iter()
            .find(|(key, _)| key == "description")
            .map(|(_, value)| value.clone());

        assert_eq!(
            title,
            Some(okf_core::FrontmatterValue::Scalar("Real Title".to_string()))
        );
        assert_eq!(
            description,
            Some(okf_core::FrontmatterValue::Scalar(
                "A concise summary.".to_string()
            ))
        );
    }
}
