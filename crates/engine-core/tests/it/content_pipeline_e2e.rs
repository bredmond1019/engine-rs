//! Hermetic end-to-end integration test for the assembled `CONTENT_PIPELINE`
//! workflow (`EN.5.A` task 13): drives the real declared graph —
//! `SourceRouterNode -> { FetchArticleNode | FetchTranscriptNode |
//! NormalizeChannelContentNode } -> SummarizeNode -> SelfCriticNode ->
//! CriticRouterNode -> { TranslateSkipRouterNode -> { TranslateNode |
//! DigestRenderNode } | IncrementCriticIterationNode -> ReviseNode ->
//! SelfCriticNode }` (back-edge) `-> ... -> DigestRenderNode ->
//! PersistToBrainNode` — through the **real** `Workflow::run` pointer-walk
//! loop, per the 2026-07-25 tasks.md review: unlike `proposal_generator_e2e.rs`
//! / `sdlc_flow_e2e.rs`, `CONTENT_PIPELINE` has no setup node that needs a
//! temp-dir worktree injected up front (`SourceRouterNode` resolves policy
//! against `PolicyConfigSource::Builtin` — a channel envelope has no repo
//! checkout), so this suite can and does cover the runner's router
//! back-edge handling end-to-end.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned
//! (every model node is built `with_transport(..)` a canned stub), no live
//! network fetch happens (`FetchArticleNode`/`FetchTranscriptNode` are built
//! `with_fetch(..)` a stub), and no live network call happens
//! (`PersistToBrainNode` is built `with_http_post(..)` a `StubHttpPost`).
//!
//! Covers the spec's acceptance criteria for task 13 (architecture.md
//! §7.1 + the 2026-07-25 tasks.md review additions):
//! (a) the `web_article`/`youtube_transcript`/`channel_message` branches
//!     each flow through to `PersistToBrainNode`, converging on the same
//!     `{title, text, source_ref}` content shape;
//! (b) two different `channel_type`s carrying the same payload kind take
//!     the same branch;
//! (c) the critic loop terminates by pass verdict, by the confidence
//!     threshold, and by `max_critic_iterations` (the cap test uses a
//!     transport that always returns `revise` and asserts exactly
//!     `max_critic_iterations` critic passes via the `iteration` field);
//! (d) an out-of-range `max_critic_iterations`/`critic_confidence_threshold`
//!     event-level policy override is rejected;
//! (e) translate-on and translate-off both render, with
//!     `translated_markdown` present only when `TranslateNode` ran;
//! (f) the same `envelope_id` run twice mints the same `artifact_id`
//!     (retried-webhook idempotency);
//! (g) `PersistToBrainNode`'s POST payload matches `StubHttpPost::last_call`;
//! (h) `registry_for_policy` rewires exactly the four Local-eligible stages,
//!     never fetch/normalize/render/persist;
//! (i) the final `TaskContext` round-trips to an `EventsRow`;
//! (j) `is_registered("CONTENT_PIPELINE")` is true after
//!     `register_builtin_workflows`;
//! (k) an `#[ignore]`-gated experiment harness drives the pipeline under
//!     each named profile, writes `content-pipeline-state.json` snapshots,
//!     and aggregates them into a ranked per-profile table.
//!
//! **Deviation from architecture.md §7.1 (documented, not a gap):** the
//! "unsupported channel" branch it describes (`SourceRouterNode` hard-failing
//! on `ChannelType::ResearchAgent` naming `EN.6.E`) was replaced by the
//! 2026-07-25 tasks.md review — `source_router.rs`'s module doc explains
//! `SourcePayload::TaskContextRef`/`WorkflowTrigger` now route to
//! `NormalizeChannelContentNode` like any other text-shaped source, closing
//! the graph under every channel Phase 6 adds. There is no error branch left
//! to exercise; [`task_context_ref_and_workflow_trigger_payloads_flow_through_normalize`]
//! covers what replaced it instead.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::channel_transport::StubChannelTransport;
use engine_core::nodes::doc_materializer::{
    MaterializeOutcome, MaterializedFile, StubDocMaterializer,
};
use engine_core::nodes::harvest_gate::{HarvestGate, HarvestMode};
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::nodes::materialize_doc::MaterializeDocNode;
use engine_core::policy::{self, PolicyConfigSource};
use engine_core::workflow::Workflow;
use engine_core::workflows::content_pipeline::action_dispatch::ActionDispatchNode;
use engine_core::workflows::content_pipeline::critic_router::CriticRouterNode;
use engine_core::workflows::content_pipeline::digest_render::DigestRenderNode;
use engine_core::workflows::content_pipeline::fetch_article::{
    ArticleFetch, FetchArticleNode, FetchedContent, StubArticleFetch,
};
use engine_core::workflows::content_pipeline::fetch_transcript::{
    FetchTranscriptNode, FetchedTranscript, StubTranscriptFetch, TranscriptFetch,
};
use engine_core::workflows::content_pipeline::graph;
use engine_core::workflows::content_pipeline::increment_critic_iteration::IncrementCriticIterationNode;
use engine_core::workflows::content_pipeline::learning_artifact::LearningArtifactPayloadNode;
use engine_core::workflows::content_pipeline::normalize_channel_content::NormalizeChannelContentNode;
use engine_core::workflows::content_pipeline::persist_to_brain::PersistToBrainNode;
use engine_core::workflows::content_pipeline::policy::{ContentPipelinePolicy, ModelTier};
use engine_core::workflows::content_pipeline::profiles;
use engine_core::workflows::content_pipeline::revise::ReviseNode;
use engine_core::workflows::content_pipeline::schema::ContentPipelineOutput;
use engine_core::workflows::content_pipeline::self_critic::SelfCriticNode;
use engine_core::workflows::content_pipeline::source_router::SourceRouterNode;
use engine_core::workflows::content_pipeline::summarize::SummarizeNode;
use engine_core::workflows::content_pipeline::translate::{TranslateNode, TranslateSkipRouterNode};
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/content-pipeline-e2e";

// ---------------------------------------------------------------------------
// Stub helpers
// ---------------------------------------------------------------------------

fn stub_outcome(structured: Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            SdkModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.01,
            },
        )]
        .into_iter()
        .collect(),
        text: serde_json::to_string(&structured).unwrap(),
        is_error: false,
        api_error_status: None,
        structured_output: Some(structured),
    }
}

/// Build a transport that always replies with `structured`, regardless of
/// the prompt/config it's handed.
fn stub_transport_returning(structured: Value) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that replies with each entry of `sequence` in turn,
/// repeating the last entry once exhausted — used to drive the critic loop
/// through a fixed sequence of verdicts across successive passes.
fn sequenced_transport(sequence: Vec<Value>) -> ModelTransport {
    let index = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_config: Config, _prompt: String| {
        let i = index.fetch_add(1, Ordering::SeqCst);
        let structured = sequence
            .get(i)
            .or_else(|| sequence.last())
            .cloned()
            .expect("sequence must be non-empty");
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that records the last `(Config, prompt)` it was called
/// with and always replies with `structured`.
fn capturing_transport(
    structured: Value,
    captured: Arc<Mutex<Option<(Config, String)>>>,
) -> ModelTransport {
    Arc::new(move |config: Config, prompt: String| {
        *captured.lock().unwrap() = Some((config.clone(), prompt.clone()));
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// Build a transport that increments `counter` on every call and always
/// replies with `structured` — used to assert an exact call count (e.g.
/// "`ReviseNode` ran exactly once", "the translate transport was never
/// called").
fn counting_transport(structured: Value, counter: Arc<AtomicUsize>) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        counter.fetch_add(1, Ordering::SeqCst);
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

fn summary_json() -> Value {
    json!({
        "summary": "A concise summary of the content.",
        "entities": ["Acme Corp"],
        "key_points": ["Point one", "Point two"],
    })
}

fn critic_json(verdict: &str, confidence: f64) -> Value {
    json!({
        "verdict": verdict,
        "confidence": confidence,
        "issues": if verdict == "revise" { vec!["needs more detail"] } else { vec![] },
    })
}

fn revised_summary_json() -> Value {
    json!({
        "summary": "A corrected, more accurate summary.",
        "entities": ["Acme Corp"],
        "key_points": ["Corrected point"],
    })
}

fn translated_json() -> Value {
    json!({ "translated_markdown": "# Resumo\n\nConteudo em portugues." })
}

// ---------------------------------------------------------------------------
// Event builders
// ---------------------------------------------------------------------------

fn web_article_event(envelope_id: &str, translate: bool) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "web_article",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "url", "url": "https://example.com/a" },
        },
        "translate": translate,
    })
}

fn youtube_transcript_event(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "youtube_transcript",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "video_id", "video_id": "abc123" },
        },
    })
}

fn channel_message_event(envelope_id: &str, channel_type: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": channel_type,
            "sender_id": "U123",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "channel_message", "text": "hello from the channel", "attachments": [] },
        },
    })
}

fn task_context_ref_event(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "research_agent",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": {
                "kind": "task_context_ref",
                "workflow_type": "RESEARCH_AGENT",
                "inline": "researched summary text",
            },
        },
    })
}

fn workflow_trigger_event(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "workflow_trigger",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": {
                "kind": "workflow_trigger",
                "workflow_type": "SDLC_FLOW",
                "event": { "task": "do-thing" },
            },
        },
    })
}

// ---------------------------------------------------------------------------
// Registry construction
// ---------------------------------------------------------------------------

/// Bundles the per-node stubs a [`build_registry`] call needs, so tests can
/// swap in a sequenced/capturing/counting stub for whichever node they want
/// to assert on.
struct Stubs {
    article_fetch: Arc<dyn ArticleFetch>,
    transcript_fetch: Arc<dyn TranscriptFetch>,
    summarize: ModelTransport,
    critic: ModelTransport,
    revise: ModelTransport,
    translate: ModelTransport,
    http_post: StubHttpPost,
    /// `EN.7.D`'s materialize seam. Stubbed here so this EN.5.A fixture
    /// stays disk-free; the real `MevDocMaterializer` against a tempdir
    /// corpus is exercised by `content_pipeline_materialize_e2e.rs`.
    materializer: StubDocMaterializer,
}

impl Stubs {
    /// Every stage passes/succeeds on the first attempt: critic emits a
    /// confident `pass`, so the loop never revises.
    fn default_passing() -> Self {
        Self {
            article_fetch: Arc::new(StubArticleFetch::succeeding(FetchedContent {
                title: Some("Example Title".to_string()),
                text: "Example article body about Acme Corp.".to_string(),
            })),
            transcript_fetch: Arc::new(StubTranscriptFetch::succeeding(FetchedTranscript {
                title: Some("A Talk".to_string()),
                text: "Transcript body about Acme Corp.".to_string(),
            })),
            summarize: stub_transport_returning(summary_json()),
            critic: stub_transport_returning(critic_json("pass", 0.95)),
            revise: stub_transport_returning(revised_summary_json()),
            translate: stub_transport_returning(translated_json()),
            http_post: StubHttpPost::succeeding(json!({"ok": true})),
            materializer: StubDocMaterializer::succeeding(MaterializeOutcome {
                wrote: true,
                planned: vec![MaterializedFile {
                    path: PathBuf::from("/tmp/brain/learning/artifact.md"),
                    note: "created".to_string(),
                }],
                diagnostics: vec![],
            }),
        }
    }
}

/// Build a fresh registry with every `CONTENT_PIPELINE` node identity
/// registered, each wired to the matching stub from `stubs` — the exact node
/// set [`graph::registry`] declares, over `graph::schema()`.
fn build_registry(stubs: &Stubs) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SourceRouterNode));
    registry.register(Box::new(
        FetchArticleNode::new().with_fetch(stubs.article_fetch.clone()),
    ));
    registry.register(Box::new(
        FetchTranscriptNode::new().with_fetch(stubs.transcript_fetch.clone()),
    ));
    registry.register(Box::new(NormalizeChannelContentNode));
    registry.register(Box::new(
        SummarizeNode::new().with_transport(stubs.summarize.clone()),
    ));
    registry.register(Box::new(
        SelfCriticNode::new().with_transport(stubs.critic.clone()),
    ));
    registry.register(Box::new(CriticRouterNode));
    registry.register(Box::new(IncrementCriticIterationNode));
    registry.register(Box::new(
        ReviseNode::new().with_transport(stubs.revise.clone()),
    ));
    registry.register(Box::new(TranslateSkipRouterNode));
    registry.register(Box::new(
        TranslateNode::new().with_transport(stubs.translate.clone()),
    ));
    registry.register(Box::new(DigestRenderNode));
    // `EN.7.D`'s materialize tail — the digest is written to the corpus
    // before the Synapse push. Stubbed seam, so this suite never touches
    // disk; a pinned brain root keeps `resolve_brain_root` out of the run.
    registry.register(Box::new(LearningArtifactPayloadNode));
    registry.register(Box::new(
        MaterializeDocNode::new("learning-artifact")
            .with_source_node("LearningArtifactPayloadNode")
            .with_materializer(Arc::new(stubs.materializer.clone()))
            .with_brain_root("/tmp/brain"),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(stubs.http_post.clone()))
            .with_url(TEST_BRAIN_URL)
            // `EN.7.C`: this fixture's assertions expect the pre-block
            // always-push behavior, so it opts explicitly into
            // `in_process` rather than relying on the new `off` default.
            .with_harvest(HarvestGate::new(HarvestMode::InProcess)),
    ));
    // `EN.6.A`'s egress terminal node — this EN.5.A fixture only needs the
    // graph to validate and run end-to-end; dispatch behavior itself is
    // exercised by `action_dispatch_e2e.rs` (EN.6.A task 6).
    registry.register(Box::new(
        ActionDispatchNode::new().with_transport(Arc::new(StubChannelTransport::succeeding())),
    ));
    registry
}

fn build_workflow(stubs: &Stubs) -> Workflow {
    Workflow::new_validated(build_registry(stubs), graph::schema())
        .expect("declared CONTENT_PIPELINE graph should validate")
}

/// Drive `workflow` through the real `Workflow::run` pointer-walk loop
/// (no `on_progress` persistence needed here) and return the final
/// `TaskContext`.
async fn drive(workflow: &Workflow, event: Value) -> TaskContext {
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("CONTENT_PIPELINE run should complete (structural errors only surface as Err)")
}

fn output_of(ctx: &TaskContext) -> ContentPipelineOutput {
    serde_json::from_value(ctx.nodes[graph_node_names::DIGEST_RENDER].clone())
        .expect("DigestRenderNode should have stored a valid ContentPipelineOutput")
}

/// Node identity string literals, named for readability at call sites
/// (mirrors each node module's own `NODE_NAME` constant).
mod graph_node_names {
    pub const SOURCE_ROUTER: &str = "SourceRouterNode";
    pub const FETCH_ARTICLE: &str = "FetchArticleNode";
    pub const FETCH_TRANSCRIPT: &str = "FetchTranscriptNode";
    pub const NORMALIZE: &str = "NormalizeChannelContentNode";
    pub const SELF_CRITIC: &str = "SelfCriticNode";
    pub const REVISE: &str = "ReviseNode";
    pub const TRANSLATE: &str = "TranslateNode";
    pub const DIGEST_RENDER: &str = "DigestRenderNode";
    pub const PERSIST_TO_BRAIN: &str = "PersistToBrainNode";
}
use graph_node_names as nn;

// ---------------------------------------------------------------------------
// Branch coverage: web_article / youtube_transcript / channel_message
// ---------------------------------------------------------------------------

#[tokio::test]
async fn web_article_branch_flows_through_to_persist() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-web-1", false)).await;

    assert_eq!(
        ctx.node_runs[nn::FETCH_ARTICLE].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[nn::FETCH_TRANSCRIPT].status,
        NodeRunStatus::Pending
    );
    assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Pending);
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );

    let output = output_of(&ctx);
    assert_eq!(output.summary, "A concise summary of the content.");
    assert!(output.digest_markdown.contains("A concise summary"));

    let (url, _body) = stubs
        .http_post
        .last_call()
        .expect("PersistToBrainNode should have POSTed");
    assert_eq!(url, TEST_BRAIN_URL);
}

#[tokio::test]
async fn youtube_transcript_branch_flows_through_to_persist() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, youtube_transcript_event("env-yt-1")).await;

    assert_eq!(
        ctx.node_runs[nn::FETCH_TRANSCRIPT].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[nn::FETCH_ARTICLE].status,
        NodeRunStatus::Pending
    );
    assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Pending);
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );

    let output = output_of(&ctx);
    assert_eq!(output.summary, "A concise summary of the content.");
}

#[tokio::test]
async fn channel_message_branch_normalizes_and_flows_through() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, channel_message_event("env-slack-1", "slack")).await;

    assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Success);
    assert_eq!(
        ctx.node_runs[nn::FETCH_ARTICLE].status,
        NodeRunStatus::Pending
    );
    assert_eq!(
        ctx.node_runs[nn::FETCH_TRANSCRIPT].status,
        NodeRunStatus::Pending
    );
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );

    // The normalized content shape converges on the same SummarizeNode
    // input every branch does.
    let output = output_of(&ctx);
    assert_eq!(output.summary, "A concise summary of the content.");
}

#[tokio::test]
async fn task_context_ref_and_workflow_trigger_payloads_flow_through_normalize() {
    // Deviation from architecture.md §7.1's "unsupported channel" test —
    // see this file's module doc comment. `TaskContextRef`/`WorkflowTrigger`
    // payloads route to `NormalizeChannelContentNode`, not an error branch.
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, task_context_ref_event("env-tcr-1")).await;
    assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Success);
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );

    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);
    let ctx = drive(&workflow, workflow_trigger_event("env-wt-1")).await;
    assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Success);
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );
}

#[tokio::test]
async fn different_channel_types_with_the_same_payload_kind_take_the_same_branch() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);
    let slack_ctx = drive(&workflow, channel_message_event("env-slack-2", "slack")).await;

    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);
    let telegram_ctx = drive(
        &workflow,
        channel_message_event("env-telegram-2", "telegram"),
    )
    .await;

    for ctx in [&slack_ctx, &telegram_ctx] {
        assert_eq!(ctx.node_runs[nn::NORMALIZE].status, NodeRunStatus::Success);
        assert_eq!(
            ctx.node_runs[nn::FETCH_ARTICLE].status,
            NodeRunStatus::Pending
        );
        assert_eq!(
            ctx.node_runs[nn::FETCH_TRANSCRIPT].status,
            NodeRunStatus::Pending
        );
    }
}

// ---------------------------------------------------------------------------
// Critic loop: pass exit, cap exit, confidence-threshold exit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn critic_loop_terminates_by_pass_verdict_after_one_revision() {
    let revise_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    stubs.critic = sequenced_transport(vec![critic_json("revise", 0.3), critic_json("pass", 0.9)]);
    stubs.revise = counting_transport(revised_summary_json(), revise_calls.clone());
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-loop-pass", false)).await;

    assert_eq!(revise_calls.load(Ordering::SeqCst), 1);
    let evaluation: engine_core::workflows::content_pipeline::schema::CriticEvaluation =
        serde_json::from_value(ctx.nodes[nn::SELF_CRITIC].clone()).expect("valid CriticEvaluation");
    assert_eq!(
        evaluation.verdict,
        engine_core::workflows::content_pipeline::schema::CriticVerdict::Pass
    );
    assert_eq!(evaluation.iteration, 1);
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );

    // Regression: the revised summary must actually propagate downstream —
    // the persisted digest reflects ReviseNode's corrected text, not the
    // original SummarizeNode draft the critic flagged.
    let output = output_of(&ctx);
    assert_eq!(output.summary, "A corrected, more accurate summary.");
    assert!(output
        .digest_markdown
        .contains("A corrected, more accurate summary."));
    assert!(!output
        .digest_markdown
        .contains("A concise summary of the content."));
}

#[tokio::test]
async fn critic_loop_terminates_by_max_critic_iterations_cap() {
    // Critic always returns `revise` with low confidence — only the cap can
    // end this loop. `max_critic_iterations = 2` must yield exactly 2
    // critic passes (architecture.md §4 / critic_router.rs's pinned
    // iteration semantics).
    let critic_calls = Arc::new(AtomicUsize::new(0));
    let revise_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    stubs.critic = counting_transport(critic_json("revise", 0.1), critic_calls.clone());
    stubs.revise = counting_transport(revised_summary_json(), revise_calls.clone());
    let workflow = build_workflow(&stubs);

    let mut event = web_article_event("env-loop-cap", false);
    event["policy"] = json!({ "max_critic_iterations": 2 });

    let ctx = drive(&workflow, event).await;

    assert_eq!(
        critic_calls.load(Ordering::SeqCst),
        2,
        "exactly max_critic_iterations critic passes"
    );
    assert_eq!(
        revise_calls.load(Ordering::SeqCst),
        1,
        "the back-edge fires once between the two critic passes, never a third time"
    );

    let evaluation: engine_core::workflows::content_pipeline::schema::CriticEvaluation =
        serde_json::from_value(ctx.nodes[nn::SELF_CRITIC].clone()).expect("valid CriticEvaluation");
    assert_eq!(evaluation.iteration, 1);
    assert_eq!(
        engine_core::workflows::content_pipeline::schema::CriticVerdict::Revise,
        evaluation.verdict
    );
    // The cap fired, not a pass verdict or the confidence threshold — the
    // loop still exits forward to persist rather than hanging.
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );
}

#[tokio::test]
async fn critic_loop_terminates_early_via_confidence_threshold_despite_revise_verdict() {
    let critic_calls = Arc::new(AtomicUsize::new(0));
    let revise_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    // "revise" verdict, but confidence (0.85) already meets the default
    // 0.8 threshold — the router must still exit on pass 1.
    stubs.critic = counting_transport(critic_json("revise", 0.85), critic_calls.clone());
    stubs.revise = counting_transport(revised_summary_json(), revise_calls.clone());
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-loop-confidence", false)).await;

    assert_eq!(critic_calls.load(Ordering::SeqCst), 1);
    assert_eq!(revise_calls.load(Ordering::SeqCst), 0);
    assert!(!ctx.nodes.contains_key(nn::REVISE));
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );
}

// ---------------------------------------------------------------------------
// Policy override validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn out_of_range_max_critic_iterations_event_override_is_rejected() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let mut event = web_article_event("env-bad-policy-1", false);
    event["policy"] = json!({ "max_critic_iterations": 999 });

    let ctx = drive(&workflow, event).await;

    let run = &ctx.node_runs[nn::SOURCE_ROUTER];
    assert_eq!(run.status, NodeRunStatus::Failed);
    assert!(run
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("max_critic_iterations"));
    // The run halted before any downstream node ran.
    assert_eq!(
        ctx.node_runs[nn::FETCH_ARTICLE].status,
        NodeRunStatus::Pending
    );
}

#[tokio::test]
async fn out_of_range_critic_confidence_threshold_event_override_is_rejected() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let mut event = web_article_event("env-bad-policy-2", false);
    event["policy"] = json!({ "critic_confidence_threshold": 1.5 });

    let ctx = drive(&workflow, event).await;

    let run = &ctx.node_runs[nn::SOURCE_ROUTER];
    assert_eq!(run.status, NodeRunStatus::Failed);
    assert!(run
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("critic_confidence_threshold"));
}

// ---------------------------------------------------------------------------
// Translate on/off
// ---------------------------------------------------------------------------

#[tokio::test]
async fn translate_on_carries_target_lang_and_populates_translated_markdown() {
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let mut stubs = Stubs::default_passing();
    stubs.translate = capturing_transport(translated_json(), captured.clone());
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-translate-on", true)).await;

    let (_config, prompt) = captured
        .lock()
        .unwrap()
        .take()
        .expect("translate transport called");
    assert!(prompt.contains("pt-BR"));

    let output = output_of(&ctx);
    assert_eq!(
        output.translated_markdown.as_deref(),
        Some("# Resumo\n\nConteudo em portugues.")
    );
    assert_eq!(ctx.node_runs[nn::TRANSLATE].status, NodeRunStatus::Success);
}

#[tokio::test]
async fn translate_off_never_invokes_translate_and_omits_translated_markdown() {
    let translate_calls = Arc::new(AtomicUsize::new(0));
    let mut stubs = Stubs::default_passing();
    stubs.translate = counting_transport(translated_json(), translate_calls.clone());
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-translate-off", false)).await;

    assert_eq!(translate_calls.load(Ordering::SeqCst), 0);
    assert!(!ctx.nodes.contains_key(nn::TRANSLATE));

    let output = output_of(&ctx);
    assert!(output.translated_markdown.is_none());
}

// ---------------------------------------------------------------------------
// Persist payload assertion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn persist_to_brain_posts_the_expected_learning_artifact_payload() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let ctx = drive(&workflow, web_article_event("env-persist-1", false)).await;

    let (url, body) = stubs
        .http_post
        .last_call()
        .expect("PersistToBrainNode should have POSTed");
    assert_eq!(url, TEST_BRAIN_URL);

    let object = body.as_object().expect("payload is an object");
    assert_eq!(
        object
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
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
    assert_eq!(body["channel_type"], json!("web_article"));
    assert_eq!(body["source_ref"], json!("https://example.com/a"));
    assert_eq!(body["summary"], json!("A concise summary of the content."));
    assert_eq!(body["language"], json!("en"));

    let output = output_of(&ctx);
    assert_eq!(body["artifact_id"], json!(output.artifact_id));
}

// ---------------------------------------------------------------------------
// Retried-webhook idempotency: same envelope_id -> same artifact_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_envelope_id_run_twice_mints_the_same_artifact_id() {
    let stubs_first = Stubs::default_passing();
    let workflow_first = build_workflow(&stubs_first);
    let ctx_first = drive(&workflow_first, web_article_event("env-retry-1", false)).await;

    let stubs_second = Stubs::default_passing();
    let workflow_second = build_workflow(&stubs_second);
    let ctx_second = drive(&workflow_second, web_article_event("env-retry-1", false)).await;

    let output_first = output_of(&ctx_first);
    let output_second = output_of(&ctx_second);
    assert_eq!(output_first.artifact_id, output_second.artifact_id);
    assert!(!output_first.artifact_id.is_empty());
}

// ---------------------------------------------------------------------------
// Local rewire: registry_for_policy touches exactly the four model stages
// ---------------------------------------------------------------------------

#[test]
fn registry_for_policy_rewires_exactly_the_four_model_stages_never_fetch_normalize_render_persist()
{
    let mut policy = ContentPipelinePolicy::default();
    policy.model_tiers.summarize = ModelTier::Local;
    policy.model_tiers.critic = ModelTier::Local;
    policy.model_tiers.revise = ModelTier::Local;
    policy.model_tiers.translate = ModelTier::Local;

    let default_registry = graph::registry();
    let policy_registry = graph::registry_for_policy(&policy);

    assert_eq!(policy_registry.len(), default_registry.len());
    for identity in [
        nn::SOURCE_ROUTER,
        nn::FETCH_ARTICLE,
        nn::FETCH_TRANSCRIPT,
        nn::NORMALIZE,
        "SummarizeNode",
        nn::SELF_CRITIC,
        "CriticRouterNode",
        "IncrementCriticIterationNode",
        nn::REVISE,
        "TranslateSkipRouterNode",
        nn::TRANSLATE,
        nn::DIGEST_RENDER,
        nn::PERSIST_TO_BRAIN,
    ] {
        assert!(
            policy_registry.contains(identity),
            "registry_for_policy should still register '{identity}'"
        );
    }
}

// ---------------------------------------------------------------------------
// EventsRow round-trip
// ---------------------------------------------------------------------------

fn events_row_for(workflow_type: &str, event: Value, ctx: &TaskContext) -> EventsRow {
    let now = Utc::now();
    EventsRow {
        id: uuid::Uuid::new_v4(),
        workflow_type: workflow_type.to_string(),
        data: event,
        task_context: ctx.clone(),
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test]
async fn final_task_context_round_trips_to_an_events_row() {
    let stubs = Stubs::default_passing();
    let workflow = build_workflow(&stubs);

    let event = web_article_event("env-eventsrow-1", false);
    let ctx = drive(&workflow, event.clone()).await;

    let row = events_row_for(graph::WORKFLOW_TYPE, event, &ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");

    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "CONTENT_PIPELINE");
    assert_eq!(round_tripped.task_context, ctx);
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

#[test]
fn is_registered_true_and_workflow_listed_after_register_builtin_workflows() {
    let mut dispatcher = engine_serve::dispatch::Dispatcher::new();
    engine_serve::workflows::register_builtin_workflows(&mut dispatcher);

    assert!(dispatcher.is_registered("CONTENT_PIPELINE"));

    let listed = dispatcher.registered_types();
    assert!(
        listed.iter().any(|t| t == "CONTENT_PIPELINE"),
        "expected 'CONTENT_PIPELINE' in the dispatcher's registered_types(), got {listed:?}"
    );

    let schema = dispatcher
        .resolve_schema("CONTENT_PIPELINE")
        .expect("CONTENT_PIPELINE schema should resolve");
    assert_eq!(schema.start_node, nn::SOURCE_ROUTER);
}

// ---------------------------------------------------------------------------
// `#[ignore]`-gated experiment harness: mirrors `proposal_generator_e2e.rs`'s
// shape. Runs each named profile end to end (hermetic — stubbed transports +
// `HttpPost`, per this workflow's own hermetic-by-construction design),
// writes each run's `content-pipeline-state.json`, and aggregates them into
// a ranked per-profile table via `crate::policy::aggregate_state_files`. Run
// manually:
//
// ```sh
// cargo test -p engine-core --test content_pipeline_e2e -- --ignored
// ```
// ---------------------------------------------------------------------------

const EXPERIMENT_PROFILES: [&str; 3] = ["baseline", "local-drafting", "fast-summarize"];

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-content-pipeline-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    dir
}

fn print_ranked_table(mut rows: Vec<policy::PolicyAggregate<ContentPipelinePolicy>>) {
    rows.sort_by(|a, b| {
        a.avg_cost_usd
            .partial_cmp(&b.avg_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "\n{:<8} {:>10} {:>12} {:>14} {:>14} {:>10}",
        "runs", "avg_cost", "avg_wall_s", "total_in_tok", "total_out_tok", "pass_rate"
    );
    for row in &rows {
        println!(
            "{:<8} {:>10.4} {:>12.2} {:>14} {:>14} {:>9.2}% {}",
            row.run_count,
            row.avg_cost_usd,
            row.avg_wall_clock_secs,
            row.total_input_tokens,
            row.total_output_tokens,
            row.pass_rate * 100.0,
            serde_json::to_string(&row.policy).unwrap_or_default(),
        );
    }
    println!();
}

fn extract_content_pipeline_run(
    value: &Value,
) -> Option<(ContentPipelinePolicy, policy::RunTelemetry)> {
    policy::extract_policy_telemetry::<ContentPipelinePolicy>(value, "policy", "telemetry")
}

#[tokio::test]
#[ignore]
async fn experiment_named_profiles_ranked_by_cost() {
    let mut state_paths: Vec<PathBuf> = Vec::new();
    let mut worktrees: Vec<PathBuf> = Vec::new();

    for profile in EXPERIMENT_PROFILES {
        let worktree = temp_worktree(profile);
        let stubs = Stubs::default_passing();
        let workflow = build_workflow(&stubs);

        let mut event = web_article_event(&format!("env-experiment-{profile}"), false);
        event["profile"] = json!(profile);

        // Sanity-check the named profile resolves before driving the run.
        let probe_ctx = TaskContext {
            event: event.clone(),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let resolved_policy =
            profiles::resolve_policy_for_run_from(&probe_ctx, &PolicyConfigSource::Builtin)
                .unwrap_or_else(|err| panic!("profile {profile:?} should resolve: {err}"));

        let final_ctx = drive(&workflow, event).await;

        let model_tier_used = BTreeMap::from([
            (
                "summarize".to_string(),
                format!("{:?}", resolved_policy.model_tiers.summarize),
            ),
            (
                "critic".to_string(),
                format!("{:?}", resolved_policy.model_tiers.critic),
            ),
            (
                "revise".to_string(),
                format!("{:?}", resolved_policy.model_tiers.revise),
            ),
            (
                "translate".to_string(),
                format!("{:?}", resolved_policy.model_tiers.translate),
            ),
        ]);
        let telemetry = policy::harvest_telemetry(
            &final_ctx,
            chrono::Utc::now(),
            policy::RunTelemetryInputs {
                start_node_identity: nn::SOURCE_ROUTER,
                verdict_stages: &[nn::SELF_CRITIC],
                cost_bearing_stages: &["SummarizeNode", nn::SELF_CRITIC, nn::REVISE, nn::TRANSLATE],
                total_attempts: 1,
                total_retries: 0,
                tasks_passed: 1,
                tasks_failed: 0,
                model_tier_used,
                model_stages: &["SummarizeNode", nn::SELF_CRITIC, nn::REVISE, nn::TRANSLATE],
            },
        );

        let state_path = worktree
            .join("planning")
            .join("content-pipeline-state.json");
        std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&json!({
                "policy": resolved_policy,
                "telemetry": telemetry,
            }))
            .expect("state serializes"),
        )
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", state_path.display()));

        state_paths.push(state_path);
        worktrees.push(worktree);
    }

    let rows = policy::aggregate_state_files::<ContentPipelinePolicy, _>(
        &state_paths,
        extract_content_pipeline_run,
    )
    .expect("all content-pipeline-state.json files should parse");

    print_ranked_table(rows.clone());

    assert!(
        !rows.is_empty(),
        "expected at least one aggregated policy row across the named profiles"
    );

    for worktree in &worktrees {
        std::fs::remove_dir_all(worktree).ok();
    }
}
