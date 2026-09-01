//! Hermetic end-to-end suite for the materialize→harvest gate (`EN.7.C`
//! task 8) — the real `Workflow::run` pointer-walk loop over `CONTENT_PIPELINE`
//! with stubbed model transports and a stubbed `HttpPost`, but the **real**
//! `MevDocMaterializer` writing into a `tempfile::tempdir()` corpus, proving
//! the three `HarvestMode`s end to end and the `HARVEST_APPROVE` hand-off
//! between them.
//!
//! Modeled on `content_pipeline_materialize_e2e.rs`'s registry/fixture
//! shape. Every test builds its own tempdir corpus and never pre-creates a
//! subdirectory it depends on the writer creating (the
//! `ensure_plan_parents()` lesson from `EN.7.D`), and makes zero real
//! network calls — `PersistToBrainNode`/`HarvestApproveNode`'s `HttpPost`
//! seam is always a stub.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::harvest_approve::HarvestApproveNode;
use engine_core::nodes::harvest_gate::{HarvestGate, HarvestMode};
use engine_core::nodes::http_post::{HttpPost, HttpPostResponse, StubHttpPost};
use engine_core::nodes::materialize_doc::MaterializeDocNode;
use engine_core::workflow::Workflow;
use engine_core::workflows::content_pipeline::action_dispatch::ActionDispatchNode;
use engine_core::workflows::content_pipeline::critic_router::CriticRouterNode;
use engine_core::workflows::content_pipeline::digest_render::DigestRenderNode;
use engine_core::workflows::content_pipeline::fetch_article::{
    FetchArticleNode, FetchedContent, StubArticleFetch,
};
use engine_core::workflows::content_pipeline::fetch_transcript::{
    FetchTranscriptNode, FetchedTranscript, StubTranscriptFetch,
};
use engine_core::workflows::content_pipeline::graph;
use engine_core::workflows::content_pipeline::increment_critic_iteration::IncrementCriticIterationNode;
use engine_core::workflows::content_pipeline::learning_artifact::LearningArtifactPayloadNode;
use engine_core::workflows::content_pipeline::normalize_channel_content::NormalizeChannelContentNode;
use engine_core::workflows::content_pipeline::persist_to_brain::PersistToBrainNode;
use engine_core::workflows::content_pipeline::revise::ReviseNode;
use engine_core::workflows::content_pipeline::self_critic::SelfCriticNode;
use engine_core::workflows::content_pipeline::source_router::SourceRouterNode;
use engine_core::workflows::content_pipeline::summarize::SummarizeNode;
use engine_core::workflows::content_pipeline::translate::{TranslateNode, TranslateSkipRouterNode};
use engine_core::workflows::harvest_approve;
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};

use engine_core::nodes::channel_transport::StubChannelTransport;

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/harvest-gate-e2e";

// ---------------------------------------------------------------------------
// Fixtures / transports
// ---------------------------------------------------------------------------

fn stub_outcome(structured: Value) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: claude_code_rs::parse::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: Default::default(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        session_id: None,
        structured_output: Some(structured),
    }
}

fn stub_transport_returning(structured: Value) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured)) }.boxed()
    })
}

fn summary_json() -> Value {
    json!({
        "summary": "A concise summary of the source about Acme Corp.",
        "key_points": ["Acme ships agents", "Rust throughout"],
        "entities": ["Acme Corp", "Rust"],
    })
}

fn critic_json() -> Value {
    json!({ "verdict": "pass", "confidence": 0.95, "issues": [] })
}

/// The learning corpus directory `okf_core::LearningArtifact`'s
/// `index_intent` registers into — pre-created so the first write has a
/// directory to land in (mirrors `content_pipeline_materialize_e2e.rs`).
fn learning_corpus_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join("docs/content/learning-corpus");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A counting wrapper around [`StubHttpPost`] so tests can assert on how
/// many times the seam was actually called, not just what the last call
/// carried.
#[derive(Clone)]
struct CountingHttpPost {
    inner: StubHttpPost,
    calls: Arc<AtomicUsize>,
}

impl CountingHttpPost {
    fn succeeding(body: Value) -> Self {
        Self {
            inner: StubHttpPost::succeeding(body),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn failing(error: impl Into<String>) -> Self {
        Self {
            inner: StubHttpPost::failing(error),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_call(&self) -> Option<(String, Value)> {
        self.inner.last_call()
    }
}

#[async_trait]
impl HttpPost for CountingHttpPost {
    async fn post(&self, url: &str, json_body: Value) -> Result<HttpPostResponse, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.post(url, json_body).await
    }
}

// ---------------------------------------------------------------------------
// Registry construction
// ---------------------------------------------------------------------------

/// Build a `CONTENT_PIPELINE` registry with stubbed model transports, fetch
/// seams, and channel transport, the real `MevDocMaterializer` pinned at
/// `root`, and `PersistToBrainNode` wired to `http_post` under the given
/// harvest gate.
fn registry_at(root: &Path, gate: HarvestGate, http_post: Arc<dyn HttpPost>) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SourceRouterNode));
    registry.register(Box::new(FetchArticleNode::new().with_fetch(Arc::new(
        StubArticleFetch::succeeding(FetchedContent {
            title: Some("Example Title".to_string()),
            text: "Example article body about Acme Corp.".to_string(),
        }),
    ))));
    registry.register(Box::new(FetchTranscriptNode::new().with_fetch(Arc::new(
        StubTranscriptFetch::succeeding(FetchedTranscript {
            title: Some("A Talk".to_string()),
            text: "Transcript body about Acme Corp.".to_string(),
        }),
    ))));
    registry.register(Box::new(NormalizeChannelContentNode));
    registry.register(Box::new(
        SummarizeNode::new().with_transport(stub_transport_returning(summary_json())),
    ));
    registry.register(Box::new(
        SelfCriticNode::new().with_transport(stub_transport_returning(critic_json())),
    ));
    registry.register(Box::new(CriticRouterNode));
    registry.register(Box::new(IncrementCriticIterationNode));
    registry.register(Box::new(
        ReviseNode::new().with_transport(stub_transport_returning(summary_json())),
    ));
    registry.register(Box::new(TranslateSkipRouterNode));
    registry.register(Box::new(
        TranslateNode::new().with_transport(stub_transport_returning(summary_json())),
    ));
    registry.register(Box::new(DigestRenderNode));
    registry.register(Box::new(LearningArtifactPayloadNode));
    // The real materializer — harvest mode never affects it.
    registry.register(Box::new(
        MaterializeDocNode::new("learning-artifact")
            .with_source_node("LearningArtifactPayloadNode")
            .with_brain_root(root),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(http_post)
            .with_url(TEST_BRAIN_URL)
            .with_harvest(gate),
    ));
    registry.register(Box::new(
        ActionDispatchNode::new().with_transport(Arc::new(StubChannelTransport::succeeding())),
    ));
    registry
}

fn web_article_event(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "web_article",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "url", "url": "https://example.com/a" },
        },
        "translate": false,
    })
}

async fn run_at(
    root: &Path,
    gate: HarvestGate,
    http_post: Arc<dyn HttpPost>,
    event: Value,
) -> TaskContext {
    Workflow::new_validated(registry_at(root, gate, http_post), graph::schema())
        .expect("declared CONTENT_PIPELINE graph should validate")
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("CONTENT_PIPELINE run should complete")
}

/// The single non-index path `MaterializeDocNode` stamped.
fn written_path(ctx: &TaskContext) -> std::path::PathBuf {
    let result = &ctx.nodes["MaterializeDocNode"];
    let paths = result["paths"].as_array().expect("paths array");
    let doc = paths
        .iter()
        .filter_map(Value::as_str)
        .find(|p| !p.ends_with("index.md"))
        .expect("a non-index document path");
    std::path::PathBuf::from(doc)
}

/// Run `HARVEST_APPROVE` over `pending` with the given `HttpPost` seam.
async fn run_harvest_approve(pending: Value, http_post: Arc<dyn HttpPost>) -> TaskContext {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        HarvestApproveNode::new().with_http_post(http_post),
    ));

    Workflow::new_validated(registry, harvest_approve::graph::schema())
        .expect("declared HARVEST_APPROVE graph should validate")
        .run(pending, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("HARVEST_APPROVE run should complete")
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_process_mode_posts_exactly_once_and_writes_the_doc() {
    let tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(tmp.path());
    let post = CountingHttpPost::succeeding(json!({"ok": true}));

    let ctx = run_at(
        tmp.path(),
        HarvestGate::new(HarvestMode::InProcess),
        Arc::new(post.clone()),
        web_article_event("env-harvest-gate-1"),
    )
    .await;

    assert_eq!(post.call_count(), 1, "expected exactly one POST");
    let (url, _body) = post.last_call().expect("post should have been recorded");
    assert_eq!(url, TEST_BRAIN_URL);

    let result = &ctx.nodes["PersistToBrainNode"];
    assert_eq!(result["posted"], json!(true));
    assert_eq!(result["skipped"], json!(false));
    assert_eq!(result["harvest_mode"], json!("in_process"));
    assert_eq!(result["pending"], Value::Null);

    let path = written_path(&ctx);
    assert!(path.exists(), "expected {path:?} to exist");
    assert!(path.starts_with(tmp.path()));
}

#[tokio::test]
async fn off_mode_default_and_explicit_land_the_same_way_with_zero_posts() {
    // Baseline: `in_process` writes the doc content this test compares
    // against, and its POST body is reused in the hand-off test.
    let in_process_tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(in_process_tmp.path());
    let in_process_post = CountingHttpPost::succeeding(json!({"ok": true}));
    let in_process_ctx = run_at(
        in_process_tmp.path(),
        HarvestGate::new(HarvestMode::InProcess),
        Arc::new(in_process_post.clone()),
        web_article_event("env-harvest-gate-off"),
    )
    .await;
    let baseline_bytes = std::fs::read_to_string(written_path(&in_process_ctx)).expect("readable");

    // Explicit `HarvestMode::Off`.
    let explicit_tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(explicit_tmp.path());
    let explicit_post = CountingHttpPost::succeeding(json!({"ok": true}));
    let explicit_ctx = run_at(
        explicit_tmp.path(),
        HarvestGate::new(HarvestMode::Off),
        Arc::new(explicit_post.clone()),
        web_article_event("env-harvest-gate-off"),
    )
    .await;

    // Resolved default: `HarvestGate::default()` with no explicit mode
    // set — the built-in default a `policy`/`profile`-free run resolves to.
    let default_tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(default_tmp.path());
    let default_post = CountingHttpPost::succeeding(json!({"ok": true}));
    let default_ctx = run_at(
        default_tmp.path(),
        HarvestGate::default(),
        Arc::new(default_post.clone()),
        web_article_event("env-harvest-gate-off"),
    )
    .await;

    for (label, post, ctx, root) in [
        ("explicit off", &explicit_post, &explicit_ctx, &explicit_tmp),
        (
            "resolved default",
            &default_post,
            &default_ctx,
            &default_tmp,
        ),
    ] {
        assert_eq!(post.call_count(), 0, "{label}: expected zero POSTs");
        assert_eq!(
            ctx.node_runs["ActionDispatchNode"].status,
            NodeRunStatus::Success,
            "{label}: run should still succeed"
        );

        let result = &ctx.nodes["PersistToBrainNode"];
        assert_eq!(result["posted"], json!(false), "{label}");
        assert_eq!(result["skipped"], json!(true), "{label}",);
        assert_eq!(result["harvest_mode"], json!("off"), "{label}");

        let path = written_path(ctx);
        assert!(path.starts_with(root.path()), "{label}");
        let bytes = std::fs::read_to_string(&path).expect("readable");
        assert_eq!(
            bytes, baseline_bytes,
            "{label}: harvest mode must not affect the materialized .md"
        );
    }
}

#[tokio::test]
async fn approval_mode_defers_and_stamps_the_written_doc_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(tmp.path());
    let post = CountingHttpPost::succeeding(json!({"ok": true}));

    let ctx = run_at(
        tmp.path(),
        HarvestGate::new(HarvestMode::Approval),
        Arc::new(post.clone()),
        web_article_event("env-harvest-gate-approval"),
    )
    .await;

    assert_eq!(
        post.call_count(),
        0,
        "approval must not POST during the run"
    );

    let result = &ctx.nodes["PersistToBrainNode"];
    assert_eq!(result["posted"], json!(false));
    assert_eq!(result["skipped"], json!(true));
    assert_eq!(result["harvest_mode"], json!("approval"));

    let path = written_path(&ctx);
    let bytes = std::fs::read_to_string(&path).expect("readable");

    let pending = &result["pending"];
    assert!(!pending.is_null(), "expected a stamped pending record");
    assert_eq!(pending["url"], json!(TEST_BRAIN_URL));
    assert!(pending["payload"].is_object());
    let doc_paths = pending["doc_paths"]
        .as_array()
        .expect("doc_paths is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        doc_paths.iter().any(|p| Path::new(p) == path),
        "pending doc_paths {doc_paths:?} should include the actually written path {path:?}"
    );

    assert!(path.exists(), "the doc must be written even when deferred");
    assert_eq!(
        std::fs::read_to_string(&path).expect("readable"),
        bytes,
        "sanity: no further write happened after stamping the pending record"
    );
}

#[tokio::test]
async fn the_approval_hand_off_posts_a_payload_byte_identical_to_in_process() {
    let event = web_article_event("env-harvest-gate-hand-off");

    // Case 1: in_process, POSTs immediately.
    let in_process_tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(in_process_tmp.path());
    let in_process_post = CountingHttpPost::succeeding(json!({"ok": true}));
    run_at(
        in_process_tmp.path(),
        HarvestGate::new(HarvestMode::InProcess),
        Arc::new(in_process_post.clone()),
        event.clone(),
    )
    .await;
    let (in_process_url, in_process_body) = in_process_post
        .last_call()
        .expect("in_process should have POSTed");

    // Case 3: approval, defers and stamps a pending record.
    let approval_tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(approval_tmp.path());
    let approval_post = CountingHttpPost::succeeding(json!({"ok": true}));
    let approval_ctx = run_at(
        approval_tmp.path(),
        HarvestGate::new(HarvestMode::Approval),
        Arc::new(approval_post.clone()),
        event,
    )
    .await;
    let pending = approval_ctx.nodes["PersistToBrainNode"]["pending"].clone();
    assert!(!pending.is_null());

    // Case 4: feed case 3's pending record verbatim into HARVEST_APPROVE.
    let hand_off_post = CountingHttpPost::succeeding(json!({"ok": true}));
    let hand_off_ctx = run_harvest_approve(pending, Arc::new(hand_off_post.clone())).await;

    assert_eq!(hand_off_post.call_count(), 1);
    let (hand_off_url, hand_off_body) = hand_off_post
        .last_call()
        .expect("HARVEST_APPROVE should have POSTed");

    assert_eq!(
        hand_off_url, in_process_url,
        "the deferred push must target the same URL an in_process push would have"
    );
    assert_eq!(
        hand_off_body, in_process_body,
        "the deferred push must be byte-identical to what in_process would have sent"
    );

    let result = &hand_off_ctx.nodes["HarvestApproveNode"];
    assert_eq!(result["approved"], json!(true));
    assert_eq!(result["posted"], json!(true));
}

#[tokio::test]
async fn all_three_harvest_modes_stamp_an_identical_persist_to_brain_key_set() {
    use std::collections::BTreeSet;

    let mut key_sets = Vec::new();
    for mode in [
        HarvestMode::Off,
        HarvestMode::InProcess,
        HarvestMode::Approval,
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        learning_corpus_dir(tmp.path());
        let post = CountingHttpPost::succeeding(json!({"ok": true}));

        let ctx = run_at(
            tmp.path(),
            HarvestGate::new(mode),
            Arc::new(post),
            web_article_event(&format!("env-harvest-gate-keys-{mode:?}")),
        )
        .await;

        let object = ctx.nodes["PersistToBrainNode"]
            .as_object()
            .expect("result is an object");
        key_sets.push((mode, object.keys().cloned().collect::<BTreeSet<_>>()));
    }

    let expected: BTreeSet<String> = [
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

    for (mode, keys) in &key_sets {
        assert_eq!(keys, &expected, "mode {mode:?} stamped a different key set");
    }
}

#[tokio::test]
async fn an_approved_harvest_whose_post_fails_surfaces_as_a_failed_run_not_a_silent_drop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(tmp.path());
    let deferring_post = CountingHttpPost::succeeding(json!({"ok": true}));

    let ctx = run_at(
        tmp.path(),
        HarvestGate::new(HarvestMode::Approval),
        Arc::new(deferring_post),
        web_article_event("env-harvest-gate-fail"),
    )
    .await;
    let pending = ctx.nodes["PersistToBrainNode"]["pending"].clone();
    assert!(!pending.is_null());

    let failing_post = CountingHttpPost::failing("brain endpoint unreachable");
    let hand_off_ctx = run_harvest_approve(pending, Arc::new(failing_post.clone())).await;

    assert_eq!(
        failing_post.call_count(),
        1,
        "the push must actually have been attempted"
    );

    let node_run = &hand_off_ctx.node_runs["HarvestApproveNode"];
    assert_eq!(
        node_run.status,
        NodeRunStatus::Failed,
        "a failed approval push must surface as a failed node run, not a silent success"
    );
    assert!(
        node_run
            .error
            .as_deref()
            .is_some_and(|e| e.contains("brain endpoint unreachable")),
        "unexpected error: {:?}",
        node_run.error
    );
    assert!(
        !hand_off_ctx.nodes.contains_key("HarvestApproveNode"),
        "a failed node must not stamp a success result"
    );
}
