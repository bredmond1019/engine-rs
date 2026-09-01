//! Hermetic end-to-end integration suite for `EN.6.E` task 5: drives the
//! real, assembled `RESEARCH_AGENT` graph (`research_agent::graph::schema`)
//! all the way through its new terminal node, `ResearchIngressDispatchNode`,
//! with a stubbed `ChannelTransport` — never a real `claude` subprocess, a
//! real corpus write, or a real network call.
//!
//! Mirrors `research_agent_e2e.rs`'s node-by-node walk (`Node::process`
//! directly against the same registered instances a real `Workflow` would
//! use, following `WorkflowSchema::next_after` at each hop) rather than
//! `Workflow::run`'s own walk loop, for the same reason that file gives:
//! `RESEARCH_AGENT` has no dedicated setup node, so there is no controlled
//! place to pre-stamp a temp-dir worktree path before the graph starts.
//! This file extends that walk two hops further — through the shared
//! `MergeContactsNode` (`EN.4.E`) and the new `ResearchIngressDispatchNode`
//! (`EN.6.E`) — since task 5's acceptance criteria live entirely on that new
//! terminal identity.
//!
//! Covers the spec's seven task-5 scenarios: (1) default-off dispatches
//! nothing; (2) `enabled: true` dispatches exactly one `TriggerWorkflow`
//! action whose event round-trips as `ContentPipelineInput`; (3)
//! `chain_depth` is propagated, not reset; (4) a chain at `MAX_CHAIN_DEPTH`
//! is refused by `WorkflowTriggerDispatch` (no HTTP call), leaving the run
//! successful; (5) a failing transport leaves the run successful with a
//! `delivered: false` receipt; (6) identical input dispatched twice yields
//! an identical `envelope_id`; (7) the `baseline` profile dispatches
//! nothing and `thorough` dispatches once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::envelope::{ChannelType, SourcePayload};
use engine_contract::TaskContext;
use engine_core::node::NodeRegistry;
use engine_core::nodes::channel_transport::{
    ChannelTransport, OutboundBody, StubChannelTransport, WorkflowTriggerDispatch,
};
use engine_core::nodes::doc_materializer::{
    MaterializeOutcome, MaterializedFile, StubDocMaterializer,
};
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::nodes::materialize_doc::MaterializeDocNode;
use engine_core::nodes::merge_contacts::MergeContactsNode;
use engine_core::policy;
use engine_core::workflows::content_pipeline::schema::ContentPipelineInput;
use engine_core::workflows::research_agent::graph::{self, ResearchModeRouterNode};
use engine_core::workflows::research_agent::ingress_dispatch::ResearchIngressDispatchNode;
use engine_core::workflows::research_agent::profiles;
use engine_core::workflows::research_agent::prospecting::ProspectingResearchNode;
use engine_core::workflows::research_agent::CompanyResearchNode;
use futures::FutureExt;
use serde_json::json;

/// A `TriggerWorkflow` chain at or beyond this many hops is refused by
/// `WorkflowTriggerDispatch` — mirrors `channel_transport.rs`'s private
/// `MAX_CHAIN_DEPTH` constant (kept as a literal here, same as that
/// module's own tests do, since the const isn't exported).
const MAX_CHAIN_DEPTH: u64 = 8;

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-research-ingress-dispatch-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    dir
}

fn stub_outcome(structured: serde_json::Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.02,
        usage: claude_code_rs::parse::Usage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        model_usage: [(
            "claude-sonnet-4-5".to_string(),
            claude_code_rs::parse::ModelUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                cost_usd: 0.02,
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

fn stub_company_transport() -> engine_core::workflows::ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        let brief = json!({
            "company_name": "Acme Corp",
            "summary": "Widget manufacturer expanding into SaaS.",
            "recent_developments": ["Raised Series B"],
            "pain_points": ["Manual invoicing"],
            "outreach_hooks": ["Recent Series B raise"],
            "sources": ["https://acme.example/news"],
            "contacts": [],
        });
        async move { Ok(stub_outcome(brief, 100, 50)) }.boxed()
    })
}

/// Builds a `MaterializeDocNode` wired with a `StubDocMaterializer` that
/// always succeeds, so this hermetic suite never touches a real corpus.
fn stub_materialize_doc_node() -> MaterializeDocNode {
    let outcome = MaterializeOutcome {
        wrote: true,
        planned: vec![MaterializedFile {
            path: PathBuf::from("/tmp/brain/business/docs/opportunities/acme-corp.md"),
            note: "created".to_string(),
        }],
        diagnostics: vec![],
    };
    MaterializeDocNode::new("opportunity")
        .with_materializer(Arc::new(StubDocMaterializer::succeeding(outcome)))
        .with_brain_root("/tmp/brain")
        .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"])
}

/// Builds a fresh registry with every `RESEARCH_AGENT` node identity
/// registered: the company-mode research branch on a stubbed transport, a
/// stubbed `MaterializeDocNode`, a `MergeContactsNode` reading the same
/// source-node preference (its contacts list is always empty for the stub
/// company brief above, so it always takes the no-seam-call no-op path —
/// no brain root is ever resolved), and `ResearchIngressDispatchNode` wired
/// to `transport`.
fn build_registry(transport: Arc<dyn ChannelTransport>) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ResearchModeRouterNode));
    registry.register(Box::new(
        CompanyResearchNode::new().with_transport(stub_company_transport()),
    ));
    registry.register(Box::new(ProspectingResearchNode::new()));
    registry.register(Box::new(stub_materialize_doc_node()));
    registry.register(Box::new(
        MergeContactsNode::new()
            .with_source_nodes(["CompanyResearchNode", "ProspectingResearchNode"]),
    ));
    registry.register(Box::new(
        ResearchIngressDispatchNode::new().with_transport(transport),
    ));
    registry
}

fn set_worktree(ctx: &mut TaskContext, worktree: &Path) {
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
}

/// Resolve `ctx.event`'s policy and stamp it under `RESOLVED_POLICY_IDENTITY`
/// — the same seeding `engine-serve`'s dispatch factory performs before a
/// real node ever sees the `ctx` (mirrors `research_agent_e2e.rs`'s helper
/// of the same name).
fn stamp_resolved_policy(ctx: &mut TaskContext, worktree: &Path) {
    let resolved = profiles::resolve_policy_for_run(ctx, worktree)
        .expect("RESEARCH_AGENT policy should resolve for this event");
    policy::stamp_resolved_policy(ctx, &resolved).expect("policy should stamp");
}

/// Drives the full logical `RESEARCH_AGENT` walk, company mode only, through
/// every hop of the declared graph: `ResearchModeRouterNode::route` ->
/// `CompanyResearchNode` -> `MaterializeDocNode` -> `MergeContactsNode` ->
/// `ResearchIngressDispatchNode`. Each hop's `Node::process` runs for real
/// against the exact instances [`build_registry`] registered; only the
/// terminal `ChannelTransport` and the upstream model/materializer seams are
/// stubbed. Stamps each hop's `NodeRunStatus::Success` transition the same
/// way `Workflow::run`'s walk loop would, mirroring `research_agent_e2e.rs`.
async fn drive(
    event: serde_json::Value,
    worktree: &Path,
    transport: Arc<dyn ChannelTransport>,
) -> TaskContext {
    let registry = build_registry(transport);

    let mut ctx = TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    set_worktree(&mut ctx, worktree);
    stamp_resolved_policy(&mut ctx, worktree);

    let schema = graph::schema();
    let mut identity = "ResearchModeRouterNode".to_string();
    loop {
        let node = registry
            .get(&identity)
            .unwrap_or_else(|| panic!("'{identity}' should be registered"));

        if let Some(router) = node.as_router() {
            identity = router
                .route(&ctx)
                .unwrap_or_else(|| panic!("'{identity}' should resolve a routing target"));
            continue;
        }

        ctx = node
            .process(ctx)
            .await
            .unwrap_or_else(|err| panic!("'{identity}' should process successfully: {err}"));
        if let Some(run) = ctx.node_runs.get_mut(&identity) {
            run.status = engine_contract::NodeRunStatus::Success;
            run.completed_at = Some(chrono::Utc::now());
        }

        match schema.next_after(&identity) {
            Some(next) => identity = next.to_string(),
            None => break,
        }
    }

    ctx
}

fn enabled_event(mode_extra: serde_json::Value) -> serde_json::Value {
    let mut event = json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "company_url": "https://acme.example",
        "policy": { "ingress_dispatch": { "enabled": true } },
    });
    for (k, v) in mode_extra.as_object().cloned().unwrap_or_default() {
        event[k] = v;
    }
    event
}

// --- (1) default policy dispatches nothing ---------------------------------

#[tokio::test]
async fn default_policy_dispatches_nothing() {
    let worktree = temp_worktree("default-off");
    let stub = Arc::new(StubChannelTransport::succeeding());

    let event = json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "company_url": "https://acme.example",
    });

    let ctx = drive(event, &worktree, stub.clone()).await;

    assert!(
        stub.calls().is_empty(),
        "default-off policy should send nothing"
    );
    let stored = ctx
        .nodes
        .get("ResearchIngressDispatchNode")
        .expect("ResearchIngressDispatchNode should have stamped a result");
    assert_eq!(stored["skipped"], json!(true));
    assert_eq!(stored["enabled"], json!(false));

    std::fs::remove_dir_all(&worktree).ok();
}

// --- (2) enabled dispatches exactly one TriggerWorkflow --------------------

#[tokio::test]
async fn enabled_policy_dispatches_exactly_one_trigger_workflow_action() {
    let worktree = temp_worktree("enabled");
    let stub = Arc::new(StubChannelTransport::succeeding());

    let event = enabled_event(json!({}));
    let ctx = drive(event, &worktree, stub.clone()).await;

    let calls = stub.calls();
    assert_eq!(calls.len(), 1, "exactly one action should have been sent");
    let action = &calls[0];
    assert_eq!(action.channel_type, ChannelType::WorkflowTrigger);

    match &action.body {
        OutboundBody::TriggerWorkflow {
            workflow_type,
            event,
        } => {
            assert_eq!(workflow_type, "CONTENT_PIPELINE");
            let input: ContentPipelineInput = serde_json::from_value(event.clone())
                .expect("event should deserialize as ContentPipelineInput");
            assert_eq!(input.envelope.channel_type, ChannelType::ResearchAgent);
            match input.envelope.source {
                SourcePayload::TaskContextRef {
                    workflow_type,
                    inline,
                    ..
                } => {
                    assert_eq!(workflow_type, "RESEARCH_AGENT");
                    let inline = inline.expect("inline research output should be present");
                    assert_eq!(inline["company_name"], json!("Acme Corp"));
                }
                other => panic!("expected TaskContextRef, got {other:?}"),
            }
        }
        other => panic!("expected a TriggerWorkflow body, got {other:?}"),
    }

    let stored = ctx
        .nodes
        .get("ResearchIngressDispatchNode")
        .expect("ResearchIngressDispatchNode should have stamped a result");
    assert_eq!(stored["skipped"], json!(false));
    assert_eq!(stored["enabled"], json!(true));

    std::fs::remove_dir_all(&worktree).ok();
}

// --- (3) chain_depth is propagated, not reset -------------------------------

#[tokio::test]
async fn chain_depth_is_propagated_from_the_parent_event() {
    let worktree = temp_worktree("chain-depth");
    let stub = Arc::new(StubChannelTransport::succeeding());

    let event = enabled_event(json!({ "chain_depth": 3 }));
    drive(event, &worktree, stub.clone()).await;

    let action = stub.last_call().expect("one call recorded");
    match action.body {
        OutboundBody::TriggerWorkflow { event, .. } => {
            assert_eq!(event["chain_depth"], json!(3));
        }
        other => panic!("expected TriggerWorkflow, got {other:?}"),
    }

    std::fs::remove_dir_all(&worktree).ok();
}

// --- (4) a chain at MAX_CHAIN_DEPTH is refused, run still succeeds ---------

#[tokio::test]
async fn a_chain_at_the_depth_cap_is_refused_by_the_transport_but_the_run_still_succeeds() {
    let worktree = temp_worktree("cap-refusal");
    let http_stub = Arc::new(StubHttpPost::succeeding(json!({ "ok": true })));
    let trigger_dispatch: Arc<dyn ChannelTransport> = Arc::new(
        WorkflowTriggerDispatch::new("http://localhost:8080/events/")
            .with_http_post(http_stub.clone()),
    );

    let event = enabled_event(json!({ "chain_depth": MAX_CHAIN_DEPTH }));
    let ctx = drive(event, &worktree, trigger_dispatch).await;

    assert!(
        http_stub.last_call().is_none(),
        "a chain at the depth cap should never reach the HTTP seam"
    );

    let stored = ctx
        .nodes
        .get("ResearchIngressDispatchNode")
        .expect("ResearchIngressDispatchNode should have stamped a result");
    assert_eq!(stored["receipt"]["delivered"], json!(false));

    std::fs::remove_dir_all(&worktree).ok();
}

// --- (5) a failing transport leaves the run successful ---------------------

#[tokio::test]
async fn a_failing_transport_leaves_the_run_successful_with_delivered_false() {
    let worktree = temp_worktree("failing-transport");
    let stub = Arc::new(StubChannelTransport::failing());

    let event = enabled_event(json!({}));
    let ctx = drive(event, &worktree, stub.clone()).await;

    assert_eq!(stub.calls().len(), 1);
    let stored = ctx
        .nodes
        .get("ResearchIngressDispatchNode")
        .expect("ResearchIngressDispatchNode should have stamped a result");
    assert_eq!(stored["receipt"]["delivered"], json!(false));

    std::fs::remove_dir_all(&worktree).ok();
}

// --- (6) identical input dispatched twice yields identical envelope_id -----

#[tokio::test]
async fn identical_input_dispatched_twice_yields_the_same_envelope_id() {
    let worktree1 = temp_worktree("determinism-1");
    let worktree2 = temp_worktree("determinism-2");
    let stub1 = Arc::new(StubChannelTransport::succeeding());
    let stub2 = Arc::new(StubChannelTransport::succeeding());

    let event = enabled_event(json!({}));
    let ctx1 = drive(event.clone(), &worktree1, stub1).await;
    let ctx2 = drive(event, &worktree2, stub2).await;

    let id1 = ctx1.nodes["ResearchIngressDispatchNode"]["envelope_id"].clone();
    let id2 = ctx2.nodes["ResearchIngressDispatchNode"]["envelope_id"].clone();
    assert_eq!(id1, id2);
    assert!(!id1.as_str().unwrap_or_default().is_empty());

    std::fs::remove_dir_all(&worktree1).ok();
    std::fs::remove_dir_all(&worktree2).ok();
}

// --- (7) profile parity: baseline off, thorough on --------------------------

#[tokio::test]
async fn baseline_profile_dispatches_nothing() {
    let worktree = temp_worktree("profile-baseline");
    let stub = Arc::new(StubChannelTransport::succeeding());

    let event = json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "profile": "baseline",
    });
    drive(event, &worktree, stub.clone()).await;

    assert!(
        stub.calls().is_empty(),
        "the baseline profile should dispatch nothing"
    );

    std::fs::remove_dir_all(&worktree).ok();
}

#[tokio::test]
async fn thorough_profile_dispatches_once() {
    let worktree = temp_worktree("profile-thorough");
    let stub = Arc::new(StubChannelTransport::succeeding());

    let event = json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "profile": "thorough",
    });
    drive(event, &worktree, stub.clone()).await;

    assert_eq!(
        stub.calls().len(),
        1,
        "the thorough profile should dispatch exactly once"
    );

    std::fs::remove_dir_all(&worktree).ok();
}
