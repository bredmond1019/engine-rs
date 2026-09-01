//! Hermetic end-to-end integration test for the `CONTENT_PIPELINE` egress
//! seam (`EN.6.A` task 6): drives the real declared graph — including the
//! terminal `PersistToBrainNode -> ActionDispatchNode` edge — through the
//! real `Workflow::run` pointer-walk loop, asserting the full outbound
//! `ChannelTransport`/`WorkflowTriggerDispatch` contract per
//! `planning/EN.5.A-content-pipeline/architecture.md` §7.2 and the
//! 2026-07-25 tasks.md review additions.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned
//! (every model node is built `with_transport(..)` a canned stub), no live
//! network fetch happens (`FetchArticleNode` is built `with_fetch(..)` a
//! stub), no live network call happens for the Brain write
//! (`PersistToBrainNode` is built `with_http_post(..)` a `StubHttpPost`),
//! and egress never touches a real channel or a real `/events/` endpoint —
//! `ActionDispatchNode` is built `with_transport(..)` either a
//! `StubChannelTransport` or a `WorkflowTriggerDispatch`/`UnwiredChannelTransport`
//! composed purely over a `StubHttpPost`.
//!
//! Covers:
//! (a) reply path — a Slack envelope with `reply_context` yields exactly one
//!     recorded `OutboundAction` whose `channel_type`/`reply_context` match
//!     the envelope and whose `OutboundBody::Digest` markdown equals the
//!     rendered digest;
//! (b) fire-and-forget — a `web_article` envelope with no `reply_context`
//!     sends nothing, and the run still succeeds;
//! (c) trigger chaining — a `trigger` request dispatches a `TriggerWorkflow`
//!     action through `WorkflowTriggerDispatch` over a `StubHttpPost`;
//!     `last_call` asserts the `/events/` URL and the `{workflow_type,
//!     data}` payload (`data` carries this run's `envelope_id`), and
//!     `last_headers` asserts the `X-API-Key` header the endpoint requires;
//! (d) chain depth cap — an event whose `chain_depth` is at or beyond the
//!     cap is refused rather than posted, so an A -> B -> A trigger cycle
//!     terminates instead of recursing;
//! (e) uncredentialed email adapter — an Email-typed reply against the live
//!     composition (`channel_transport_live` over a stubbed `HttpPost`)
//!     routes to the real `EmailChannelTransport`, which yields a
//!     `delivered=false` receipt reporting its own missing-credential
//!     error (not the generic unwired-channel error) without failing the
//!     run;
//! (f) send-failure resilience — a failing `StubChannelTransport` still
//!     lets the run succeed, recording a `delivered=false` receipt;
//! (g) `EventsRow` round-trip — the final `TaskContext` (including the
//!     dispatch receipts on `ctx.nodes["ActionDispatchNode"]`) survives a
//!     serialize/deserialize round trip;
//! (h) Local profile rewire — `registry_for_policy` under an all-Local
//!     policy leaves `ActionDispatchNode` registered and untouched.
//!
//! **Documented deviation from the tasks.md review note's "parent_run_id"
//! wording:** `engine_contract::TaskContext` carries no `run_id` field at
//! all (see `crates/engine-contract/src/task_context.rs`), and
//! `architecture.md` §7.2 — the section's own source of truth — only
//! requires the child event to carry this run's `envelope_id`, not a
//! separate `parent_run_id`. `envelope_id` *is* this run's correlation key
//! (stamped by `SourceRouterNode`), so the child-event assertions below
//! check for it under that name; there is no separate `parent_run_id` field
//! anywhere in the codebase to assert on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::channel_transport::{
    channel_transport_live, ChannelTransport, StubChannelTransport, WorkflowTriggerDispatch,
};
use engine_core::nodes::doc_materializer::{MaterializeOutcome, StubDocMaterializer};
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::nodes::materialize_doc::MaterializeDocNode;
use engine_core::workflow::Workflow;
use engine_core::workflows::content_pipeline::action_dispatch::{ActionDispatchNode, NODE_NAME};
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
use engine_core::workflows::content_pipeline::revise::ReviseNode;
use engine_core::workflows::content_pipeline::self_critic::SelfCriticNode;
use engine_core::workflows::content_pipeline::source_router::SourceRouterNode;
use engine_core::workflows::content_pipeline::summarize::SummarizeNode;
use engine_core::workflows::content_pipeline::translate::{TranslateNode, TranslateSkipRouterNode};
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/action-dispatch-e2e";
const TEST_EVENTS_URL: &str = "http://localhost:8080/events/";

// ---------------------------------------------------------------------------
// Stub helpers (mirrors content_pipeline_e2e.rs's conventions)
// ---------------------------------------------------------------------------

fn stub_outcome(structured: Value) -> Outcome {
    Outcome {
        cost_usd: 0.01,
        usage: SdkUsage {
            input_tokens: 40,
            output_tokens: 20,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            SdkModelUsage {
                input_tokens: 40,
                output_tokens: 20,
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
        "summary": "A concise summary of the content.",
        "entities": ["Acme Corp"],
        "key_points": ["Point one", "Point two"],
    })
}

fn critic_pass_json() -> Value {
    json!({ "verdict": "pass", "confidence": 0.95, "issues": [] })
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

fn slack_reply_context() -> Value {
    json!({
        "thread_id": "t-1",
        "conversation_id": null,
        "channel_token": "c-1",
    })
}

fn slack_event_with_reply(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "slack",
            "sender_id": "U123",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "channel_message", "text": "hello from the channel", "attachments": [] },
            "reply_context": slack_reply_context(),
        },
    })
}

fn email_event_with_reply(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "email",
            "sender_id": "someone@example.com",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "channel_message", "text": "hello via email", "attachments": [] },
            "reply_context": { "thread_id": null, "conversation_id": "conv-1", "channel_token": "mailbox-1" },
        },
    })
}

fn web_article_event(envelope_id: &str) -> Value {
    json!({
        "envelope": {
            "envelope_id": envelope_id,
            "channel_type": "web_article",
            "timestamp": "2026-07-25T00:00:00Z",
            "source": { "kind": "url", "url": "https://example.com/a" },
        },
    })
}

fn web_article_event_with_trigger(envelope_id: &str, chain_depth: Option<u64>) -> Value {
    let mut event = web_article_event(envelope_id);
    let mut trigger_event = json!({ "note": "chain onward" });
    if let Some(depth) = chain_depth {
        trigger_event["chain_depth"] = json!(depth);
    }
    event["trigger"] = json!({
        "workflow_type": "CONTENT_PIPELINE",
        "event": trigger_event,
    });
    event
}

// ---------------------------------------------------------------------------
// Registry construction
// ---------------------------------------------------------------------------

struct Stubs {
    article_fetch: Arc<dyn ArticleFetch>,
    transcript_fetch: Arc<dyn TranscriptFetch>,
    summarize: ModelTransport,
    critic: ModelTransport,
    revise: ModelTransport,
    translate: ModelTransport,
    http_post: StubHttpPost,
}

impl Stubs {
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
            critic: stub_transport_returning(critic_pass_json()),
            revise: stub_transport_returning(revised_summary_json()),
            translate: stub_transport_returning(translated_json()),
            http_post: StubHttpPost::succeeding(json!({"ok": true})),
        }
    }
}

/// Build a fresh registry with every `CONTENT_PIPELINE` node identity
/// registered — every node the same stubbed way `content_pipeline_e2e.rs`
/// does, except `ActionDispatchNode`, which is wired to `transport` so each
/// test can inject the exact `ChannelTransport` it wants to assert on.
fn build_registry(stubs: &Stubs, transport: Arc<dyn ChannelTransport>) -> NodeRegistry {
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
    // `EN.7.D`'s materialize tail. This suite is about egress, so the seam
    // is stubbed and the brain root pinned — nothing here touches disk.
    registry.register(Box::new(LearningArtifactPayloadNode));
    registry.register(Box::new(
        MaterializeDocNode::new("learning-artifact")
            .with_source_node("LearningArtifactPayloadNode")
            .with_materializer(Arc::new(StubDocMaterializer::succeeding(
                MaterializeOutcome::default(),
            )))
            .with_brain_root("/tmp/brain"),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(stubs.http_post.clone()))
            .with_url(TEST_BRAIN_URL),
    ));
    registry.register(Box::new(
        ActionDispatchNode::new().with_transport(transport),
    ));
    registry
}

fn build_workflow(stubs: &Stubs, transport: Arc<dyn ChannelTransport>) -> Workflow {
    Workflow::new_validated(build_registry(stubs, transport), graph::schema())
        .expect("declared CONTENT_PIPELINE graph should validate")
}

async fn drive(workflow: &Workflow, event: Value) -> TaskContext {
    workflow
        .run(event, Box::new(|_ctx: &TaskContext| {}))
        .await
        .expect("CONTENT_PIPELINE run should complete (structural errors only surface as Err)")
}

fn dispatched_of(ctx: &TaskContext) -> Vec<Value> {
    ctx.nodes[NODE_NAME]["dispatched"]
        .as_array()
        .expect("ActionDispatchNode should store a `dispatched` array")
        .clone()
}

mod nn {
    pub const ACTION_DISPATCH: &str = "ActionDispatchNode";
    pub const PERSIST_TO_BRAIN: &str = "PersistToBrainNode";
    pub const DIGEST_RENDER: &str = "DigestRenderNode";
}

// ---------------------------------------------------------------------------
// (a) reply path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reply_context_present_sends_exactly_one_digest_action_matching_the_rendered_digest() {
    let stubs = Stubs::default_passing();
    let stub_transport = Arc::new(StubChannelTransport::succeeding());
    let workflow = build_workflow(&stubs, stub_transport.clone());

    let ctx = drive(&workflow, slack_event_with_reply("env-reply-1")).await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success
    );

    let calls = stub_transport.calls();
    assert_eq!(calls.len(), 1, "exactly one reply action should be sent");
    let action = &calls[0];
    assert_eq!(
        action.channel_type,
        engine_contract::envelope::ChannelType::Slack
    );
    let reply_context = action
        .reply_context
        .as_ref()
        .expect("reply_context should be present");
    assert_eq!(reply_context.thread_id.as_deref(), Some("t-1"));
    assert_eq!(reply_context.channel_token.as_deref(), Some("c-1"));

    let rendered_digest = ctx.nodes[nn::DIGEST_RENDER]["digest_markdown"]
        .as_str()
        .expect("DigestRenderNode should have stored digest_markdown")
        .to_string();
    match &action.body {
        engine_core::nodes::channel_transport::OutboundBody::Digest { markdown, .. } => {
            assert_eq!(markdown, &rendered_digest);
        }
        other => panic!("expected a Digest body, got {other:?}"),
    }

    let dispatched = dispatched_of(&ctx);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0]["receipt"]["delivered"], json!(true));
    assert_eq!(dispatched[0]["envelope_id"], json!("env-reply-1"));
}

// ---------------------------------------------------------------------------
// (b) fire-and-forget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_reply_context_sends_nothing_and_the_run_still_succeeds() {
    let stubs = Stubs::default_passing();
    let stub_transport = Arc::new(StubChannelTransport::succeeding());
    let workflow = build_workflow(&stubs, stub_transport.clone());

    let ctx = drive(&workflow, web_article_event("env-fire-forget-1")).await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs[nn::PERSIST_TO_BRAIN].status,
        NodeRunStatus::Success
    );
    assert!(
        stub_transport.calls().is_empty(),
        "no reply_context and no trigger => zero sends"
    );
    assert_eq!(dispatched_of(&ctx), Vec::<Value>::new());
}

// ---------------------------------------------------------------------------
// (c) trigger chaining: URL, payload, X-API-Key header
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trigger_request_dispatches_to_events_url_with_payload_and_api_key_header() {
    let stubs = Stubs::default_passing();
    let http_stub = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
    let trigger_transport: Arc<dyn ChannelTransport> = Arc::new(
        WorkflowTriggerDispatch::new(TEST_EVENTS_URL)
            .with_http_post(http_stub.clone())
            .with_api_key("super-secret"),
    );
    let workflow = build_workflow(&stubs, trigger_transport);

    let ctx = drive(
        &workflow,
        web_article_event_with_trigger("env-trigger-1", None),
    )
    .await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success
    );

    let (url, body) = http_stub
        .last_call()
        .expect("WorkflowTriggerDispatch should have posted to /events/");
    assert_eq!(url, TEST_EVENTS_URL);
    assert_eq!(body["workflow_type"], json!("CONTENT_PIPELINE"));
    assert_eq!(body["data"]["envelope_id"], json!("env-trigger-1"));
    assert_eq!(body["data"]["note"], json!("chain onward"));
    assert_eq!(body["data"]["chain_depth"], json!(1));

    let headers = http_stub
        .last_headers()
        .expect("post_with_headers should have been used for the trigger POST");
    assert!(
        headers.contains(&("X-API-Key".to_string(), "super-secret".to_string())),
        "headers were: {headers:?}"
    );

    let dispatched = dispatched_of(&ctx);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0]["receipt"]["delivered"], json!(true));
    assert_eq!(dispatched[0]["envelope_id"], json!("env-trigger-1"));
}

// ---------------------------------------------------------------------------
// (d) chain depth cap: refused rather than recursing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_chain_at_the_depth_cap_is_refused_rather_than_posted() {
    // MAX_CHAIN_DEPTH is a private constant of channel_transport.rs; use a
    // value comfortably at/above any reasonable cap so this test doesn't
    // need to import it. The dispatch failure is recorded as a
    // `delivered=false` receipt, not a run failure — the assertion that
    // matters is "never reached the HTTP seam", i.e. never recursed.
    const AT_OR_BEYOND_ANY_REASONABLE_CAP: u64 = 100;

    let stubs = Stubs::default_passing();
    let http_stub = Arc::new(StubHttpPost::succeeding(json!({"ok": true})));
    let trigger_transport: Arc<dyn ChannelTransport> =
        Arc::new(WorkflowTriggerDispatch::new(TEST_EVENTS_URL).with_http_post(http_stub.clone()));
    let workflow = build_workflow(&stubs, trigger_transport);

    let ctx = drive(
        &workflow,
        web_article_event_with_trigger("env-chain-cap-1", Some(AT_OR_BEYOND_ANY_REASONABLE_CAP)),
    )
    .await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success,
        "a refused chain must not fail the run"
    );
    assert!(
        http_stub.last_call().is_none(),
        "a chain at/beyond the cap should never reach the HTTP seam (no A -> B -> A recursion)"
    );

    let dispatched = dispatched_of(&ctx);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0]["receipt"]["delivered"], json!(false));
}

// ---------------------------------------------------------------------------
// (e) wired-but-uncredentialed email channel: delivered=false without
//     failing the run, and the detail comes from the real email adapter
//     (not the generic "no ChannelTransport adapter wired" unwired path) —
//     `EN.6.B` wires `ChannelType::Email` to a real `EmailChannelTransport`,
//     so this no longer exercises `UnwiredChannelTransport` at all.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unwired_email_channel_yields_delivered_false_naming_the_owning_block_without_failing_the_run(
) {
    let stubs = Stubs::default_passing();
    // The live composition: TriggerWorkflow -> WorkflowTriggerDispatch,
    // Email -> the real EmailChannelTransport, every other channel ->
    // UnwiredChannelTransport. With RESEND_API_KEY unset in the test
    // environment, the email adapter refuses to send and reports its own
    // credential error rather than reaching the network — this stays
    // hermetic (the live transport's HttpPost is never reached for a
    // non-trigger action).
    let live_transport = channel_transport_live(TEST_EVENTS_URL);
    let workflow = build_workflow(&stubs, live_transport);

    let ctx = drive(&workflow, email_event_with_reply("env-unwired-1")).await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success,
        "a send failure on the email adapter must not fail the run"
    );

    let dispatched = dispatched_of(&ctx);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0]["receipt"]["delivered"], json!(false));
    let detail = dispatched[0]["receipt"]["detail"]
        .as_str()
        .expect("detail should be a string");
    assert!(
        !detail.contains("no ChannelTransport adapter wired"),
        "email must not route to UnwiredChannelTransport; detail was: {detail}"
    );
    assert!(
        detail.contains("RESEND_API_KEY"),
        "expected the email adapter's own credential error, got: {detail}"
    );
}

// ---------------------------------------------------------------------------
// (f) send-failure resilience
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failing_transport_records_a_delivered_false_receipt_and_the_run_still_succeeds() {
    let stubs = Stubs::default_passing();
    let stub_transport = Arc::new(StubChannelTransport::failing());
    let workflow = build_workflow(&stubs, stub_transport.clone());

    let ctx = drive(&workflow, slack_event_with_reply("env-failing-1")).await;

    assert_eq!(
        ctx.node_runs[nn::ACTION_DISPATCH].status,
        NodeRunStatus::Success
    );
    let dispatched = dispatched_of(&ctx);
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0]["receipt"]["delivered"], json!(false));
    assert_eq!(
        dispatched[0]["receipt"]["detail"],
        json!("stub configured to fail")
    );
}

// ---------------------------------------------------------------------------
// (g) EventsRow round-trip, including dispatch receipts
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
async fn final_task_context_including_dispatch_receipts_round_trips_to_an_events_row() {
    let stubs = Stubs::default_passing();
    let stub_transport = Arc::new(StubChannelTransport::succeeding());
    let workflow = build_workflow(&stubs, stub_transport);

    let event = slack_event_with_reply("env-eventsrow-1");
    let ctx = drive(&workflow, event.clone()).await;

    // Sanity: the receipts are actually present before round-tripping, so
    // this test would fail loudly if `ActionDispatchNode` stopped storing
    // them rather than silently passing on an empty payload.
    assert_eq!(dispatched_of(&ctx).len(), 1);

    let row = events_row_for(graph::WORKFLOW_TYPE, event, &ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");

    assert_eq!(round_tripped, row);
    assert_eq!(
        round_tripped.task_context.nodes[nn::ACTION_DISPATCH],
        ctx.nodes[nn::ACTION_DISPATCH]
    );
}

// ---------------------------------------------------------------------------
// (h) Local profile leaves ActionDispatchNode untouched
// ---------------------------------------------------------------------------

#[test]
fn local_profile_rewire_leaves_action_dispatch_node_registered_and_untouched() {
    let mut policy = ContentPipelinePolicy::default();
    policy.model_tiers.summarize = ModelTier::Local;
    policy.model_tiers.critic = ModelTier::Local;
    policy.model_tiers.revise = ModelTier::Local;
    policy.model_tiers.translate = ModelTier::Local;

    let default_registry = graph::registry();
    let policy_registry = graph::registry_for_policy(&policy);

    assert_eq!(policy_registry.len(), default_registry.len());
    assert!(policy_registry.contains(nn::ACTION_DISPATCH));
}

// ---------------------------------------------------------------------------
// Sanity: default-succeeding stub count guard so a future edit that starts
// double-sending a reply is caught immediately.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reply_and_trigger_both_present_sends_exactly_two_actions() {
    let call_count = Arc::new(AtomicUsize::new(0));
    struct CountingTransport {
        inner: StubChannelTransport,
        count: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ChannelTransport for CountingTransport {
        async fn send(
            &self,
            action: &engine_core::nodes::channel_transport::OutboundAction,
        ) -> Result<engine_core::nodes::channel_transport::ChannelSendReceipt, String> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.inner.send(action).await
        }
    }

    let stubs = Stubs::default_passing();
    let transport = Arc::new(CountingTransport {
        inner: StubChannelTransport::succeeding(),
        count: call_count.clone(),
    });
    let workflow = build_workflow(&stubs, transport);

    let mut event = slack_event_with_reply("env-both-1");
    event["trigger"] = json!({
        "workflow_type": "CONTENT_PIPELINE",
        "event": {},
    });

    let ctx = drive(&workflow, event).await;

    assert_eq!(call_count.load(Ordering::SeqCst), 2);
    assert_eq!(dispatched_of(&ctx).len(), 2);
}
