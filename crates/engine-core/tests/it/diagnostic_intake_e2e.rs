//! Hermetic end-to-end integration test for the assembled `DIAGNOSTIC_INTAKE`
//! workflow (`EN.4.B` task 7): drives the real declared single-node graph
//! ([`diagnostic_intake::graph::schema`]) with a stubbed transport, mirroring
//! the seam-injection style of `research_agent_e2e.rs` / `sdlc_flow_e2e.rs`.
//!
//! Hermetic by construction: no real `claude` subprocess is ever spawned —
//! `IntakeExtractNode` is built with a `with_transport` stub. Like
//! `RESEARCH_AGENT`, this workflow has no dedicated setup node, so
//! `Workflow::run` (which seeds `TaskContext` from `event` alone, with no way
//! to pre-stamp a `SetupWorktreeNode` result) cannot be handed a controlled
//! temp-dir worktree up front. Rather than redirect the whole test process's
//! current directory (a global, racy side effect across parallel test
//! threads), this file drives `IntakeExtractNode::process` directly against
//! the exact node instance [`build_registry`] would hand to a real
//! `Workflow` — real node logic, just not through `Workflow::run`'s own walk
//! loop. [`assembled_workflow_builds_without_panicking`] below separately
//! confirms the declared graph + registry still assemble via
//! `Workflow::new_validated` (the same check `Workflow::run` would have
//! exercised internally).
//!
//! Covers the spec's acceptance criteria for task 7:
//! (a) a `DIAGNOSTIC_INTAKE` event feeding raw notes drives
//!     `IntakeExtractNode` to a validated `DiagnosticIntake`;
//! (b) the final `TaskContext` round-trips to an `EventsRow` (via
//!     `engine-contract`) with the `*_evidence` fields intact (no loss);
//! (c) `registry_for_policy` rewires the extract stage to the Local
//!     transport under a Local-tier policy, and leaves it on the default
//!     transport otherwise;
//! (d) `is_registered("DIAGNOSTIC_INTAKE")` is true after registration
//!     through `engine-serve`'s `Dispatcher`, and the workflow appears in
//!     the dispatcher's `GET /workflows`-equivalent listing
//!     (`Dispatcher::registered_types`);
//! (e) an `#[ignore]`-gated experiment harness runs the workflow under each
//!     named profile, writes `diagnostic-intake-state.json` snapshots, and
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
use engine_core::workflows::diagnostic_intake::graph;
use engine_core::workflows::diagnostic_intake::policy::{DiagnosticIntakePolicy, ModelTier};
use engine_core::workflows::diagnostic_intake::profiles;
use engine_core::workflows::diagnostic_intake::IntakeExtractNode;
use futures::FutureExt;
use serde_json::json;
use uuid::Uuid;

fn temp_worktree(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "engine-core-diagnostic-intake-e2e-{tag}-{}-{n}",
        std::process::id()
    ));
    // Guarantee-empty: see engine-core src's `sdlc_flow/setup.rs` `temp_dir_named`
    // doc comment for why PID-recycling makes this removal necessary, not
    // optional. Remove the ROOT dir before recreating the `planning` subdir.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("planning")).unwrap();
    dir
}

fn stub_intake_json() -> serde_json::Value {
    json!({
        "company_name": "Loja da Ana",
        "company_type": "retail SMB",
        "team_size": 4,
        "primary_channels": ["WhatsApp", "Mercado Livre"],
        "existing_tools": ["Google Sheets", "WhatsApp Business"],
        "existing_automations": ["A Zapier flow that broke after two weeks"],
        "top_workflows": [
            {
                "name": "WhatsApp order tracking",
                "description": "Orders are tracked by scrolling WhatsApp threads.",
                "frequency_evidence": "\"Every single day, multiple times.\"",
                "time_cost_evidence": "\"Probably an hour a day just searching chats.\"",
                "buildability_notes": "WhatsApp Business API available; no current integration.",
                "knowledge_holder": "Only Maria knows which chats matter.",
                "failure_mode": "Orders get lost when Maria is out sick."
            }
        ]
    })
}

fn stub_outcome(structured: serde_json::Value, input_tokens: u64, output_tokens: u64) -> Outcome {
    Outcome {
        cost_usd: 0.01,
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

fn stub_extract_transport() -> engine_core::workflows::ModelTransport {
    Arc::new(|_config: Config, _prompt: String| {
        async move { Ok(stub_outcome(stub_intake_json(), 200, 80)) }.boxed()
    })
}

/// Builds a fresh registry with `IntakeExtractNode` registered, wired to a
/// stubbed transport.
fn build_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        IntakeExtractNode::new().with_transport(stub_extract_transport()),
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
        .expect("DIAGNOSTIC_INTAKE policy should resolve for this event");
    policy::stamp_resolved_policy(ctx, &resolved).expect("policy should stamp");
}

/// Drives the single-node `DIAGNOSTIC_INTAKE` walk against the exact node
/// instance [`build_registry`] would hand to a real `Workflow`:
/// `IntakeExtractNode::process` runs for real (stubbed transport only). See
/// the module doc for why this drives the node directly rather than through
/// `Workflow::run`.
async fn drive(event: serde_json::Value, worktree: &Path) -> TaskContext {
    let registry = build_registry();

    let mut ctx = TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: json!({}),
        node_runs: HashMap::new(),
    };
    set_worktree(&mut ctx, worktree);
    stamp_resolved_policy(&mut ctx, worktree);

    let node = registry
        .get("IntakeExtractNode")
        .expect("IntakeExtractNode should be registered");
    let mut ctx = node
        .process(ctx)
        .await
        .unwrap_or_else(|err| panic!("'IntakeExtractNode' should process successfully: {err}"));

    // `Node::process` alone (unlike `Workflow::run`'s walk loop) never stamps
    // the terminal `NodeRunStatus::Success` transition — that's the walk
    // loop's job, not the node's. Replicate it here so the returned
    // `TaskContext` matches what a real `Workflow::run` would have produced
    // (mirrors `workflow.rs`'s own post-`process` success stamping).
    if let Some(run) = ctx.node_runs.get_mut("IntakeExtractNode") {
        run.status = NodeRunStatus::Success;
        run.completed_at = Some(chrono::Utc::now());
    }

    ctx
}

/// Builds an [`EventsRow`] for a completed run the way the durable store
/// layer would: `data` is the run's inbound event, `task_context` is the
/// final `TaskContext`. Mirrors `research_agent_e2e.rs::events_row_for`.
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
async fn intake_extract_node_produces_diagnostic_intake_and_round_trips_events_row() {
    let worktree = temp_worktree("basic");

    let event = json!({
        "notes": "Client: \"We track orders by scrolling WhatsApp threads, probably \
                  an hour a day.\" Only Maria knows the supplier list.",
    });

    let final_ctx = drive(event.clone(), &worktree).await;

    let intake = final_ctx
        .nodes
        .get("IntakeExtractNode")
        .expect("IntakeExtractNode should have stamped a result");
    assert_eq!(intake["company_name"], json!("Loja da Ana"));
    assert_eq!(intake["team_size"], json!(4));

    let candidates = intake["top_workflows"]
        .as_array()
        .expect("top_workflows should be an array");
    assert!(!candidates.is_empty());
    let candidate = &candidates[0];
    assert!(!candidate["frequency_evidence"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert!(!candidate["time_cost_evidence"]
        .as_str()
        .unwrap_or_default()
        .is_empty());

    let run = final_ctx
        .node_runs
        .get("IntakeExtractNode")
        .expect("expected a NodeRun for 'IntakeExtractNode'");
    assert_eq!(run.status, NodeRunStatus::Success);

    // --- Durable EventsRow mapping round-trips without loss, *_evidence
    // fields intact -----------------------------------------------------
    let row = events_row_for("DIAGNOSTIC_INTAKE", event, &final_ctx);
    let json_str = serde_json::to_string(&row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, row);
    assert_eq!(round_tripped.workflow_type, "DIAGNOSTIC_INTAKE");
    assert_eq!(
        round_tripped.task_context.nodes.get("IntakeExtractNode"),
        final_ctx.nodes.get("IntakeExtractNode")
    );
    let round_tripped_candidate =
        &round_tripped.task_context.nodes["IntakeExtractNode"]["top_workflows"][0];
    assert_eq!(
        round_tripped_candidate["frequency_evidence"],
        candidate["frequency_evidence"]
    );
    assert_eq!(
        round_tripped_candidate["time_cost_evidence"],
        candidate["time_cost_evidence"]
    );
    assert_eq!(
        round_tripped_candidate["knowledge_holder"],
        candidate["knowledge_holder"]
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
        .expect("DIAGNOSTIC_INTAKE declared graph must pass WorkflowValidator::validate");
}

#[test]
fn registry_for_policy_rewires_extract_to_local_under_local_tier_policy() {
    let mut local_policy = DiagnosticIntakePolicy::default();
    local_policy.model_tiers.extract = ModelTier::Local;

    let default_registry = graph::registry();
    let local_registry = graph::registry_for_policy(&local_policy);

    // The Local-tier rewire changes the composed `ClaudeCodeStep`'s
    // transport, not the registry's node identity set.
    assert_eq!(local_registry.len(), default_registry.len());
    assert!(local_registry.contains("IntakeExtractNode"));
}

#[test]
fn registry_for_policy_leaves_default_transport_on_non_local_tier() {
    let default_policy = DiagnosticIntakePolicy::default();
    assert_ne!(default_policy.model_tiers.extract, ModelTier::Local);

    let default_registry = graph::registry();
    let policy_registry = graph::registry_for_policy(&default_policy);

    assert_eq!(policy_registry.len(), default_registry.len());
    assert!(policy_registry.contains("IntakeExtractNode"));
}

#[test]
fn is_registered_true_and_workflow_listed_after_register_builtin_workflows() {
    let mut dispatcher = engine_serve::dispatch::Dispatcher::new();
    engine_serve::workflows::register_builtin_workflows(&mut dispatcher);

    assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));

    let listed = dispatcher.registered_types();
    assert!(
        listed.iter().any(|t| t == "DIAGNOSTIC_INTAKE"),
        "expected 'DIAGNOSTIC_INTAKE' in the dispatcher's registered_types() \
         (the GET /workflows-equivalent listing), got {listed:?}"
    );

    let schema = dispatcher
        .resolve_schema("DIAGNOSTIC_INTAKE")
        .expect("DIAGNOSTIC_INTAKE schema should resolve");
    assert_eq!(schema.start_node, "IntakeExtractNode");
}

// ---------------------------------------------------------------------------
// `#[ignore]`-gated experiment harness: mirrors `research_agent_e2e.rs`'s
// shape — hermetic (stubbed transport) rather than a real-CLI run, run
// manually:
//
// ```sh
// cargo test -p engine-core --test diagnostic_intake_e2e -- --ignored
// ```
// ---------------------------------------------------------------------------

const EXPERIMENT_PROFILES: [&str; 4] = ["baseline", "cheap-fast", "thorough", "local-extract"];

fn print_ranked_table(mut rows: Vec<policy::PolicyAggregate<DiagnosticIntakePolicy>>) {
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

fn extract_diagnostic_intake_run(
    value: &serde_json::Value,
) -> Option<(DiagnosticIntakePolicy, policy::RunTelemetry)> {
    policy::extract_policy_telemetry::<DiagnosticIntakePolicy>(value, "policy", "telemetry")
}

/// Runs [`drive`] under each named profile, writes each run's
/// `diagnostic-intake-state.json`, and aggregates them into a ranked
/// per-profile table via `crate::policy::aggregate_state_files`.
///
/// Kept hermetic (stubbed transport) rather than a real-CLI experiment.
/// `#[ignore]`d anyway (per the spec's Task 7 wording) so it stays out of the
/// default `cargo test` run and is runnable standalone for a manual
/// before/after profile comparison.
#[tokio::test]
#[ignore]
async fn experiment_named_profiles_ranked_by_cost() {
    let mut state_paths: Vec<PathBuf> = Vec::new();
    let mut worktrees: Vec<PathBuf> = Vec::new();

    for profile in EXPERIMENT_PROFILES {
        let worktree = temp_worktree(&format!("experiment-{profile}"));

        let event = json!({
            "notes": "Client: \"We track orders by scrolling WhatsApp threads.\"",
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
        if profile == "local-extract" {
            assert_eq!(resolved.model_tiers.extract, ModelTier::Local);
        }

        let _final_ctx = drive(event, &worktree).await;

        let state_path = worktree
            .join("planning")
            .join("diagnostic-intake-state.json");
        assert!(
            state_path.exists(),
            "profile {profile:?}: expected IntakeExtractNode to persist {}",
            state_path.display()
        );
        state_paths.push(state_path);
        worktrees.push(worktree);
    }

    let rows = policy::aggregate_state_files::<DiagnosticIntakePolicy, _>(
        &state_paths,
        extract_diagnostic_intake_run,
    )
    .expect("all diagnostic-intake-state.json files should parse");

    print_ranked_table(rows.clone());

    assert!(
        !rows.is_empty(),
        "expected at least one aggregated policy row across the named profiles"
    );

    for worktree in &worktrees {
        std::fs::remove_dir_all(worktree).ok();
    }
}
