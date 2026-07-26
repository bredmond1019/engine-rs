//! `PersistToBrainNode` — the terminal `HttpPost` write of a
//! `LearningArtifact` to Synapse `POST /ingest/*` (EN.5.A task 11; D51: no
//! embedding or pgvector in engine-rs — this seam only POSTs).
//!
//! EN.4.C pattern (mirrors `proposal_generator::persist_to_brain`):
//! `with_http_post`/`with_url` builders, live default `http_post_live()`.
//! Reads `DigestRenderNode`'s stored `ContentPipelineOutput` plus the
//! `source_ref` whichever of `FetchArticleNode`/`FetchTranscriptNode`/
//! `NormalizeChannelContentNode` converged on (task 5's `{title, text,
//! source_ref}` shape — mirrors `summarize::read_converged_content`'s
//! read-preference), and builds the `LearningArtifact` payload
//! `{artifact_id, channel_type, source_ref, summary, digest_markdown,
//! entities, language}`. `language` is the event's `target_lang` when
//! `translated_markdown` is present on the output, `"en"` otherwise (the
//! digest was never translated, so it's still in its original language).
//!
//! On success, stamps `{"posted": true, "status", "artifact_id",
//! "response"}` onto `ctx.nodes[NODE_NAME]`. A non-2xx response (or
//! transport failure) surfaces as a `NodeError` — a failed brain push halts
//! the run rather than silently dropping the finished digest. Terminal in
//! EN.5.A: no forward connection (EN.6.A wires `ActionDispatchNode` after
//! this — see the commented-out edit in
//! `planning/EN.5.A-content-pipeline/architecture.md` §5).

use engine_contract::TaskContext;
use serde_json::{json, Value};

use crate::node::{Node, NodeError};
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::workflows::{get_result, put_result};

use super::schema::{ContentPipelineInput, ContentPipelineOutput};
use super::{digest_render, fetch_article, fetch_transcript, normalize_channel_content};

/// The `Node::name()` identity `PersistToBrainNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "PersistToBrainNode";

/// The Synapse brain ingest endpoint this node POSTs to (`OR.Q`,
/// `POST /ingest/*`). Not configurable via `ContentPipelinePolicy` — the
/// endpoint address is deployment topology, not a per-run policy knob. Live
/// ingest URL is a placeholder until Synapse `OR.Q` wiring lands (same
/// status as EN.4.C); tests stub `HttpPost`, so this does not gate the
/// block.
const BRAIN_INGEST_URL: &str = "http://localhost:8000/ingest/learning";

/// The `language` value POSTed when the digest was never translated.
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
/// `source_ref` — mirrors `summarize::read_converged_content`'s
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

/// The terminal node that POSTs the finished `ContentPipelineOutput` to the
/// brain ingest endpoint over the injectable `HttpPost` seam. No forward
/// connection in EN.5.A.
pub struct PersistToBrainNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    url: String,
}

impl PersistToBrainNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl, POSTing to
    /// [`BRAIN_INGEST_URL`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            url: BRAIN_INGEST_URL.to_string(),
        }
    }

    /// Override the `HttpPost` seam. Tests inject a `StubHttpPost` so the
    /// gated suite never contacts a live brain endpoint.
    #[must_use]
    pub fn with_http_post(mut self, http_post: std::sync::Arc<dyn HttpPost>) -> Self {
        self.http_post = http_post;
        self
    }

    /// Override the target URL. Tests use this to assert on the exact URL
    /// the stub was POSTed to; production code leaves it at
    /// [`BRAIN_INGEST_URL`].
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }
}

impl Default for PersistToBrainNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for PersistToBrainNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let output = read_output(&ctx)?;
        let source_ref = read_source_ref(&ctx)?;

        let language = if output.translated_markdown.is_some() {
            event.target_lang.clone()
        } else {
            DEFAULT_LANGUAGE.to_string()
        };

        let channel_type = serde_json::to_value(output.source_channel)
            .map_err(|err| NodeError::new(format!("failed to serialize channel_type: {err}")))?;

        let payload = json!({
            "artifact_id": output.artifact_id,
            "channel_type": channel_type,
            "source_ref": source_ref,
            "summary": output.summary,
            "digest_markdown": output.digest_markdown,
            "entities": output.entities,
            "language": language,
        });

        let response = self
            .http_post
            .post(&self.url, payload)
            .await
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: brain ingest push failed: {err}"))
            })?;

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "posted": true,
                "status": response.status,
                "artifact_id": output.artifact_id,
                "response": response.body,
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
    use std::collections::{BTreeSet, HashMap};

    use serde_json::json;

    use crate::nodes::http_post::StubHttpPost;

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

    #[tokio::test]
    async fn process_posts_the_expected_learning_artifact_payload() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/learning");

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let (url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(url, "https://brain.example/ingest/learning");

        let object = body.as_object().expect("payload is an object");
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
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        assert_eq!(body["artifact_id"], json!("artifact-1"));
        assert_eq!(body["channel_type"], json!("web_article"));
        assert_eq!(body["source_ref"], json!("https://example.com/a"));
        assert_eq!(body["summary"], json!("A concise summary."));
        assert_eq!(
            body["digest_markdown"],
            json!("# Digest\n\nA concise summary.")
        );
        assert_eq!(body["entities"], json!(["Acme Corp"]));
        assert_eq!(body["language"], json!("en"));

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(true));
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["artifact_id"], json!("artifact-1"));
    }

    #[tokio::test]
    async fn process_uses_target_lang_when_translated_markdown_is_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let mut event = base_event(true);
        event["target_lang"] = json!("pt-BR");
        let mut ctx = empty_ctx(event);
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(true));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["language"], json!("pt-BR"));
    }

    #[tokio::test]
    async fn process_reads_source_ref_from_fetch_transcript_when_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(
            &mut ctx,
            fetch_transcript::NODE_NAME,
            json!({
                "title": "A Video",
                "text": "Transcript text.",
                "source_ref": "yt:abc123",
            }),
        );

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body["source_ref"], json!("yt:abc123"));
    }

    #[tokio::test]
    async fn process_surfaces_a_stub_failure_as_a_node_error() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("brain endpoint unreachable"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_output_is_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no ContentPipelineOutput stored"));
    }

    #[tokio::test]
    async fn process_errors_when_no_source_ref_content_is_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no content stored by"));
    }

    #[tokio::test]
    async fn process_errors_when_event_is_invalid() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub));

        let mut ctx = TaskContext {
            event: json!({ "not_envelope": "oops" }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid CONTENT_PIPELINE event"));
    }

    #[test]
    fn default_constructs_without_panicking() {
        let _node = PersistToBrainNode::default();
    }
}
