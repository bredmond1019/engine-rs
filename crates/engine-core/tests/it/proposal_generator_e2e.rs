//! Hermetic end-to-end integration test for the assembled `PROPOSAL_GENERATOR`
//! workflow (`EN.4.C` task 11): drives the real declared seven-node chain —
//! `ProposalCompanyResearchNode -> OpportunityIdentifierNode ->
//! ProposalWriterNode -> ProposalReviewNode -> ProposalReviewRouterNode ->
//! { PersistToBrainNode | ProposalReviseNode -> PersistToBrainNode }` —
//! through **both** router branches, with stubbed `ModelTransport` +
//! `HttpPost` throughout, mirroring `research_agent_e2e.rs`'s driving style.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned
//! (every model node is built `with_transport(..)` a canned stub) and no
//! live network call ever happens (`PersistToBrainNode` is built
//! `with_http_post(..)` a `StubHttpPost`). Like `RESEARCH_AGENT`,
//! `PROPOSAL_GENERATOR` has no dedicated setup node, so `Workflow::run`
//! (which seeds `TaskContext` from `event` alone) cannot be handed a
//! controlled temp-dir worktree path up front — this file drives each
//! node's `Node::process` directly, in declared-graph order, against the
//! exact node instances [`build_registry`] would hand to a real `Workflow`,
//! and calls `ProposalReviewRouterNode::route` to pick the pass/revise
//! branch exactly as the walk loop would. [`assembled_workflow_builds`]
//! below separately confirms the declared graph + real-transport registry
//! still assemble via `Workflow::new_validated`.
//!
//! Covers the spec's acceptance criteria for task 11:
//! (a) a `pass` verdict routes `ProposalReviewRouterNode -> PersistToBrainNode`
//!     directly, and a `revise` verdict routes `-> ProposalReviseNode ->
//!     PersistToBrainNode`, both hermetic;
//! (b) the final `TaskContext` round-trips to an `EventsRow` (via
//!     `engine-contract`) without loss, for both branches;
//! (c) the composite/sort/≤3-profile validators each reject a malformed
//!     `AutomationRoadmap` and accept a well-formed one;
//! (d) `OpportunityIdentifierNode` scores from `event.diagnostic_intake`'s
//!     `*_evidence` fields when present, and falls back to the web brief
//!     when absent — both paths driven end-to-end;
//! (e) `PersistToBrainNode`'s POST payload
//!     `{artifact_id, company_name, doc_type, section, content, roadmap}`
//!     matches the `HttpPost` stub, for both branches;
//! (f) `registry_for_policy` rewires `{opportunity, review, revise}` under a
//!     Local profile but never `research`;
//! (g) `is_registered("PROPOSAL_GENERATOR")` is true after registration
//!     through `engine-serve`'s `Dispatcher`, and the workflow appears in
//!     `Dispatcher::registered_types()`;
//! (h) an `#[ignore]`-gated experiment harness drives the pipeline under
//!     each named profile, writes `proposal-generator-state.json` snapshots
//!     (carrying `review_verdict`/`revised`), and aggregates them into a
//!     ranked per-profile table via `crate::policy::aggregate_state_files`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::policy;
use engine_core::workflow::Workflow;
use engine_core::workflows::diagnostic_intake::schema::{DiagnosticIntake, WorkflowCandidate};
use engine_core::workflows::proposal_generator::company_research::ProposalCompanyResearchNode;
use engine_core::workflows::proposal_generator::graph;
use engine_core::workflows::proposal_generator::opportunity_identifier::OpportunityIdentifierNode;
use engine_core::workflows::proposal_generator::persist_to_brain::PersistToBrainNode;
use engine_core::workflows::proposal_generator::policy::{ModelTier, ProposalGeneratorPolicy};
use engine_core::workflows::proposal_generator::profiles;
use engine_core::workflows::proposal_generator::review::ProposalReviewNode;
use engine_core::workflows::proposal_generator::review_router::ProposalReviewRouterNode;
use engine_core::workflows::proposal_generator::revise::ProposalReviseNode;
use engine_core::workflows::proposal_generator::schema::{
    self, AutomationRoadmap, AutomationRoadmapValidationError,
};
use engine_core::workflows::proposal_generator::writer::ProposalWriterNode;
use engine_core::workflows::ModelTransport;
use futures::FutureExt;
use serde_json::{json, Value};
use uuid::Uuid;

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/proposal-generator-e2e";

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-proposal-generator-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    dir
}

fn set_worktree(ctx: &mut TaskContext, worktree: &Path) {
    ctx.nodes.insert(
        "SetupWorktreeNode".to_string(),
        json!({ "worktree_path": worktree.to_string_lossy() }),
    );
}

/// Resolve `ctx.event`'s policy (built-in + `harness.json` + named
/// `profile:` + inline `policy` override, high->low precedence) and stamp it
/// under `RESOLVED_POLICY_IDENTITY` — the same seeding `engine-serve`'s
/// dispatch factory performs before a real node ever sees the `ctx` (EN.5.D
/// task 8: nodes no longer re-resolve inside `process()`, so this hermetic
/// harness must replicate dispatch's resolve-once step itself).
fn stamp_resolved_policy(ctx: &mut TaskContext, worktree: &Path) {
    let resolved = profiles::resolve_policy_for_run(ctx, worktree)
        .expect("PROPOSAL_GENERATOR policy should resolve for this event");
    policy::stamp_resolved_policy(ctx, &resolved).expect("policy should stamp");
}

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

/// Build a transport that records the last `(Config, prompt)` it was
/// called with (into `captured`) and always replies with `structured`.
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

fn stub_company_brief_json() -> Value {
    json!({
        "company_name": "Loja da Ana",
        "summary": "Retail SMB tracking orders manually over WhatsApp.",
        "recent_developments": ["Opened a second storefront"],
        "pain_points": ["Manual order tracking"],
        "outreach_hooks": ["Recent second-storefront opening"],
        "sources": ["https://loja-da-ana.example/news"],
    })
}

fn stub_scored_candidates_json() -> Value {
    json!({
        "candidates": [
            {
                "name": "Supplier follow-up messages",
                "frequency": 3.0,
                "time_cost": 2.0,
                "buildability": 4.0,
                "rationale": "Frequent but lower time cost."
            },
            {
                "name": "WhatsApp order tracking",
                "frequency": 5.0,
                "time_cost": 4.0,
                "buildability": 4.0,
                "rationale": "Happens daily and is fully manual today."
            }
        ]
    })
}

fn stub_roadmap_json(painful_workflow_summary: &str) -> Value {
    json!({
        "situation": {
            "company_name": "Loja da Ana",
            "business_type": "retail SMB",
            "team_size": 4,
            "painful_workflow_summary": painful_workflow_summary,
            "candidate_count": 2,
        },
        "candidates": [
            {
                "name": "WhatsApp order tracking",
                "frequency": 5.0,
                "time_cost": 4.0,
                "buildability": 4.0,
                "composite": 4.35,
                "tier": "quick_win",
                "rationale": "Happens daily and is fully manual today.",
            },
            {
                "name": "Supplier follow-up messages",
                "frequency": 3.0,
                "time_cost": 2.0,
                "buildability": 4.0,
                "composite": 2.85,
                "tier": "core_build",
                "rationale": "Frequent but lower time cost.",
            }
        ],
        "top_profiles": [
            {
                "name": "WhatsApp order tracking",
                "today": "Manually scrolled.",
                "proposed_solution": "Automated bot with human approval gate.",
                "stack": "WhatsApp Business API + small service.",
                "rough_scope": "2-3 weeks.",
                "expected_roi": "Saves ~5 hrs/week.",
            }
        ],
        "recommendation": {
            "start_with": "WhatsApp order tracking",
            "phase_1_scope": ["Order intake bot"],
            "how_it_works": "Connects to WhatsApp Business API.",
            "call_to_action": "Book a call to proceed.",
        },
    })
}

fn stub_review_verdict_json(verdict: &str, notes: &str) -> Value {
    json!({ "verdict": verdict, "notes": notes })
}

fn base_event(diagnostic_intake: Option<DiagnosticIntake>) -> Value {
    json!({
        "company_name": "Loja da Ana",
        "company_url": "https://loja-da-ana.example",
        "diagnostic_intake": diagnostic_intake,
    })
}

fn diagnostic_intake_fixture() -> DiagnosticIntake {
    DiagnosticIntake {
        company_name: "Loja da Ana".to_string(),
        company_type: "retail SMB".to_string(),
        team_size: 4,
        primary_channels: vec!["WhatsApp".to_string()],
        existing_tools: vec!["Google Sheets".to_string()],
        existing_automations: vec![],
        top_workflows: vec![
            WorkflowCandidate {
                name: "WhatsApp order tracking".to_string(),
                description: "Orders tracked in chat.".to_string(),
                frequency_evidence: "\"Every day.\"".to_string(),
                time_cost_evidence: "\"An hour a day.\"".to_string(),
                buildability_notes: "API available.".to_string(),
                knowledge_holder: "Maria.".to_string(),
                failure_mode: "Orders lost.".to_string(),
            },
            WorkflowCandidate {
                name: "Supplier follow-up messages".to_string(),
                description: "Manual follow-up texts.".to_string(),
                frequency_evidence: "\"A few times a week.\"".to_string(),
                time_cost_evidence: "\"20 min each.\"".to_string(),
                buildability_notes: "Templated messages.".to_string(),
                knowledge_holder: "Carlos.".to_string(),
                failure_mode: "Late follow-up.".to_string(),
            },
        ],
    }
}

/// Bundles the per-node transports/stub `HttpPost` a [`build_registry`]
/// call needs, so tests can swap in a captured or verdict-specific
/// transport for whichever node they want to assert on.
struct Stubs {
    company_research: ModelTransport,
    opportunity: ModelTransport,
    writer: ModelTransport,
    review: ModelTransport,
    revise: ModelTransport,
    http_post: StubHttpPost,
}

impl Stubs {
    fn default_passing() -> Self {
        Self {
            company_research: stub_transport_returning(stub_company_brief_json()),
            opportunity: stub_transport_returning(stub_scored_candidates_json()),
            writer: stub_transport_returning(stub_roadmap_json(
                "Orders tracked by scrolling WhatsApp threads.",
            )),
            review: stub_transport_returning(stub_review_verdict_json("pass", "Looks good.")),
            revise: stub_transport_returning(stub_roadmap_json(
                "Corrected: orders tracked in a shared spreadsheet.",
            )),
            http_post: StubHttpPost::succeeding(json!({"ok": true})),
        }
    }

    fn revising() -> Self {
        Self {
            review: stub_transport_returning(stub_review_verdict_json(
                "revise",
                "Composite math is off for candidate 1.",
            )),
            ..Self::default_passing()
        }
    }
}

/// Build a fresh registry with every `PROPOSAL_GENERATOR` node identity
/// registered, each wired to the matching stub from `stubs`.
fn build_registry(stubs: &Stubs) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        ProposalCompanyResearchNode::new().with_transport(stubs.company_research.clone()),
    ));
    registry.register(Box::new(
        OpportunityIdentifierNode::new().with_transport(stubs.opportunity.clone()),
    ));
    registry.register(Box::new(
        ProposalWriterNode::new().with_transport(stubs.writer.clone()),
    ));
    registry.register(Box::new(
        ProposalReviewNode::new().with_transport(stubs.review.clone()),
    ));
    registry.register(Box::new(ProposalReviewRouterNode));
    registry.register(Box::new(
        ProposalReviseNode::new().with_transport(stubs.revise.clone()),
    ));
    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(stubs.http_post.clone()))
            .with_url(TEST_BRAIN_URL),
    ));
    registry
}

/// Stamp the framework-owned `NodeRunStatus::Success` transition a real
/// `Workflow::run` walk loop would have applied after a successful
/// `Node::process` call (`Node::process` alone never does this — see
/// `research_agent_e2e.rs`'s `drive` for the same rationale).
fn stamp_success(ctx: &mut TaskContext, identity: &str) {
    if let Some(run) = ctx.node_runs.get_mut(identity) {
        run.status = NodeRunStatus::Success;
        run.completed_at = Some(Utc::now());
    }
}

/// Drive the declared `PROPOSAL_GENERATOR` walk end to end against the
/// exact node instances [`build_registry`] would hand to a real `Workflow`:
/// the four linear stages in order, then `ProposalReviewRouterNode::route`
/// picks the branch, then either `PersistToBrainNode` directly (pass) or
/// `ProposalReviseNode -> PersistToBrainNode` (revise) — real composition,
/// real routing decision, real node logic, just not through
/// `Workflow::run`'s own walk loop (see module doc for why).
async fn drive(event: Value, worktree: &Path, registry: &NodeRegistry) -> (String, TaskContext) {
    let mut ctx = TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    set_worktree(&mut ctx, worktree);
    stamp_resolved_policy(&mut ctx, worktree);

    for identity in [
        "ProposalCompanyResearchNode",
        "OpportunityIdentifierNode",
        "ProposalWriterNode",
        "ProposalReviewNode",
    ] {
        let node = registry
            .get(identity)
            .unwrap_or_else(|| panic!("'{identity}' should be registered"));
        ctx = node
            .process(ctx)
            .await
            .unwrap_or_else(|err| panic!("'{identity}' should process successfully: {err}"));
        stamp_success(&mut ctx, identity);
    }

    let router = registry
        .get("ProposalReviewRouterNode")
        .expect("router should be registered");
    let branch = router
        .as_router()
        .expect("ProposalReviewRouterNode should be a Router")
        .route(&ctx)
        .expect("router should resolve a target identity for a valid verdict");

    if branch == "ProposalReviseNode" {
        let node = registry
            .get("ProposalReviseNode")
            .expect("ProposalReviseNode should be registered");
        ctx = node
            .process(ctx)
            .await
            .expect("ProposalReviseNode should process successfully");
        stamp_success(&mut ctx, "ProposalReviseNode");
    }

    let persist = registry
        .get("PersistToBrainNode")
        .expect("PersistToBrainNode should be registered");
    ctx = persist
        .process(ctx)
        .await
        .expect("PersistToBrainNode should process successfully");
    stamp_success(&mut ctx, "PersistToBrainNode");

    (branch, ctx)
}

/// Builds an [`EventsRow`] for a completed run the way the durable store
/// layer would: `data` is the run's inbound event, `task_context` is the
/// final `TaskContext`. Mirrors `research_agent_e2e.rs::events_row_for`.
fn events_row_for(workflow_type: &str, event: Value, ctx: &TaskContext) -> EventsRow {
    let now = Utc::now();
    EventsRow {
        id: Uuid::new_v4(),
        workflow_type: workflow_type.to_string(),
        data: event,
        task_context: ctx.clone(),
        created_at: now,
        updated_at: now,
    }
}

fn assert_events_row_round_trips(event: Value, ctx: &TaskContext) {
    let row = events_row_for(graph::WORKFLOW_TYPE, event, ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "PROPOSAL_GENERATOR");
    assert_eq!(round_tripped.task_context, *ctx);
}

fn persisted_roadmap(stub: &StubHttpPost) -> (String, Value) {
    let (url, body) = stub
        .last_call()
        .expect("PersistToBrainNode should have POSTed");
    let object = body.as_object().expect("payload is an object");
    assert_eq!(
        object
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "artifact_id",
            "company_name",
            "doc_type",
            "section",
            "content",
            "roadmap"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
    assert_eq!(body["company_name"], json!("Loja da Ana"));
    assert_eq!(body["doc_type"], json!("automation_roadmap"));
    assert_eq!(body["section"], json!("full"));
    (url, body["roadmap"].clone())
}

#[tokio::test]
async fn pass_verdict_routes_directly_to_persist_and_round_trips_events_row() {
    let worktree = temp_worktree("pass");
    let stubs = Stubs::default_passing();
    let registry = build_registry(&stubs);

    let event = base_event(None); // no diagnostic_intake: exercises the web-brief fallback.
    let (branch, final_ctx) = drive(event.clone(), &worktree, &registry).await;
    assert_eq!(branch, "PersistToBrainNode");

    // OpportunityIdentifierNode fell back to the web brief and scored real
    // candidates end-to-end.
    let opportunity_output = &final_ctx.nodes["OpportunityIdentifierNode"];
    let candidates = opportunity_output["candidates"]
        .as_array()
        .expect("candidates array");
    assert!(!candidates.is_empty());

    // ProposalReviseNode never ran on the pass branch.
    assert!(!final_ctx.nodes.contains_key("ProposalReviseNode"));

    // The writer's original draft is the one that reached the brain.
    let (url, roadmap_value) = persisted_roadmap(&stubs.http_post);
    assert_eq!(url, TEST_BRAIN_URL);
    let roadmap: AutomationRoadmap =
        serde_json::from_value(roadmap_value.clone()).expect("valid AutomationRoadmap");
    assert!(schema::validate_automation_roadmap(&roadmap).is_ok());
    assert_eq!(roadmap_value, final_ctx.nodes["ProposalWriterNode"]);

    let persist_result = &final_ctx.nodes["PersistToBrainNode"];
    assert_eq!(persist_result["posted"], json!(true));
    assert_eq!(persist_result["status"], json!(200));

    assert_events_row_round_trips(event, &final_ctx);

    std::fs::remove_dir_all(&worktree).ok();
}

#[tokio::test]
async fn revise_verdict_routes_through_revise_node_before_persist_and_round_trips_events_row() {
    let worktree = temp_worktree("revise");
    let stubs = Stubs::revising();
    let registry = build_registry(&stubs);

    // diagnostic_intake present: exercises the evidence-based scoring path.
    let event = base_event(Some(diagnostic_intake_fixture()));
    let (branch, final_ctx) = drive(event.clone(), &worktree, &registry).await;
    assert_eq!(branch, "ProposalReviseNode");

    let opportunity_output = &final_ctx.nodes["OpportunityIdentifierNode"];
    let candidates = opportunity_output["candidates"]
        .as_array()
        .expect("candidates array");
    assert!(!candidates.is_empty());

    // ProposalReviseNode did run on the revise branch, and its corrected
    // roadmap — not the writer's original draft — is the one that reached
    // the brain.
    assert!(final_ctx.nodes.contains_key("ProposalReviseNode"));
    let (url, roadmap_value) = persisted_roadmap(&stubs.http_post);
    assert_eq!(url, TEST_BRAIN_URL);
    assert_eq!(roadmap_value, final_ctx.nodes["ProposalReviseNode"]);
    assert_ne!(roadmap_value, final_ctx.nodes["ProposalWriterNode"]);
    let roadmap: AutomationRoadmap =
        serde_json::from_value(roadmap_value).expect("valid AutomationRoadmap");
    assert!(schema::validate_automation_roadmap(&roadmap).is_ok());

    let review_result = &final_ctx.nodes["ProposalReviewNode"];
    assert_eq!(review_result["verdict"], json!("revise"));

    assert_events_row_round_trips(event, &final_ctx);

    std::fs::remove_dir_all(&worktree).ok();
}

#[tokio::test]
async fn opportunity_identifier_scores_from_diagnostic_intake_evidence_end_to_end() {
    let worktree = temp_worktree("evidence");
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let mut stubs = Stubs::default_passing();
    stubs.opportunity = capturing_transport(stub_scored_candidates_json(), captured.clone());
    let registry = build_registry(&stubs);

    let event = base_event(Some(diagnostic_intake_fixture()));
    let (_branch, _final_ctx) = drive(event, &worktree, &registry).await;

    let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
    assert!(prompt.contains("diagnostic-intake evidence"));
    assert!(prompt.contains("\"Every day.\""));

    std::fs::remove_dir_all(&worktree).ok();
}

#[tokio::test]
async fn opportunity_identifier_falls_back_to_web_brief_end_to_end() {
    let worktree = temp_worktree("brief-fallback");
    let captured: Arc<Mutex<Option<(Config, String)>>> = Arc::new(Mutex::new(None));
    let mut stubs = Stubs::default_passing();
    stubs.opportunity = capturing_transport(stub_scored_candidates_json(), captured.clone());
    let registry = build_registry(&stubs);

    let event = base_event(None);
    let (_branch, _final_ctx) = drive(event, &worktree, &registry).await;

    let (_config, prompt) = captured.lock().unwrap().take().expect("transport called");
    assert!(prompt.contains("company research brief"));
    assert!(prompt.contains("Retail SMB tracking orders manually over WhatsApp."));

    std::fs::remove_dir_all(&worktree).ok();
}

/// Confirms the declared graph + real-transport registry still assemble via
/// `Workflow::new_validated` — the structural check `Workflow::run` would
/// have exercised internally, kept as its own smoke test since [`drive`]
/// above bypasses `Workflow::run`'s walk loop.
#[test]
fn assembled_workflow_builds_without_panicking() {
    let registry = graph::registry();
    let schema = graph::schema();
    let _workflow = Workflow::new_validated(registry, schema)
        .expect("PROPOSAL_GENERATOR declared graph must pass WorkflowValidator::validate");
}

#[test]
fn registry_for_policy_rewires_opportunity_review_revise_but_never_research() {
    let mut policy = ProposalGeneratorPolicy::default();
    policy.model_tiers.opportunity = ModelTier::Local;
    policy.model_tiers.review = ModelTier::Local;
    policy.model_tiers.revise = ModelTier::Local;

    let default_registry = graph::registry();
    let policy_registry = graph::registry_for_policy(&policy);

    // Rewiring never changes the registry's node count/identity set — only
    // the transport those nodes' composed `ClaudeCodeStep` uses.
    assert_eq!(policy_registry.len(), default_registry.len());
    for identity in [
        "ProposalCompanyResearchNode",
        "OpportunityIdentifierNode",
        "ProposalWriterNode",
        "ProposalReviewNode",
        "ProposalReviewRouterNode",
        "ProposalReviseNode",
        "PersistToBrainNode",
    ] {
        assert!(
            policy_registry.contains(identity),
            "registry_for_policy should still register '{identity}'"
        );
    }
}

#[test]
fn is_registered_true_and_workflow_listed_after_register_builtin_workflows() {
    let mut dispatcher = engine_serve::dispatch::Dispatcher::new();
    engine_serve::workflows::register_builtin_workflows(&mut dispatcher);

    assert!(dispatcher.is_registered("PROPOSAL_GENERATOR"));

    let listed = dispatcher.registered_types();
    assert!(
        listed.iter().any(|t| t == "PROPOSAL_GENERATOR"),
        "expected 'PROPOSAL_GENERATOR' in the dispatcher's registered_types() \
         (the GET /workflows-equivalent listing), got {listed:?}"
    );

    let schema = dispatcher
        .resolve_schema("PROPOSAL_GENERATOR")
        .expect("PROPOSAL_GENERATOR schema should resolve");
    assert_eq!(schema.start_node, "ProposalCompanyResearchNode");
}

// ---------------------------------------------------------------------------
// Deliverable-scoring validators: reject a malformed `AutomationRoadmap`,
// accept a well-formed one (`schema::validate_*`, exercised again here at
// the e2e level against the exact roadmap shape this pipeline produces).
// ---------------------------------------------------------------------------

fn well_formed_roadmap() -> AutomationRoadmap {
    serde_json::from_value(stub_roadmap_json(
        "Orders tracked by scrolling WhatsApp threads.",
    ))
    .expect("stub roadmap is well-formed")
}

#[test]
fn composite_sort_and_top_profiles_validators_accept_a_well_formed_roadmap() {
    let roadmap = well_formed_roadmap();
    assert!(schema::validate_composite_scores(&roadmap).is_ok());
    assert!(schema::validate_candidates_sorted(&roadmap).is_ok());
    assert!(schema::validate_top_profiles_count(&roadmap).is_ok());
    assert!(schema::validate_automation_roadmap(&roadmap).is_ok());
}

#[test]
fn composite_validator_rejects_a_malformed_composite() {
    let mut roadmap = well_formed_roadmap();
    roadmap.candidates[0].composite = 0.5; // deliberately wrong
    let err = schema::validate_composite_scores(&roadmap).unwrap_err();
    assert!(matches!(
        err,
        AutomationRoadmapValidationError::CompositeMismatch { index: 0, .. }
    ));
}

#[test]
fn sort_validator_rejects_out_of_order_candidates() {
    let mut roadmap = well_formed_roadmap();
    roadmap.candidates.reverse();
    let err = schema::validate_candidates_sorted(&roadmap).unwrap_err();
    assert!(matches!(
        err,
        AutomationRoadmapValidationError::NotSortedDescending { .. }
    ));
}

#[test]
fn top_profiles_validator_rejects_more_than_three_profiles() {
    let mut roadmap = well_formed_roadmap();
    let profile = roadmap.top_profiles[0].clone();
    roadmap.top_profiles = vec![profile.clone(), profile.clone(), profile.clone(), profile];
    let err = schema::validate_top_profiles_count(&roadmap).unwrap_err();
    assert!(matches!(
        err,
        AutomationRoadmapValidationError::TooManyTopProfiles { actual: 4, max: 3 }
    ));
}

// ---------------------------------------------------------------------------
// `#[ignore]`-gated experiment harness: mirrors `research_agent_e2e.rs`'s
// shape. Unlike the SDLC/research-agent workflows, no `PROPOSAL_GENERATOR`
// node persists its own `proposal-generator-state.json` (this workflow has
// no dedicated setup/wrap-up node), so this harness assembles the state
// snapshot itself from the driven run — `policy` (the resolved
// `ProposalGeneratorPolicy`), `telemetry` (a `crate::policy::RunTelemetry`
// harvested via `crate::policy::harvest_telemetry`), plus the two
// domain-specific fields the spec calls for: `review_verdict` and
// `revised`. Run manually:
//
// ```sh
// cargo test -p engine-core --test proposal_generator_e2e -- --ignored
// ```
// ---------------------------------------------------------------------------

const EXPERIMENT_PROFILES: [&str; 3] = ["baseline", "local-judgment", "skip-review"];

fn print_ranked_table(mut rows: Vec<policy::PolicyAggregate<ProposalGeneratorPolicy>>) {
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

fn extract_proposal_generator_run(
    value: &Value,
) -> Option<(ProposalGeneratorPolicy, policy::RunTelemetry)> {
    policy::extract_policy_telemetry::<ProposalGeneratorPolicy>(value, "policy", "telemetry")
}

/// Runs [`drive`] under each named profile, writes each run's
/// `proposal-generator-state.json`, and aggregates them into a ranked
/// per-profile table via `crate::policy::aggregate_state_files`.
///
/// Kept hermetic (stubbed transports + `HttpPost`) rather than a real-CLI
/// experiment, per the workflow's own hermetic-by-construction design.
/// `#[ignore]`d anyway (per the spec's Task 11 wording) so it stays out of
/// the default `cargo test` run and is runnable standalone for a manual
/// before/after profile comparison.
#[tokio::test]
#[ignore]
async fn experiment_named_profiles_ranked_by_cost() {
    let mut state_paths: Vec<PathBuf> = Vec::new();
    let mut worktrees: Vec<PathBuf> = Vec::new();

    for profile in EXPERIMENT_PROFILES {
        let worktree = temp_worktree(&format!("experiment-{profile}"));
        let stubs = Stubs::default_passing();
        let registry = build_registry(&stubs);

        let mut event = base_event(Some(diagnostic_intake_fixture()));
        event["profile"] = json!(profile);

        // Sanity-check the named profile resolves before driving the run.
        let mut probe_ctx = TaskContext {
            event: event.clone(),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        set_worktree(&mut probe_ctx, &worktree);
        let resolved_policy = profiles::resolve_policy_for_run(&probe_ctx, &worktree)
            .unwrap_or_else(|err| panic!("profile {profile:?} should resolve: {err}"));

        let (branch, final_ctx) = drive(event, &worktree, &registry).await;

        let review_verdict = final_ctx.nodes["ProposalReviewNode"]["verdict"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let revised = branch == "ProposalReviseNode";

        let model_tier_used = BTreeMap::from([
            (
                "research".to_string(),
                format!("{:?}", resolved_policy.model_tiers.research),
            ),
            (
                "opportunity".to_string(),
                format!("{:?}", resolved_policy.model_tiers.opportunity),
            ),
            (
                "writer".to_string(),
                format!("{:?}", resolved_policy.model_tiers.writer),
            ),
            (
                "review".to_string(),
                format!("{:?}", resolved_policy.model_tiers.review),
            ),
            (
                "revise".to_string(),
                format!("{:?}", resolved_policy.model_tiers.revise),
            ),
        ]);
        let telemetry = policy::harvest_telemetry(
            &final_ctx,
            chrono::Utc::now(),
            policy::RunTelemetryInputs {
                start_node_identity: "ProposalCompanyResearchNode",
                verdict_stages: &["ProposalReviewNode"],
                cost_bearing_stages: &[],
                total_attempts: 1,
                total_retries: u32::from(revised),
                tasks_passed: 1,
                tasks_failed: 0,
                model_tier_used,
                model_stages: &[],
            },
        );

        let state_path = worktree
            .join("planning")
            .join("proposal-generator-state.json");
        std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&json!({
                "policy": resolved_policy,
                "telemetry": telemetry,
                "review_verdict": review_verdict,
                "revised": revised,
            }))
            .expect("state serializes"),
        )
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", state_path.display()));

        state_paths.push(state_path);
        worktrees.push(worktree);
    }

    let rows = policy::aggregate_state_files::<ProposalGeneratorPolicy, _>(
        &state_paths,
        extract_proposal_generator_run,
    )
    .expect("all proposal-generator-state.json files should parse");

    print_ranked_table(rows.clone());

    assert!(
        !rows.is_empty(),
        "expected at least one aggregated policy row across the named profiles"
    );

    for worktree in &worktrees {
        std::fs::remove_dir_all(worktree).ok();
    }
}
