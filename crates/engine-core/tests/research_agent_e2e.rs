//! Hermetic end-to-end integration test for the assembled `RESEARCH_AGENT`
//! workflow (`EN.4.A` task 8): drives the real declared graph
//! ([`research_agent::graph::schema`]) from `ResearchModeRouterNode` through
//! each terminal node with a stubbed transport, mirroring the seam-injection
//! style of `sdlc_flow_e2e.rs`.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned —
//! both `CompanyResearchNode` and `ProspectingResearchNode` are built with
//! `with_transport` stubs. Unlike `SDLC_FLOW`, `RESEARCH_AGENT` has no
//! dedicated setup node (per the spec's Context Pointers), so
//! `Workflow::run` (which seeds `TaskContext` from `event` alone, with no
//! way to pre-stamp a `SetupWorktreeNode` result) cannot be handed a
//! controlled temp-dir worktree path up front. Rather than redirect the
//! whole test process's current directory (a global, racy side effect
//! across parallel test threads), this file drives the two-hop walk
//! (`ResearchModeRouterNode::route` -> the routed terminal node's
//! `Node::process`) directly against the same registered node instances
//! [`build_registry`] would hand to a real `Workflow` — real composition,
//! real routing decision, real terminal-node logic, just not through
//! `Workflow::run`'s own walk loop. [`assembled_workflow_builds`] below
//! separately confirms the declared graph + registry still assemble via
//! `Workflow::new_validated` (the same check `Workflow::run` would have
//! exercised internally).
//!
//! Covers the spec's acceptance criteria for task 8:
//! (a) a `mode: company` event routes `ResearchModeRouterNode ->
//!     CompanyResearchNode`, and a `mode: prospecting` event routes to
//!     `ProspectingResearchNode`;
//! (b) the final `TaskContext` round-trips to an `EventsRow` (via
//!     `engine-contract`) without loss, for both modes;
//! (c) `registry_for_policy` rewires nothing to the `Local` tier on the two
//!     WebSearch stages;
//! (d) `is_registered("RESEARCH_AGENT")` is true after registration through
//!     `engine-serve`'s `Dispatcher`, and the workflow appears in the
//!     dispatcher's `GET /workflows`-equivalent listing
//!     (`Dispatcher::registered_types`);
//! (e) an `#[ignore]`-gated experiment harness runs the workflow under each
//!     named profile, writes `research-agent-state.json` snapshots, and
//!     aggregates them into a ranked per-profile table via
//!     `crate::policy::aggregate_state_files`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use claude_code_rs::{Config, Outcome};
use engine_contract::{EventsRow, NodeRunStatus, TaskContext};
use engine_core::node::NodeRegistry;
use engine_core::policy;
use engine_core::workflow::Workflow;
use engine_core::workflows::research_agent::graph::{self, ResearchModeRouterNode};
use engine_core::workflows::research_agent::policy::{ModelTier, ResearchAgentPolicy};
use engine_core::workflows::research_agent::profiles;
use engine_core::workflows::research_agent::prospecting::ProspectingResearchNode;
use engine_core::workflows::research_agent::CompanyResearchNode;
use futures::FutureExt;
use serde_json::json;
use uuid::Uuid;

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-research-agent-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
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
        });
        async move { Ok(stub_outcome(brief, 100, 50)) }.boxed()
    })
}

fn stub_prospecting_transport() -> engine_core::workflows::ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        let result = json!({
            "vertical": "legal-tech",
            "prospects": [
                {
                    "name": "Jane Doe Legal",
                    "pain_points": ["Slow contract turnaround"],
                    "pillar": "automation",
                    "outreach_hook": "Posted about contract delays on r/legaltech",
                    "source": "https://reddit.com/r/legaltech/abc",
                }
            ],
            "common_pain_points": ["Manual contract review"],
            "sources": ["https://reddit.com/r/legaltech"],
        });
        async move { Ok(stub_outcome(result, 120, 80)) }.boxed()
    })
}

/// Builds a fresh registry with every `RESEARCH_AGENT` node identity
/// registered, both terminal nodes wired to a stubbed transport.
fn build_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(ResearchModeRouterNode));
    registry.register(Box::new(
        CompanyResearchNode::new().with_transport(stub_company_transport()),
    ));
    registry.register(Box::new(
        ProspectingResearchNode::new().with_transport(stub_prospecting_transport()),
    ));
    registry
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
        .expect("RESEARCH_AGENT policy should resolve for this event");
    policy::stamp_resolved_policy(ctx, &resolved).expect("policy should stamp");
}

/// Drives the full logical `RESEARCH_AGENT` walk against the exact node
/// instances a real `Workflow` built from [`build_registry`] would use:
/// `ResearchModeRouterNode::route` picks the terminal identity from
/// `event.mode`, then that node's `Node::process` runs for real (stubbed
/// transport only). See the module doc for why this drives the nodes
/// directly rather than through `Workflow::run`.
async fn drive(event: serde_json::Value, worktree: &Path) -> (String, TaskContext) {
    let registry = build_registry();

    let mut ctx = TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    set_worktree(&mut ctx, worktree);
    stamp_resolved_policy(&mut ctx, worktree);

    let router = registry
        .get("ResearchModeRouterNode")
        .expect("router should be registered");
    let target = router
        .as_router()
        .expect("ResearchModeRouterNode should be a Router")
        .route(&ctx)
        .expect("router should resolve a target identity for a valid mode");

    let terminal = registry
        .get(&target)
        .unwrap_or_else(|| panic!("routed identity '{target}' should be registered"));
    let mut ctx = terminal
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("'{target}' should process successfully: {err}"));

    // `Node::process` alone (unlike `Workflow::run`'s walk loop) never
    // stamps the terminal `NodeRunStatus::Success` transition — that's the
    // walk loop's job, not the node's. Replicate it here so the returned
    // `TaskContext` matches what a real `Workflow::run` would have produced
    // (mirrors `workflow.rs`'s own post-`process` success stamping).
    if let Some(run) = ctx.node_runs.get_mut(&target) {
        run.status = engine_contract::NodeRunStatus::Success;
        run.completed_at = Some(chrono::Utc::now());
    }

    (target, ctx)
}

/// Builds an [`EventsRow`] for a completed run the way the durable store
/// layer would: `data` is the run's inbound event, `task_context` is the
/// final `TaskContext`. Mirrors `sdlc_flow_e2e.rs::events_row_for`.
fn events_row_for(workflow_type: &str, event: serde_json::Value, ctx: &TaskContext) -> EventsRow {
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

#[tokio::test]
async fn company_mode_routes_to_company_research_node_and_round_trips_events_row() {
    let worktree = temp_worktree("company");

    let event = json!({
        "mode": "company",
        "company_name": "Acme Corp",
        "company_url": "https://acme.example",
    });

    let (target, final_ctx) = drive(event.clone(), &worktree).await;
    assert_eq!(target, "CompanyResearchNode");

    let brief = final_ctx
        .nodes
        .get("CompanyResearchNode")
        .expect("CompanyResearchNode should have stamped a result");
    assert_eq!(brief["company_name"], json!("Acme Corp"));
    assert!(!brief["summary"].as_str().unwrap_or_default().is_empty());

    let run = final_ctx
        .node_runs
        .get("CompanyResearchNode")
        .expect("expected a NodeRun for 'CompanyResearchNode'");
    assert_eq!(run.status, NodeRunStatus::Success);

    // --- Durable EventsRow mapping round-trips without loss -----------------
    let row = events_row_for("RESEARCH_AGENT", event, &final_ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "RESEARCH_AGENT");
    assert_eq!(
        round_tripped.task_context.nodes.get("CompanyResearchNode"),
        final_ctx.nodes.get("CompanyResearchNode")
    );

    std::fs::remove_dir_all(&worktree).ok();
}

#[tokio::test]
async fn prospecting_mode_routes_to_prospecting_research_node_and_round_trips_events_row() {
    let worktree = temp_worktree("prospecting");

    let event = json!({
        "mode": "prospecting",
        "vertical": "legal-tech",
        "topic": "contract review pain points",
    });

    let (target, final_ctx) = drive(event.clone(), &worktree).await;
    assert_eq!(target, "ProspectingResearchNode");

    let result = final_ctx
        .nodes
        .get("ProspectingResearchNode")
        .expect("ProspectingResearchNode should have stamped a result");
    assert_eq!(result["vertical"], json!("legal-tech"));
    assert!(!result["prospects"].as_array().unwrap().is_empty());

    let run = final_ctx
        .node_runs
        .get("ProspectingResearchNode")
        .expect("expected a NodeRun for 'ProspectingResearchNode'");
    assert_eq!(run.status, NodeRunStatus::Success);

    // --- Durable EventsRow mapping round-trips without loss -----------------
    let row = events_row_for("RESEARCH_AGENT", event, &final_ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "RESEARCH_AGENT");
    assert_eq!(
        round_tripped
            .task_context
            .nodes
            .get("ProspectingResearchNode"),
        final_ctx.nodes.get("ProspectingResearchNode")
    );

    std::fs::remove_dir_all(&worktree).ok();
}

/// Confirms the declared graph + real-transport registry still assemble via
/// `Workflow::new_validated` — the structural check `Workflow::run` would
/// have exercised internally, kept as its own smoke test since [`drive`]
/// above bypasses `Workflow::run`'s walk loop.
#[test]
fn assembled_workflow_builds_without_panicking() {
    let registry = build_registry();
    let schema = graph::schema();
    let _workflow = Workflow::new_validated(registry, schema)
        .expect("RESEARCH_AGENT declared graph must pass WorkflowValidator::validate");
}

#[test]
fn registry_for_policy_rewires_nothing_to_local_on_websearch_stages() {
    let mut local_policy = ResearchAgentPolicy::default();
    local_policy.model_tiers.research = ModelTier::Local;
    local_policy.model_tiers.prospect = ModelTier::Local;

    // Even when the resolved policy asks for the (unsupported) `Local`
    // tier on either WebSearch stage, `registry_for_policy` must return the
    // exact same node set as the real-transport `registry()` — no stage is
    // ever rewired to a local single-shot endpoint, since that endpoint
    // cannot serve `WebSearch`/`WebFetch`.
    let default_registry = graph::registry();
    let policy_registry = graph::registry_for_policy(&local_policy);

    assert_eq!(policy_registry.len(), default_registry.len());
    for identity in [
        "ResearchModeRouterNode",
        "CompanyResearchNode",
        "ProspectingResearchNode",
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

    assert!(dispatcher.is_registered("RESEARCH_AGENT"));

    let listed = dispatcher.registered_types();
    assert!(
        listed.iter().any(|t| t == "RESEARCH_AGENT"),
        "expected 'RESEARCH_AGENT' in the dispatcher's registered_types() \
         (the GET /workflows-equivalent listing), got {listed:?}"
    );

    let schema = dispatcher
        .resolve_schema("RESEARCH_AGENT")
        .expect("RESEARCH_AGENT schema should resolve");
    assert_eq!(schema.start_node, "ResearchModeRouterNode");
}

// ---------------------------------------------------------------------------
// `#[ignore]`-gated experiment harness: mirrors `sdlc_flow_experiment.rs`'s
// shape, but hermetic (stubbed transports) rather than a real-CLI run, since
// `RESEARCH_AGENT`'s two terminal nodes require live `WebSearch`/`WebFetch`
// tool access to produce a meaningful real-CLI comparison — out of scope for
// this gated suite. Run manually:
//
// ```sh
// cargo test -p engine-core --test research_agent_e2e -- --ignored
// ```
// ---------------------------------------------------------------------------

const EXPERIMENT_PROFILES: [&str; 3] = ["baseline", "cheap-fast", "thorough"];

fn print_ranked_table(mut rows: Vec<policy::PolicyAggregate<ResearchAgentPolicy>>) {
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

fn extract_research_agent_run(
    value: &serde_json::Value,
) -> Option<(ResearchAgentPolicy, policy::RunTelemetry)> {
    policy::extract_policy_telemetry::<ResearchAgentPolicy>(value, "policy", "telemetry")
}

/// Runs [`drive`] under each named profile (company mode), writes each
/// run's `research-agent-state.json`, and aggregates them into a ranked
/// per-profile table via `crate::policy::aggregate_state_files`.
///
/// Kept hermetic (stubbed transports) rather than a real-CLI experiment —
/// unlike `sdlc_flow_experiment.rs`, a real run here would need a live
/// `WebSearch`/`WebFetch`-capable `claude` session, which this gated suite
/// never spawns. `#[ignore]`d anyway (per the spec's Task 8 wording) so it
/// stays out of the default `cargo test` run and is runnable standalone for
/// a manual before/after profile comparison.
#[tokio::test]
#[ignore]
async fn experiment_named_profiles_ranked_by_cost() {
    let mut state_paths: Vec<PathBuf> = Vec::new();
    let mut worktrees: Vec<PathBuf> = Vec::new();

    for profile in EXPERIMENT_PROFILES {
        let worktree = temp_worktree(&format!("experiment-{profile}"));

        let event = json!({
            "mode": "company",
            "company_name": "Acme Corp",
            "profile": profile,
        });

        // Sanity-check the named profile resolves before driving the run.
        let mut probe_ctx = TaskContext {
            event: event.clone(),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        set_worktree(&mut probe_ctx, &worktree);
        let resolved = profiles::resolve_policy_for_run(&probe_ctx, &worktree)
            .unwrap_or_else(|err| panic!("profile {profile:?} should resolve: {err}"));
        assert_ne!(resolved.model_tiers.research, ModelTier::Local);

        let (_target, _final_ctx) = drive(event, &worktree).await;

        let state_path = worktree.join("planning").join("research-agent-state.json");
        assert!(
            state_path.exists(),
            "profile {profile:?}: expected CompanyResearchNode to persist {}",
            state_path.display()
        );
        state_paths.push(state_path);
        worktrees.push(worktree);
    }

    let rows = policy::aggregate_state_files::<ResearchAgentPolicy, _>(
        &state_paths,
        extract_research_agent_run,
    )
    .expect("all research-agent-state.json files should parse");

    print_ranked_table(rows.clone());

    assert!(
        !rows.is_empty(),
        "expected at least one aggregated policy row across the named profiles"
    );

    for worktree in &worktrees {
        std::fs::remove_dir_all(worktree).ok();
    }
}
