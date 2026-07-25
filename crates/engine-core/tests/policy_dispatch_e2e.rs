//! End-to-end hermetic test for `EN.5.D`'s headline claim: a `profile` sent
//! over `POST /events/` actually reaches a *served* run and changes which
//! transport a judgment stage calls — not just which `ProposalGeneratorPolicy`
//! value gets computed in isolation.
//!
//! Entirely hermetic: no real `claude` subprocess is ever spawned (every
//! model node is wired to a stub `ModelTransport`) and no live network call
//! ever happens (the `local` tier's `LocalHttpPost` seam and
//! `PersistToBrainNode`'s `HttpPost` seam are both stubbed). This file
//! drives the real `engine_serve::dispatch::Dispatcher` (a dev-dependency of
//! `engine-core`, not a cycle — see `engine-serve/src/workflows.rs`'s module
//! doc) exactly the way `POST /events/` does: `dispatch_with_event` resolves
//! policy from the triggering event and hands back a runnable `Workflow`,
//! which is then driven through the real `Workflow::run_with` pointer walk
//! (not a hand-rolled node-by-node drive), so the framework-owned
//! `RunTelemetry` stamp (`EN.5.D` task 10) is exercised for real.
//!
//! Covers the spec's Acceptance Criteria:
//! (a) `{"profile": "local-judgment"}` resolves `{opportunity, review,
//!     revise}` to `ModelTier::Local` and `research`/`writer` stay off it —
//!     an assertion that fails against `main` today (no served path reaches
//!     `registry_for_policy` at all pre-`EN.5.D`);
//! (b) driving that resolved policy's judgment stages through the real
//!     `openai_compat_transport` seam (stubbed `LocalHttpPost`, a
//!     panic-if-called cloud fallback) proves the local content actually
//!     flows through the run, while `research`'s cloud-only stub content
//!     flows through unchanged;
//! (c) an unknown `profile` name fails loudly through the real production
//!     `Dispatcher` (`engine_serve::workflows::register_proposal_generator`),
//!     naming the offending profile, rather than silently resolving to
//!     builtin defaults;
//! (d) `PROPOSAL_GENERATOR`'s registration has no worktree at dispatch time
//!     (no `SetupWorktreeNode`, no repo) and still resolves policy
//!     successfully — the worktree-free `PolicyConfigSource::Builtin` path;
//! (e) the completed run's `TaskContext` round-trips to an `EventsRow`
//!     unchanged, and carries a `RunTelemetry` block in `metadata`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
use claude_code_rs::{Config, Outcome};
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::nodes::http_post::StubHttpPost;
use engine_core::nodes::openai_compat_transport::{openai_compat_transport, LocalHttpPost};
use engine_core::policy::{self, PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::workflow::{Workflow, RUN_TELEMETRY_METADATA_KEY};
use engine_core::workflows::proposal_generator::company_research::ProposalCompanyResearchNode;
use engine_core::workflows::proposal_generator::graph;
use engine_core::workflows::proposal_generator::opportunity_identifier::OpportunityIdentifierNode;
use engine_core::workflows::proposal_generator::persist_to_brain::PersistToBrainNode;
use engine_core::workflows::proposal_generator::policy::{ModelTier, ProposalGeneratorPolicy};
use engine_core::workflows::proposal_generator::profiles;
use engine_core::workflows::proposal_generator::review::ProposalReviewNode;
use engine_core::workflows::proposal_generator::review_router::ProposalReviewRouterNode;
use engine_core::workflows::proposal_generator::revise::ProposalReviseNode;
use engine_core::workflows::proposal_generator::writer::ProposalWriterNode;
use engine_core::workflows::ModelTransport;
use engine_serve::dispatch::{DispatchError, Dispatcher};
use futures::FutureExt;
use serde_json::{json, Value};
use uuid::Uuid;

const TEST_BRAIN_URL: &str = "https://brain.example/ingest/policy-dispatch-e2e";

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

/// A cloud transport that always replies with `structured`, regardless of
/// the prompt/config it's handed. Used for the two stages `local-judgment`
/// never rewires (`research`, `writer`).
fn cloud_stub_transport(structured: Value) -> ModelTransport {
    Arc::new(move |_config: Config, _prompt: String| {
        let structured = structured.clone();
        async move { Ok(stub_outcome(structured, 40, 20)) }.boxed()
    })
}

/// A cloud fallback that panics if invoked — wired as the `local`-tier
/// nodes' `cloud_fallback` argument in this file's tests, so the run only
/// passes if the local `LocalHttpPost` stub was actually reached and
/// succeeded (proving the profile really routed those stages to
/// `openai_compat_transport`'s local path, not a silent cloud fallback).
fn panicking_cloud_fallback() -> ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        async move {
            panic!(
                "cloud fallback should never be called: the stubbed local \
                 endpoint always succeeds in this test"
            )
        }
        .boxed()
    })
}

/// A `LocalHttpPost` stub that always answers with `content` as the chat
/// completion's message content (bare JSON text, no code fence — exercises
/// `parse_structured_or_fenced`'s fenced-text fallback, since
/// `openai_compat_transport`'s synthesized `Outcome` never carries a
/// `structured_output`).
fn local_http_post_returning(content: Value) -> LocalHttpPost {
    let text = serde_json::to_string(&content).unwrap();
    Arc::new(move |_url: String, _body: Value| {
        let text = text.clone();
        async move {
            Ok(json!({
                "choices": [{ "message": { "content": text } }],
                "usage": { "prompt_tokens": 12, "completion_tokens": 34 },
            }))
        }
        .boxed()
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
                "name": "WhatsApp order tracking",
                "frequency": 5.0,
                "time_cost": 4.0,
                "buildability": 4.0,
                "rationale": "Happens daily and is fully manual today."
            },
            {
                "name": "Supplier follow-up messages",
                "frequency": 3.0,
                "time_cost": 2.0,
                "buildability": 4.0,
                "rationale": "Frequent but lower time cost."
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
        "phases": [
            {
                "phase_number": 1,
                "phase_name": "Automate order tracking",
                "workflow_name": "WhatsApp order tracking",
                "problem_statement": "Orders are tracked by scrolling chat threads.",
                "proposed_automation": "A shared order-tracking sheet fed by webhook.",
                "weekly_time_saved_hours": 5.0,
                "price_usd": 800.0,
                "timeline_weeks": 2,
            },
            {
                "phase_number": 2,
                "phase_name": "Automate supplier follow-up",
                "workflow_name": "Supplier follow-up messages",
                "problem_statement": "Follow-ups are typed by hand.",
                "proposed_automation": "Templated scheduled messages.",
                "weekly_time_saved_hours": 2.0,
                "price_usd": 400.0,
                "timeline_weeks": 1,
            },
        ],
        "total_price_usd": 1200.0,
        "total_weekly_hours_saved": 7.0,
    })
}

fn stub_review_verdict_json(verdict: &str, notes: &str) -> Value {
    json!({ "verdict": verdict, "notes": notes })
}

fn base_event(profile: Option<&str>) -> Value {
    json!({
        "company_name": "Loja da Ana",
        "company_url": "https://loja-da-ana.example",
        "profile": profile,
    })
}

/// Serialize `policy` into the single-entry seed map
/// `{RESOLVED_POLICY_IDENTITY: policy}` — the same shape
/// `engine_serve::workflows`'s `seed_resolved_policy` writes, so a node
/// reading the stamp sees the identical representation whether it was
/// seeded at dispatch or stamped mid-run.
fn seed_resolved_policy<P: serde::Serialize>(policy: &P) -> HashMap<String, Value> {
    let value = serde_json::to_value(policy).expect("policy should serialize");
    let mut seeded = HashMap::new();
    seeded.insert(RESOLVED_POLICY_IDENTITY.to_string(), value);
    seeded
}

/// Build a hermetic `PROPOSAL_GENERATOR` registry wired the way
/// `graph::registry_for_policy` wires it — `{opportunity, review, revise}`
/// route through `openai_compat_transport` whenever `policy` resolves them
/// to `ModelTier::Local`, `research`/`writer` never do — except every
/// transport (local's `LocalHttpPost`, every stage's cloud stub) is
/// injected as a hermetic stub rather than `graph.rs`'s real
/// `reqwest`/`claude` CLI wiring. This is the same "stub-wire the real
/// declared graph instead of calling `registry_for_policy` directly"
/// pattern `proposal_generator_e2e.rs::build_registry` already uses for
/// non-local stages.
fn build_hermetic_registry(policy: &ProposalGeneratorPolicy) -> NodeRegistry {
    let mut registry = NodeRegistry::new();

    registry.register(Box::new(
        ProposalCompanyResearchNode::new()
            .with_transport(cloud_stub_transport(stub_company_brief_json())),
    ));

    let opportunity_transport = if policy.model_tiers.opportunity == ModelTier::Local {
        openai_compat_transport(
            policy.local.clone(),
            local_http_post_returning(stub_scored_candidates_json()),
            panicking_cloud_fallback(),
        )
    } else {
        cloud_stub_transport(stub_scored_candidates_json())
    };
    registry.register(Box::new(
        OpportunityIdentifierNode::new().with_transport(opportunity_transport),
    ));

    registry.register(Box::new(ProposalWriterNode::new().with_transport(
        cloud_stub_transport(stub_roadmap_json(
            "Orders tracked by scrolling WhatsApp threads.",
        )),
    )));

    let review_transport = if policy.model_tiers.review == ModelTier::Local {
        openai_compat_transport(
            policy.local.clone(),
            local_http_post_returning(stub_review_verdict_json("pass", "Looks good.")),
            panicking_cloud_fallback(),
        )
    } else {
        cloud_stub_transport(stub_review_verdict_json("pass", "Looks good."))
    };
    registry.register(Box::new(
        ProposalReviewNode::new().with_transport(review_transport),
    ));

    registry.register(Box::new(ProposalReviewRouterNode));

    let revise_transport = if policy.model_tiers.revise == ModelTier::Local {
        openai_compat_transport(
            policy.local.clone(),
            local_http_post_returning(stub_roadmap_json("Corrected via local revise.")),
            panicking_cloud_fallback(),
        )
    } else {
        cloud_stub_transport(stub_roadmap_json("Corrected via cloud revise."))
    };
    registry.register(Box::new(
        ProposalReviseNode::new().with_transport(revise_transport),
    ));

    registry.register(Box::new(
        PersistToBrainNode::new()
            .with_http_post(Arc::new(StubHttpPost::succeeding(json!({"ok": true}))))
            .with_url(TEST_BRAIN_URL),
    ));

    registry
}

/// Register `PROPOSAL_GENERATOR` on a fresh `Dispatcher` with a hermetic,
/// stub-wired factory: mirrors
/// `engine_serve::workflows::register_proposal_generator`'s dispatch-time
/// shape exactly (resolve policy from the event via
/// `PolicyConfigSource::Builtin`, build the policy-dependent registry, seed
/// `RESOLVED_POLICY_IDENTITY`) but swaps `graph::registry_for_policy`'s
/// real-transport registry for [`build_hermetic_registry`]'s stub-wired
/// one, so this test never spawns a subprocess or contacts a live network
/// endpoint while still exercising the real
/// `Dispatcher::dispatch_with_event` seam.
fn register_hermetic_proposal_generator(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        graph::schema(),
        Box::new(|event: &Value| {
            let ctx = TaskContext {
                event: event.clone(),
                nodes: HashMap::new(),
                metadata: json!({}),
                node_runs: HashMap::new(),
            };
            let policy = profiles::resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
                .map_err(|err| err.to_string())?;
            let registry = build_hermetic_registry(&policy);
            let seeded = seed_resolved_policy(&policy);
            Workflow::new_validated(registry, graph::schema())
                .map(|workflow| workflow.with_seeded_nodes(seeded))
                .map_err(|err| err.to_string())
        }),
    );
}

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

/// (a) + (b) + (e): triggering `PROPOSAL_GENERATOR` through the served
/// dispatch path (`Dispatcher::dispatch_with_event`, exactly as `POST
/// /events/` calls it) with `{"profile": "local-judgment"}` resolves the
/// judgment stages to `ModelTier::Local`, and actually running the
/// dispatched `Workflow` proves those stages' local content flows through
/// (the panic-if-called cloud fallback never fires) while `research`'s
/// cloud-only content is untouched. The completed run's `TaskContext`
/// carries a `RunTelemetry` block and round-trips to an `EventsRow`
/// unchanged.
#[tokio::test]
async fn local_judgment_profile_routes_judgment_stages_through_local_dispatch() {
    let mut dispatcher = Dispatcher::new();
    register_hermetic_proposal_generator(&mut dispatcher);

    let event = base_event(Some("local-judgment"));

    // Confirm the resolved policy itself first (the AC's structural half):
    // `{opportunity, review, revise}` -> Local, `research`/`writer` stay off
    // it — this is the exact resolution `dispatch_with_event`'s factory
    // performs internally.
    let ctx_for_policy_check = TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    let resolved =
        profiles::resolve_policy_for_run_from(&ctx_for_policy_check, &PolicyConfigSource::Builtin)
            .expect("local-judgment should resolve");
    assert_eq!(resolved.model_tiers.opportunity, ModelTier::Local);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
    assert_ne!(resolved.model_tiers.research, ModelTier::Local);
    assert_ne!(resolved.model_tiers.writer, ModelTier::Local);

    let workflow = dispatcher
        .dispatch_with_event(graph::WORKFLOW_TYPE, &event)
        .expect("dispatch_with_event should resolve 'local-judgment' to a runnable Workflow");

    let on_progress: engine_core::OnProgress<'_> = Box::new(|_ctx| {});
    let final_ctx = workflow
        .run_with(
            event.clone(),
            on_progress,
            engine_core::RunOptions::default(),
        )
        .await
        .expect("PROPOSAL_GENERATOR should run to completion against stubbed transports");

    // The pass verdict routes straight to `PersistToBrainNode`; every node
    // on that path ran and succeeded, and the cloud-fallback panic never
    // fired (or this test would already have failed) — proving the
    // judgment stages actually hit the local `openai_compat_transport`
    // path, not a silent cloud fallback.
    for identity in [
        "ProposalCompanyResearchNode",
        "OpportunityIdentifierNode",
        "ProposalWriterNode",
        "ProposalReviewNode",
        "ProposalReviewRouterNode",
        "PersistToBrainNode",
    ] {
        assert_eq!(
            final_ctx.node_runs[identity].status,
            NodeRunStatus::Success,
            "'{identity}' should have run to completion"
        );
    }

    // The local-routed `OpportunityIdentifierNode` stage's final output
    // (scored + sorted candidates, `put_result`-written over the raw
    // `ClaudeCodeStep` output) should carry the candidate the local stub
    // returned.
    let opportunity_output = &final_ctx.nodes["OpportunityIdentifierNode"];
    let candidate_names: Vec<&str> = opportunity_output["candidates"]
        .as_array()
        .expect("OpportunityIdentifierNode should have written a candidates array")
        .iter()
        .map(|candidate| candidate["name"].as_str().unwrap())
        .collect();
    assert!(
        candidate_names.contains(&"WhatsApp order tracking"),
        "OpportunityIdentifierNode's output should carry the local stub's candidates, got: {candidate_names:?}"
    );

    // `research` never rewires: its final output (the parsed `CompanyBrief`,
    // `put_result`-written over the raw `ClaudeCodeStep` output) carries the
    // cloud stub's brief.
    let research_output = &final_ctx.nodes["ProposalCompanyResearchNode"];
    assert_eq!(
        research_output["company_name"].as_str().unwrap(),
        "Loja da Ana",
        "ProposalCompanyResearchNode's output should carry the cloud stub's brief"
    );

    // (e) `RunTelemetry` was stamped by the real `Workflow::run_with` walk
    // (EN.5.D task 10) — not hand-rolled by this test.
    let telemetry_value = final_ctx
        .metadata
        .get(RUN_TELEMETRY_METADATA_KEY)
        .unwrap_or_else(|| panic!("metadata should carry '{RUN_TELEMETRY_METADATA_KEY}'"));
    let telemetry: policy::RunTelemetry = serde_json::from_value(telemetry_value.clone())
        .expect("run_telemetry metadata should deserialize into RunTelemetry");
    assert!(
        telemetry.total_cost_usd >= 0.0,
        "telemetry should carry a non-negative total cost"
    );

    // (e) The completed run's `TaskContext` round-trips to an `EventsRow`
    // unchanged, and the `TaskContext`/`EventsRow` wire shape stays exactly
    // what the durable store already writes.
    assert_events_row_round_trips(event, &final_ctx);
}

/// (c): an unknown `profile` name sent over the real, production
/// `engine_serve::workflows::register_proposal_generator` registration
/// fails loudly — a 4xx-shaped `DispatchError`, not a silent fall-through
/// to builtin defaults — and names the offending profile. Policy
/// resolution never invokes a transport, so this exercises the real
/// production factory directly (no stubbing needed).
#[test]
fn unknown_profile_name_fails_loudly_through_the_real_dispatcher() {
    let mut dispatcher = Dispatcher::new();
    engine_serve::workflows::register_proposal_generator(&mut dispatcher);

    let event = base_event(Some("not-a-real-profile"));

    let result = dispatcher.dispatch_with_event(graph::WORKFLOW_TYPE, &event);

    match result {
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            assert!(
                message.contains("not-a-real-profile"),
                "error message should name the offending profile, got: {message}"
            );
        }
        Ok(_) => panic!("expected PolicyResolutionFailed for an unknown profile, got Ok"),
        Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
    }
}

/// (d): `PROPOSAL_GENERATOR` has no worktree at dispatch time (no
/// `SetupWorktreeNode` output, no repo checkout) — its production
/// registration resolves policy via the worktree-free
/// `PolicyConfigSource::Builtin` rather than falling back to
/// `std::env::current_dir()`, so an event with no worktree information at
/// all still resolves policy successfully.
#[test]
fn workflow_with_no_worktree_path_resolves_policy_successfully() {
    let mut dispatcher = Dispatcher::new();
    engine_serve::workflows::register_proposal_generator(&mut dispatcher);

    let event = base_event(None);

    let result = dispatcher.dispatch_with_event(graph::WORKFLOW_TYPE, &event);

    match result {
        Ok(_) => {}
        Err(err) => panic!(
            "a workflow with no worktree path should resolve policy successfully, got: {err}"
        ),
    }
}

/// Sanity check that this file's stub-wired registration mirrors
/// `graph::registry_for_policy`'s node identity set exactly (same seven
/// nodes), so the "hermetic dispatch" path this file drives is a faithful
/// stand-in for the real one, not a different graph shape.
#[test]
fn hermetic_registry_contains_the_same_seven_node_identities_as_the_real_one() {
    let policy = ProposalGeneratorPolicy::default();
    let registry = build_hermetic_registry(&policy);
    let real_registry = graph::registry();

    assert_eq!(registry.len(), real_registry.len());
    for identity in [
        "ProposalCompanyResearchNode",
        "OpportunityIdentifierNode",
        "ProposalWriterNode",
        "ProposalReviewNode",
        "ProposalReviewRouterNode",
        "ProposalReviseNode",
        "PersistToBrainNode",
    ] {
        assert!(registry.contains(identity));
    }
}
