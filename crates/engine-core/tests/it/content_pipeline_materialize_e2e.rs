//! Hermetic end-to-end test of `CONTENT_PIPELINE`'s learning-artifact
//! materialization (`EN.7.D` task 7) — the real `Workflow::run` pointer-walk
//! loop with stubbed model transports and a stubbed `HttpPost`, but the
//! **real** `MevDocMaterializer` writing into a `tempfile::tempdir()`
//! corpus.
//!
//! This is the block's payoff test: it proves the generic
//! `MaterializeDocNode` — the same node, seam, and writer core
//! `RESEARCH_AGENT` uses for opportunities — materializes a second doc
//! shape with no core-writer change. Only the model string
//! (`"learning-artifact"`) and the source node differ.
//!
//! Follows `opportunity_loop_e2e.rs` for the tempdir-corpus pattern (the
//! brain root is passed explicitly via `MaterializeDocNode::with_brain_root`,
//! never through `ENGINE_BRAIN_ROOT`, so this suite is immune to any other
//! test's env-var mutation) and `content_pipeline_e2e.rs` for the
//! stub-transport registry shape.
//!
//! Every test is hermetic: no network, and no write anywhere outside its own
//! tempdir — in particular nothing under the real `agentic-portfolio` corpus.

use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::{NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::harvest_gate::{HarvestGate, HarvestMode};
use engine_core::nodes::http_post::StubHttpPost;
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
use engine_core::workflows::content_pipeline::policy::ContentPipelinePolicy;
use engine_core::workflows::content_pipeline::revise::ReviseNode;
use engine_core::workflows::content_pipeline::self_critic::SelfCriticNode;
use engine_core::workflows::content_pipeline::source_router::SourceRouterNode;
use engine_core::workflows::content_pipeline::summarize::SummarizeNode;
use engine_core::workflows::content_pipeline::translate::{TranslateNode, TranslateSkipRouterNode};
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use okf_core::{parse_nested_frontmatter, BrainDocModel, LearningArtifact};
use serde_json::{json, Value};

use engine_core::nodes::channel_transport::StubChannelTransport;

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/materialize-e2e";

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

/// The corpus directory `okf_core::LearningArtifact`'s `index_intent`
/// registers into, **derived from the model rather than hardcoded**.
///
/// `mev`'s materializer resolves its write target as
/// `root/dirname(index_path)/link_target`, so this directory is decided
/// entirely by okf-core's `LEARNING_CORPUS_INDEX`. Hardcoding the literal
/// here is what let a one-line const repoint in another repo break this
/// suite in four places at once, with a failure ("0 documents on disk")
/// that reads like a writer bug rather than a moved target — it cost a
/// misdiagnosis against `mev::write_atomic`. Derive it, and the suite
/// follows the model wherever it points.
fn learning_corpus_rel_dir() -> std::path::PathBuf {
    let artifact = okf_core::LearningArtifact::from_payload(&serde_json::json!({}));
    Path::new(&artifact.index_intent().index_path)
        .parent()
        .expect("index_path must have a parent directory component")
        .to_path_buf()
}

/// The same directory under `root`, pre-created so the first write has a
/// directory to land in (mirrors `opportunity_loop_e2e.rs`'s
/// `opportunities_dir`).
fn learning_corpus_dir(root: &Path) -> std::path::PathBuf {
    let dir = root.join(learning_corpus_rel_dir());
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Registry construction
// ---------------------------------------------------------------------------

/// Build a `CONTENT_PIPELINE` registry with stubbed model transports, fetch
/// seams, `HttpPost`, and channel transport — but the REAL
/// `MevDocMaterializer` (`MaterializeDocNode::new`'s default), pinned at
/// `root`, configured from `materialize` exactly as
/// `graph::registry_for_policy` does.
fn registry_at(root: &Path, policy: &ContentPipelinePolicy) -> NodeRegistry {
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
    // The real materializer — the point of this suite.
    registry.register(Box::new(
        MaterializeDocNode::new("learning-artifact")
            .with_source_node("LearningArtifactPayloadNode")
            .with_brain_root(root)
            .with_enabled(policy.materialize.enabled)
            .with_write(policy.materialize.write),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(StubHttpPost::succeeding(json!({"ok": true}))))
            .with_url(TEST_BRAIN_URL)
            // `EN.7.C`: this suite's assertions expect the pre-block
            // always-push behavior, so it opts explicitly into
            // `in_process` rather than relying on the new `off` default.
            .with_harvest(HarvestGate::new(HarvestMode::InProcess)),
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

async fn run_at(root: &Path, policy: &ContentPipelinePolicy, event: Value) -> TaskContext {
    Workflow::new_validated(registry_at(root, policy), graph::schema())
        .expect("declared CONTENT_PIPELINE graph should validate")
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("CONTENT_PIPELINE run should complete")
}

/// The single path `MaterializeDocNode` stamped, asserted to exist on disk.
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

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_content_run_materializes_a_valid_learning_artifact_document() {
    let tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(tmp.path());

    let ctx = run_at(
        tmp.path(),
        &ContentPipelinePolicy::default(),
        web_article_event("env-materialize-1"),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );

    let result = &ctx.nodes["MaterializeDocNode"];
    assert_eq!(result["materialized"], json!(true));
    assert_eq!(result["skipped"], json!(false));
    assert_eq!(result["dry_run"], json!(false));
    assert_eq!(result["model"], json!("learning-artifact"));

    let path = written_path(&ctx);
    assert!(path.exists(), "expected {path:?} to exist");
    assert!(
        path.starts_with(tmp.path()),
        "write escaped the tempdir corpus: {path:?}"
    );

    let content = std::fs::read_to_string(&path).expect("readable");
    assert!(content.contains("type: LearningArtifact"));

    let fields = parse_nested_frontmatter(&content).expect("must parse frontmatter");
    let artifact = LearningArtifact::from_frontmatter(&fields).expect("must reconstruct");
    assert!(!artifact.artifact_id.is_empty());
    assert_eq!(artifact.channel_type, "web_article");
    assert_eq!(artifact.source_ref, "https://example.com/a");
    assert_eq!(artifact.language, "en");
    assert!(!artifact.summary.is_empty());
    assert!(
        artifact.entities.contains(&"Acme Corp".to_string()),
        "entities should carry through from the summarize stage: {:?}",
        artifact.entities
    );

    // The digest body rides in a generated sentinel section, not frontmatter.
    assert!(content.contains("<!-- BEGIN generated:digest -->"));
    assert!(content.contains("<!-- END generated:digest -->"));
}

#[tokio::test]
async fn materializing_the_same_digest_twice_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = learning_corpus_dir(tmp.path());
    let policy = ContentPipelinePolicy::default();

    // Same envelope_id both runs — `artifact_id` is minted from it, so this
    // is the retried-webhook case that must not duplicate a document.
    let first = run_at(tmp.path(), &policy, web_article_event("env-idempotent")).await;
    let first_path = written_path(&first);
    let first_content = std::fs::read_to_string(&first_path).expect("readable");
    assert_eq!(
        first.nodes["MaterializeDocNode"]["materialized"],
        json!(true)
    );

    let second = run_at(tmp.path(), &policy, web_article_event("env-idempotent")).await;

    // The writer recognises the document is unchanged and plans NO write at
    // all — idempotency enforced at the planner, not by rewriting identical
    // bytes. `materialized: false` here is the up-to-date path, distinct
    // from the `skipped: true` policy-disabled path.
    let second_result = &second.nodes["MaterializeDocNode"];
    assert_eq!(second_result["materialized"], json!(false));
    assert_eq!(second_result["skipped"], json!(false));
    assert_eq!(
        second_result["paths"].as_array().map(Vec::len),
        Some(0),
        "an unchanged re-run should plan no file writes"
    );
    let warnings = second_result["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        warnings.contains("is already up to date"),
        "expected an up-to-date diagnostic, got: {warnings}"
    );

    assert_eq!(
        std::fs::read_to_string(&first_path).expect("readable"),
        first_content,
        "a re-run must leave the document byte-identical on disk"
    );

    let docs: Vec<_> = std::fs::read_dir(&corpus)
        .expect("corpus readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".md") && name != "index.md")
        .collect();
    assert_eq!(
        docs.len(),
        1,
        "re-running must not mint a second document: {docs:?}"
    );

    // ... and no duplicated index row for the same slug.
    let index_path = corpus.join("index.md");
    if index_path.exists() {
        let index = std::fs::read_to_string(&index_path).expect("readable");
        let slug = first_path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .to_string();
        assert_eq!(
            index.matches(&format!("{slug}.md")).count(),
            1,
            "index should carry exactly one row for '{slug}'"
        );
    }
}

#[tokio::test]
async fn materialize_disabled_writes_nothing_and_the_run_still_completes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = learning_corpus_dir(tmp.path());

    let mut policy = ContentPipelinePolicy::default();
    policy.materialize.enabled = false;

    let ctx = run_at(tmp.path(), &policy, web_article_event("env-disabled")).await;

    let result = &ctx.nodes["MaterializeDocNode"];
    assert_eq!(result["materialized"], json!(false));
    assert_eq!(result["skipped"], json!(true));

    let written: Vec<_> = std::fs::read_dir(&corpus)
        .expect("corpus readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        written.is_empty(),
        "materialize:off must not touch the corpus: {written:?}"
    );

    // The run still reaches the terminal egress node, and the Synapse push
    // still happened — disabling materialization changes nothing else.
    assert_eq!(
        ctx.node_runs["ActionDispatchNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(ctx.nodes["PersistToBrainNode"]["posted"], json!(true));
}

#[tokio::test]
async fn dry_run_plans_the_path_without_writing_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = learning_corpus_dir(tmp.path());

    let mut policy = ContentPipelinePolicy::default();
    policy.materialize.write = false;

    let ctx = run_at(tmp.path(), &policy, web_article_event("env-dry-run")).await;

    let result = &ctx.nodes["MaterializeDocNode"];
    assert_eq!(result["dry_run"], json!(true));
    assert_eq!(result["skipped"], json!(false));

    // The planned path is still named — that visibility is why the seam
    // plans before applying — but nothing landed on disk.
    let path = written_path(&ctx);
    assert!(!path.exists(), "dry run must not write {path:?}");
    let written: Vec<_> = std::fs::read_dir(&corpus)
        .expect("corpus readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(written.is_empty(), "dry run wrote files: {written:?}");
}

#[tokio::test]
async fn materialization_runs_before_the_synapse_push() {
    // The ordering claim, observed at run time rather than in the declared
    // schema: a failing Synapse push must not cost the run its document.
    let tmp = tempfile::tempdir().expect("tempdir");
    learning_corpus_dir(tmp.path());

    let mut registry = registry_at(tmp.path(), &ContentPipelinePolicy::default());
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(StubHttpPost::failing("brain unreachable")))
            .with_url(TEST_BRAIN_URL)
            // `EN.7.C`: an attempted push must still fail loudly, so this
            // exercises `in_process` explicitly rather than the new `off`
            // default (which would never attempt a push at all).
            .with_harvest(HarvestGate::new(HarvestMode::InProcess)),
    ));

    let workflow = Workflow::new_validated(registry, graph::schema())
        .expect("declared CONTENT_PIPELINE graph should validate");
    // A node failure halts the walk but is reported in the returned context
    // (only structural errors surface as `Err`) — same convention as
    // `content_pipeline_e2e.rs`.
    let ctx = workflow
        .run(
            web_article_event("env-push-fails"),
            Box::new(|_ctx: &TaskContext| {}),
        )
        .await
        .expect("run should return a context");

    let push = &ctx.node_runs["PersistToBrainNode"];
    assert_eq!(push.status, NodeRunStatus::Failed);
    assert!(
        push.error
            .as_deref()
            .is_some_and(|e| e.contains("brain ingest push failed")),
        "unexpected error: {:?}",
        push.error
    );
    // Materialization already succeeded, and the egress node never ran.
    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs["ActionDispatchNode"].status,
        NodeRunStatus::Pending
    );

    // The document survives the failed push.
    let docs: Vec<_> = std::fs::read_dir(tmp.path().join(learning_corpus_rel_dir()))
        .expect("corpus readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".md") && name != "index.md")
        .collect();
    assert_eq!(
        docs.len(),
        1,
        "the digest should already be on disk when the push fails: {docs:?}"
    );
}

#[tokio::test]
async fn a_first_ever_write_creates_the_missing_corpus_subtree() {
    // Regression: `mev::brain::emit::apply_plan` calls `std::fs::write`
    // directly and never creates directories, so a first-ever write into a
    // brain root with no learning-artifact corpus subtree used to fail with
    // `E_EMIT_WRITE_FAILED` and halt the run. The real corpus does NOT have
    // that directory yet, so this is the production path, not an edge case.
    //
    // Deliberately does NOT call `learning_corpus_dir` — the whole point is
    // that the target subtree is absent. Every other test here pre-creates
    // it (mirroring `opportunity_loop_e2e.rs`), which is exactly what
    // masked this.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(!tmp.path().join(learning_corpus_rel_dir()).exists());

    let ctx = run_at(
        tmp.path(),
        &ContentPipelinePolicy::default(),
        web_article_event("env-missing-subtree"),
    )
    .await;

    assert_eq!(
        ctx.node_runs["MaterializeDocNode"].status,
        NodeRunStatus::Success,
        "a missing corpus subtree must not fail the run"
    );
    assert_eq!(ctx.nodes["MaterializeDocNode"]["materialized"], json!(true));

    let path = written_path(&ctx);
    assert!(path.exists(), "expected {path:?} to have been created");
}

#[tokio::test]
async fn the_same_node_and_seam_serve_both_doc_kinds() {
    // The generality claim, asserted mechanically: constructing the writer
    // for the opportunity model and for the learning-artifact model differs
    // only by the model string — same type, same seam, same writer core.
    let tmp = tempfile::tempdir().expect("tempdir");

    for model in ["opportunity", "learning-artifact"] {
        let node = MaterializeDocNode::new(model).with_brain_root(tmp.path());
        assert_eq!(
            engine_core::node::Node::name(&node),
            "MaterializeDocNode",
            "both doc kinds run under the same node identity"
        );
    }
}
