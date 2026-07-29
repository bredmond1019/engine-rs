//! `PersistToBrainNode` — the terminal, harvest-gate-governed push of a
//! `LearningArtifact` to Synapse `POST /ingest/*` (EN.5.A task 11; D51: no
//! embedding or pgvector in engine-rs — this seam only POSTs).
//!
//! EN.4.C pattern (mirrors `proposal_generator::persist_to_brain`):
//! `with_http_post`/`with_url` builders, live default `http_post_live()`.
//! `EN.7.D` task 4: the payload is no longer built inline here — it
//! delegates to `super::learning_artifact::build_learning_artifact_payload`,
//! the single source of the `LearningArtifact` payload shape
//! `{artifact_id, channel_type, source_ref, summary, digest_markdown,
//! entities, language}` shared with `LearningArtifactPayloadNode` (the two
//! consumers can never drift). `MaterializeDocNode` now runs upstream of
//! this node in the declared `CONTENT_PIPELINE` graph (task 6): the source
//! `.md` is written before this node's Synapse push, so a failed push no
//! longer costs the run its written document (D53).
//!
//! `EN.7.C` task 4: this node is now the execution site of the generic
//! `crate::nodes::harvest_gate::HarvestGate` every pipeline inherits. The
//! payload is always built via `build_learning_artifact_payload` first and
//! unconditionally — payload derivation never forks by mode, which is what
//! keeps a deferred (`approval`) push byte-identical to what an `in_process`
//! push would have sent. The node then branches on `HarvestGate::decide()`:
//!
//! * `Post` (`in_process`) — POST over the `HttpPost` seam exactly as
//!   before task 4.
//! * `Skip` (`off`, the default) — no POST at all; the run continues, and
//!   indexing is left to the existing manifest / `index_brain` freshness
//!   reindex.
//! * `Defer` (`approval`) — no POST; a pending record
//!   (`crate::nodes::harvest_gate::pending_harvest_record`) is stamped so an
//!   operator can complete the harvest later via `HARVEST_APPROVE`, which
//!   POSTs the exact same payload to the same URL. The materialized doc
//!   path(s) are read from the upstream `MaterializeDocNode` result's
//!   `paths` field; an absent/skipped upstream yields an empty list, not an
//!   error.
//!
//! The order relative to `MaterializeDocNode` is unchanged: the `.md` is
//! always written identically regardless of harvest mode. `process` stamps
//! **one stable key set** in all three modes —
//! `{posted, skipped, harvest_mode, status, artifact_id, response,
//! pending}` — with `status`/`response`/`pending` set to `null` where they
//! do not apply, and `harvest_mode` carrying the resolved mode string so
//! telemetry can attribute cost to the setting that caused it (CLAUDE.md
//! rule 6). A non-2xx response (or transport failure) on an *attempted*
//! push (`in_process`) still surfaces as a `NodeError` — the gate changes
//! whether the push happens, never how a real failure is reported. Terminal
//! in EN.5.A: no forward connection (EN.6.A wires `ActionDispatchNode` after
//! this — see the commented-out edit in
//! `planning/EN.5.A-content-pipeline/architecture.md` §5).

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::harvest_gate::{pending_harvest_record, HarvestDecision, HarvestGate};
use crate::nodes::http_post::{http_post_live, HttpPost};
use crate::nodes::materialize_doc;
use crate::workflows::{get_result, put_result};

use super::learning_artifact::build_learning_artifact_payload;

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

/// The terminal node that POSTs the finished `ContentPipelineOutput` to the
/// brain ingest endpoint over the injectable `HttpPost` seam. No forward
/// connection in EN.5.A.
pub struct PersistToBrainNode {
    http_post: std::sync::Arc<dyn HttpPost>,
    url: String,
    harvest: HarvestGate,
}

impl PersistToBrainNode {
    /// Construct with the live `reqwest`-backed `HttpPost` impl, POSTing to
    /// [`BRAIN_INGEST_URL`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            http_post: http_post_live(),
            url: BRAIN_INGEST_URL.to_string(),
            harvest: HarvestGate::default(),
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

    /// Override the resolved harvest gate. Defaults to `HarvestGate::default()`
    /// (`off` — no explicit ingest POST; the freshness reindex indexes the
    /// materialized `.md`).
    #[must_use]
    pub fn with_harvest(mut self, gate: HarvestGate) -> Self {
        self.harvest = gate;
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
        // Built first and unconditionally, in every mode: payload derivation
        // must not fork by harvest mode, which is what makes a deferred
        // (`approval`) push byte-identical to what an `in_process` push
        // would have sent.
        let payload = build_learning_artifact_payload(&ctx)?;
        let artifact_id = payload["artifact_id"].clone();
        let harvest_mode = self.harvest.mode();

        let mut ctx = ctx;

        match self.harvest.decide() {
            HarvestDecision::Post => {
                let response = self
                    .http_post
                    .post(&self.url, payload)
                    .await
                    .map_err(|err| {
                        NodeError::new(format!("{NODE_NAME}: brain ingest push failed: {err}"))
                    })?;

                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "posted": true,
                        "skipped": false,
                        "harvest_mode": harvest_mode,
                        "status": response.status,
                        "artifact_id": artifact_id,
                        "response": response.body,
                        "pending": null,
                    }),
                );
            }
            HarvestDecision::Skip => {
                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "posted": false,
                        "skipped": true,
                        "harvest_mode": harvest_mode,
                        "status": null,
                        "artifact_id": artifact_id,
                        "response": null,
                        "pending": null,
                    }),
                );
            }
            HarvestDecision::Defer => {
                let doc_paths = get_result(&ctx, materialize_doc::NODE_NAME)
                    .and_then(|result| result.get("paths"))
                    .and_then(|paths| paths.as_array())
                    .map(|paths| {
                        paths
                            .iter()
                            .filter_map(|p| p.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let artifact_id_str = artifact_id.as_str().unwrap_or_default().to_string();
                let pending =
                    pending_harvest_record(artifact_id_str, self.url.clone(), payload, doc_paths);

                put_result(
                    &mut ctx,
                    NODE_NAME,
                    json!({
                        "posted": false,
                        "skipped": true,
                        "harvest_mode": harvest_mode,
                        "status": null,
                        "artifact_id": artifact_id,
                        "response": null,
                        "pending": pending,
                    }),
                );
            }
        }

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

    use crate::nodes::harvest_gate::HarvestMode;
    use crate::nodes::http_post::StubHttpPost;
    use crate::workflows::content_pipeline::{digest_render, fetch_article, fetch_transcript};

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
            .with_url("https://brain.example/ingest/learning")
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

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
        assert_eq!(result["skipped"], json!(false));
        assert_eq!(result["harvest_mode"], json!("in_process"));
        assert_eq!(result["status"], json!(200));
        assert_eq!(result["artifact_id"], json!("artifact-1"));
        assert_eq!(result["pending"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn process_posts_a_body_identical_to_the_shared_builder_output() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let expected = build_learning_artifact_payload(&ctx).expect("builder should succeed");

        node.process(ctx).await.expect("process should succeed");

        let (_url, body) = stub.last_call().expect("post should have been recorded");
        assert_eq!(body, expected);
    }

    #[tokio::test]
    async fn process_uses_target_lang_when_translated_markdown_is_present() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

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
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

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
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub))
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

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

    // -- EN.7.C harvest gate ----------------------------------------------

    #[tokio::test]
    async fn default_node_makes_zero_posts_under_the_new_off_default() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        // No `with_harvest` call at all: the new `off` default, asserted so
        // this behavior change can never be silent.
        let node = PersistToBrainNode::new().with_http_post(std::sync::Arc::new(stub.clone()));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_none(), "should make zero HTTP calls");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(false));
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["harvest_mode"], json!("off"));
    }

    #[tokio::test]
    async fn off_mode_makes_zero_posts_and_stamps_the_documented_shape() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_harvest(HarvestGate::new(HarvestMode::Off));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_none(), "should make zero HTTP calls");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(false));
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["harvest_mode"], json!("off"));
    }

    #[tokio::test]
    async fn approval_mode_makes_zero_posts_and_stamps_a_pending_record() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub.clone()))
            .with_url("https://brain.example/ingest/learning")
            .with_harvest(HarvestGate::new(HarvestMode::Approval));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());
        put_result(
            &mut ctx,
            materialize_doc::NODE_NAME,
            json!({
                "materialized": true,
                "skipped": false,
                "dry_run": false,
                "model": "learning-artifact",
                "paths": ["brain/content/learning/artifact-1.md"],
                "warnings": [],
            }),
        );

        let expected_payload =
            build_learning_artifact_payload(&ctx).expect("builder should succeed");

        let ctx = node.process(ctx).await.expect("process should succeed");

        assert!(stub.last_call().is_none(), "should make zero HTTP calls");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(result["posted"], json!(false));
        assert_eq!(result["skipped"], json!(true));
        assert_eq!(result["harvest_mode"], json!("approval"));

        let pending = &result["pending"];
        assert_eq!(pending["artifact_id"], json!("artifact-1"));
        assert_eq!(
            pending["url"],
            json!("https://brain.example/ingest/learning")
        );
        assert_eq!(pending["payload"], expected_payload);
        assert_eq!(
            pending["doc_paths"],
            json!(["brain/content/learning/artifact-1.md"])
        );
    }

    #[tokio::test]
    async fn approval_mode_defaults_doc_paths_to_empty_when_no_upstream_materialize_stamp() {
        let stub = StubHttpPost::succeeding(json!({"ok": true}));
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub))
            .with_harvest(HarvestGate::new(HarvestMode::Approval));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let ctx = node.process(ctx).await.expect("process should succeed");

        let pending = &ctx.nodes[NODE_NAME]["pending"];
        assert_eq!(pending["doc_paths"], json!([]));
    }

    #[tokio::test]
    async fn all_three_harvest_modes_stamp_an_identical_key_set() {
        let expected_keys: BTreeSet<String> = [
            "posted",
            "skipped",
            "harvest_mode",
            "status",
            "artifact_id",
            "response",
            "pending",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        for mode in [
            HarvestMode::Off,
            HarvestMode::InProcess,
            HarvestMode::Approval,
        ] {
            let stub = StubHttpPost::succeeding(json!({"ok": true}));
            let node = PersistToBrainNode::new()
                .with_http_post(std::sync::Arc::new(stub))
                .with_harvest(HarvestGate::new(mode));

            let mut ctx = empty_ctx(base_event(false));
            put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
            put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

            let ctx = node.process(ctx).await.expect("process should succeed");
            let object = ctx.nodes[NODE_NAME]
                .as_object()
                .expect("result is an object");
            assert_eq!(
                object.keys().cloned().collect::<BTreeSet<_>>(),
                expected_keys,
                "mode {mode:?} stamped a different key set"
            );
        }
    }

    #[tokio::test]
    async fn a_stub_failure_under_in_process_is_still_a_node_error() {
        let stub = StubHttpPost::failing("brain endpoint unreachable");
        let node = PersistToBrainNode::new()
            .with_http_post(std::sync::Arc::new(stub))
            .with_harvest(HarvestGate::new(HarvestMode::InProcess));

        let mut ctx = empty_ctx(base_event(false));
        put_result(&mut ctx, digest_render::NODE_NAME, output_json(false));
        put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("brain endpoint unreachable"));
    }

    #[tokio::test]
    async fn a_missing_upstream_digest_is_still_an_error_in_every_mode() {
        for mode in [
            HarvestMode::Off,
            HarvestMode::InProcess,
            HarvestMode::Approval,
        ] {
            let stub = StubHttpPost::succeeding(json!({"ok": true}));
            let node = PersistToBrainNode::new()
                .with_http_post(std::sync::Arc::new(stub))
                .with_harvest(HarvestGate::new(mode));

            let mut ctx = empty_ctx(base_event(false));
            put_result(&mut ctx, fetch_article::NODE_NAME, content_json());

            let err = node.process(ctx).await.expect_err("should fail");
            assert!(
                err.message.contains("no ContentPipelineOutput stored"),
                "mode {mode:?} did not error as expected: {}",
                err.message
            );
        }
    }
}
