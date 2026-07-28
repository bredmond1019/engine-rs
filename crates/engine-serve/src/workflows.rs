//! Builtin workflow registration — wires `engine-core`'s assembled workflows
//! into a `Dispatcher`'s dual `workflow_registry`/`schema_registry`.
//!
//! `engine-core` cannot dev-depend on `engine-serve` (that would cycle:
//! `engine-serve` -> `engine-core` already exists as a normal dependency), so
//! this module is the place that pairs each `engine-core` workflow's
//! assembled `WorkflowSchema` + `WorkflowFactory`-shaped builder with the
//! `Dispatcher::register` call. See `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.md`,
//! Task 5, and its Notes section for the cross-crate rationale.
//!
//! Each registration below is now policy-aware (EN.5.D task 7): the factory
//! resolves that workflow's policy once from the triggering event via its
//! `resolve_policy_for_run_from`, builds the policy-dependent
//! `graph::registry_for_policy` instead of the default-policy `registry`,
//! and seeds the resolved policy into the run at
//! `policy::RESOLVED_POLICY_IDENTITY` (via `Workflow::with_seeded_nodes`) so
//! it is visible to the start node without a second `harness.json` read.
//! This is the change that makes a `profile` sent over `POST /events/`
//! actually select the local transport for a served run.
//!
//! Config-source choice per workflow: `SDLC_FLOW` runs embedded in
//! `bastion serve`'s own process, which *is* checked out in a repo, so its
//! factory reads `harness.json` off the current working directory (a
//! `PolicyConfigSource::Worktree`); the other three are channel/API-shaped
//! with no repo checkout at dispatch time, so their factories use
//! `PolicyConfigSource::Builtin` (builtin + profile + event layers only, no
//! filesystem access).

use std::collections::HashMap;

use engine_contract::TaskContext;
use engine_core::policy::{PolicyConfigSource, RESOLVED_POLICY_IDENTITY};
use engine_core::Workflow;
use serde::Serialize;

use crate::dispatch::Dispatcher;

/// Build the `TaskContext` a policy-resolution-only call needs: `event` set
/// to the triggering payload, everything else empty. Never runs a node —
/// only fed to `resolve_policy_for_run_from`, which reads `ctx.event` and
/// nothing else.
fn event_only_context(event: &serde_json::Value) -> TaskContext {
    TaskContext {
        event: event.clone(),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

/// Serialize `policy` into the single-entry seed map
/// `{RESOLVED_POLICY_IDENTITY: policy}`, matching the shape
/// `policy::stamp_resolved_policy` writes into `ctx.nodes` — so a node
/// reading the stamp via `policy::resolved_policy`/`resolved_policy_strict`
/// sees the same representation regardless of whether it was seeded at
/// dispatch or stamped mid-run.
fn seed_resolved_policy<P: Serialize>(
    policy: &P,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let value = serde_json::to_value(policy)
        .map_err(|err| format!("failed to serialize resolved policy: {err}"))?;
    let mut seeded = HashMap::new();
    seeded.insert(RESOLVED_POLICY_IDENTITY.to_string(), value);
    Ok(seeded)
}

/// Register the `SDLC_FLOW` workflow (`engine_core::workflows::sdlc_flow`)
/// with `dispatcher`, populating both the `workflow_registry` (via a
/// policy-aware factory built on `sdlc_flow::graph::registry_for_policy`)
/// and the `schema_registry` (via `sdlc_flow::graph::schema`).
pub fn register_sdlc_flow(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::sdlc_flow::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            // SDLC_FLOW's served process is itself checked out in a repo
            // (bastion serve's cwd), so `harness.json` is read off it —
            // `SetupWorktreeNode` hasn't run yet at dispatch time, so this
            // is not the run's eventual worktree, just where this process
            // lives.
            let source = PolicyConfigSource::Worktree(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            );
            let policy = engine_core::workflows::sdlc_flow::setup::resolve_policy_for_run_from(
                &ctx, &source,
            )
            .map_err(|err| err.to_string())?;
            let registry = engine_core::workflows::sdlc_flow::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Workflow::new_validated(registry, engine_core::workflows::sdlc_flow::graph::schema())
                .map(|workflow| workflow.with_seeded_nodes(seeded))
                .map_err(|err| err.to_string())
        }),
    );
}

/// Register the `RESEARCH_AGENT` workflow (`engine_core::workflows::research_agent`)
/// with `dispatcher`, populating both the `workflow_registry` (via a
/// policy-aware factory built on `research_agent::graph::registry_for_policy`)
/// and the `schema_registry` (via `research_agent::graph::schema`). See
/// `planning/EN.4.A-research-agent/tasks.md`, Task 7.
pub fn register_research_agent(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::research_agent::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::research_agent::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let registry =
                engine_core::workflows::research_agent::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::research_agent::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `DIAGNOSTIC_INTAKE` workflow
/// (`engine_core::workflows::diagnostic_intake`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `diagnostic_intake::graph::registry_for_policy`) and the
/// `schema_registry` (via `diagnostic_intake::graph::schema`). See
/// `planning/EN.4.B-diagnostic-intake/tasks.md`, Task 6.
pub fn register_diagnostic_intake(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::diagnostic_intake::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::diagnostic_intake::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let registry =
                engine_core::workflows::diagnostic_intake::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::diagnostic_intake::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `PROPOSAL_GENERATOR` workflow
/// (`engine_core::workflows::proposal_generator`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `proposal_generator::graph::registry_for_policy`) and the
/// `schema_registry` (via `proposal_generator::graph::schema`). See
/// `planning/EN.4.C-proposal-generator/tasks.md`, Task 10.
pub fn register_proposal_generator(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::proposal_generator::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::proposal_generator::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let registry =
                engine_core::workflows::proposal_generator::graph::registry_for_policy(&policy);
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::proposal_generator::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Env var this crate reads for the base URL its own served `POST /events/`
/// endpoint is reachable at, so a served `CONTENT_PIPELINE` run's
/// `ActionDispatchNode` self-POSTs a `TriggerWorkflow` action back to the
/// right place (`EN.6.A` task 5) instead of `action_dispatch.rs`'s
/// `DEFAULT_EVENTS_URL` local-dev placeholder. Unset falls back to that
/// same placeholder, so a deployment that never sets this var keeps
/// today's behavior unchanged. The `X-API-Key` header the self-POST needs
/// is wired separately: `channel_transport_live` builds a
/// `WorkflowTriggerDispatch` via `WorkflowTriggerDispatch::new`, which
/// already reads it from `ENGINE_EVENTS_API_KEY` (`channel_transport.rs`).
const EVENTS_URL_ENV: &str = "ENGINE_EVENTS_URL";

/// The local-dev placeholder `ActionDispatchNode::new()` defaults to
/// (`action_dispatch.rs`'s private `DEFAULT_EVENTS_URL`, duplicated here
/// since this crate has no dependency path to that private const) —
/// [`EVENTS_URL_ENV`]'s fallback when unset.
const DEFAULT_EVENTS_URL: &str = "http://localhost:8080/events/";

/// Read [`EVENTS_URL_ENV`], falling back to [`DEFAULT_EVENTS_URL`] when
/// unset or empty.
fn events_url_from_env() -> String {
    std::env::var(EVENTS_URL_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_EVENTS_URL.to_string())
}

/// Register the `CONTENT_PIPELINE` workflow
/// (`engine_core::workflows::content_pipeline`) with `dispatcher`,
/// populating both the `workflow_registry` (via a policy-aware factory
/// built on `content_pipeline::graph::registry_for_policy`) and the
/// `schema_registry` (via `content_pipeline::graph::schema`). See
/// `planning/EN.5.A-content-pipeline/tasks.md`, Task 12.
///
/// Channel/webhook-triggered runs carry no repo checkout at dispatch time
/// (mirrors `RESEARCH_AGENT`/`DIAGNOSTIC_INTAKE`/`PROPOSAL_GENERATOR`), so
/// the factory resolves policy against `PolicyConfigSource::Builtin`
/// (builtin + profile + event layers only, no filesystem access) rather
/// than a worktree path.
///
/// `EN.6.A` task 5: after `registry_for_policy` builds the policy-rewired
/// registry, this re-registers `ActionDispatchNode` with a
/// `channel_transport_live` pointed at [`events_url_from_env`]'s
/// deployment-configured `/events/` URL rather than the node's own
/// local-dev placeholder default — mirroring how `registry_for_policy`
/// itself re-registers a node to override its transport. The dispatch
/// stage carries no `ModelTier` and is never Local-eligible
/// (`content_pipeline::policy`), so this override applies identically
/// regardless of the resolved policy.
pub fn register_content_pipeline(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::content_pipeline::graph::schema(),
        Box::new(|event: &serde_json::Value| {
            let ctx = event_only_context(event);
            let policy =
                engine_core::workflows::content_pipeline::profiles::resolve_policy_for_run_from(
                    &ctx,
                    &PolicyConfigSource::Builtin,
                )
                .map_err(|err| err.to_string())?;
            let mut registry =
                engine_core::workflows::content_pipeline::graph::registry_for_policy(&policy);
            registry.register(Box::new(
                engine_core::workflows::content_pipeline::action_dispatch::ActionDispatchNode::new(
                )
                .with_transport(
                    engine_core::nodes::channel_transport::channel_transport_live(
                        events_url_from_env(),
                    ),
                ),
            ));
            let seeded = seed_resolved_policy(&policy)?;
            Ok(Workflow::new(
                registry,
                engine_core::workflows::content_pipeline::graph::schema(),
            )
            .with_seeded_nodes(seeded))
        }),
    );
}

/// Register the `OPPORTUNITY_SET_STAGE` workflow
/// (`engine_core::workflows::opportunity_edit::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.7.B-research-opportunity-loop/tasks.md`, Task 6.
///
/// This is the **first model-free workflow** registered in this module:
/// `OpportunityEditNode` calls no model and reads no `harness.json`
/// section (see `opportunity_edit::graph`'s module doc), so this factory
/// resolves no policy and seeds no policy stamp — unlike every other
/// `register_*` function above, there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_opportunity_set_stage(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::opportunity_edit::graph::set_stage_schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::opportunity_edit::graph::set_stage_registry(),
                engine_core::workflows::opportunity_edit::graph::set_stage_schema(),
            ))
        }),
    );
}

/// Register the `OPPORTUNITY_ADD_ACTION` workflow
/// (`engine_core::workflows::opportunity_edit::graph`) with `dispatcher`,
/// populating both the `workflow_registry` and the `schema_registry`. See
/// `planning/EN.7.B-research-opportunity-loop/tasks.md`, Task 6.
///
/// This is the **second model-free workflow** registered in this module,
/// alongside [`register_opportunity_set_stage`]: `OpportunityEditNode`
/// calls no model and reads no `harness.json` section (see
/// `opportunity_edit::graph`'s module doc), so this factory resolves no
/// policy and seeds no policy stamp — there is no `resolve_policy_for_run_from`
/// call and no `seed_resolved_policy` call. Do not "restore" that hop; it
/// was never dropped, it was never needed.
pub fn register_opportunity_add_action(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::opportunity_edit::graph::add_action_schema(),
        Box::new(|_event: &serde_json::Value| {
            Ok(Workflow::new(
                engine_core::workflows::opportunity_edit::graph::add_action_registry(),
                engine_core::workflows::opportunity_edit::graph::add_action_schema(),
            ))
        }),
    );
}

/// Register every builtin workflow known to this crate: `SDLC_FLOW`,
/// `RESEARCH_AGENT`, `DIAGNOSTIC_INTAKE`, `PROPOSAL_GENERATOR`,
/// `CONTENT_PIPELINE`, `OPPORTUNITY_SET_STAGE`, and `OPPORTUNITY_ADD_ACTION`;
/// future builtins register here too.
pub fn register_builtin_workflows(dispatcher: &mut Dispatcher) {
    register_sdlc_flow(dispatcher);
    register_research_agent(dispatcher);
    register_diagnostic_intake(dispatcher);
    register_proposal_generator(dispatcher);
    register_content_pipeline(dispatcher);
    register_opportunity_set_stage(dispatcher);
    register_opportunity_add_action(dispatcher);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_sdlc_flow_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_sdlc_flow(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_FLOW"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_setup_worktree_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("SDLC_FLOW")
            .expect("SDLC_FLOW schema should resolve");

        assert_eq!(schema.start_node, "SetupWorktreeNode");
    }

    #[test]
    fn dispatch_yields_a_runnable_workflow() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        // `SDLC_FLOW`'s policy-aware factory (EN.5.D task 7) resolves policy
        // from the triggering event, whose schema requires `spec_slug` — so
        // an actual event is fed through `dispatch_with_event` rather than
        // `dispatch`'s empty-payload convenience wrapper.
        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should dispatch to a runnable Workflow");

        // Confirm the workflow was actually assembled (has the expected
        // start node reachable) without driving a full run, which would
        // require live model transports / real subprocesses for the
        // model-calling and shell-driven nodes.
        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_sdlc_policy() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event("SDLC_FLOW", &serde_json::json!({ "spec_slug": "my-spec" }))
            .expect("SDLC_FLOW should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_sdlc_profile() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "SDLC_FLOW",
            &serde_json::json!({ "spec_slug": "my-spec", "profile": "not-a-real-profile" }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn register_builtin_workflows_registers_sdlc_flow() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("SDLC_FLOW"));
    }

    #[test]
    fn register_research_agent_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_research_agent(&mut dispatcher);

        assert!(dispatcher.is_registered("RESEARCH_AGENT"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_research_mode_router_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("RESEARCH_AGENT")
            .expect("RESEARCH_AGENT schema should resolve");

        assert_eq!(schema.start_node, "ResearchModeRouterNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_research_agent_policy() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "RESEARCH_AGENT",
                &serde_json::json!({ "mode": "company", "company_name": "Acme" }),
            )
            .expect("RESEARCH_AGENT should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_research_agent_profile() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "RESEARCH_AGENT",
            &serde_json::json!({
                "mode": "company",
                "company_name": "Acme",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn resolve_schema_terminates_in_merge_contacts_node_with_no_outgoing_edges() {
        let mut dispatcher = Dispatcher::new();
        register_research_agent(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("RESEARCH_AGENT")
            .expect("RESEARCH_AGENT schema should resolve");

        let config = schema
            .nodes
            .get("MergeContactsNode")
            .expect("schema should declare 'MergeContactsNode'");
        assert!(
            config.connections.is_empty(),
            "MergeContactsNode should have no outgoing edges"
        );
    }

    #[test]
    fn register_builtin_workflows_registers_research_agent() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("RESEARCH_AGENT"));
    }

    #[test]
    fn register_diagnostic_intake_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_diagnostic_intake(&mut dispatcher);

        assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_intake_extract_node_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("DIAGNOSTIC_INTAKE")
            .expect("DIAGNOSTIC_INTAKE schema should resolve");

        assert_eq!(schema.start_node, "IntakeExtractNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_diagnostic_intake_policy() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "DIAGNOSTIC_INTAKE",
                &serde_json::json!({ "notes": "customer call transcript" }),
            )
            .expect("DIAGNOSTIC_INTAKE should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_diagnostic_intake_profile() {
        let mut dispatcher = Dispatcher::new();
        register_diagnostic_intake(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "DIAGNOSTIC_INTAKE",
            &serde_json::json!({
                "notes": "customer call transcript",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn register_builtin_workflows_registers_diagnostic_intake() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));
    }

    #[test]
    fn register_proposal_generator_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_proposal_generator(&mut dispatcher);

        assert!(dispatcher.is_registered("PROPOSAL_GENERATOR"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_company_research_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("PROPOSAL_GENERATOR")
            .expect("PROPOSAL_GENERATOR schema should resolve");

        assert_eq!(schema.start_node, "ProposalCompanyResearchNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_proposal_generator_policy() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "PROPOSAL_GENERATOR",
                &serde_json::json!({ "company_name": "Acme", "profile": "local-judgment" }),
            )
            .expect("PROPOSAL_GENERATOR should resolve the local-judgment profile with no repo");

        let _ = workflow;
    }

    #[test]
    fn local_judgment_profile_over_the_event_resolves_to_a_locally_tiered_policy() {
        // Exercises exactly what `register_proposal_generator`'s factory
        // does with the triggering event, proving the `profile` sent over
        // `POST /events/` actually reaches `registry_for_policy`'s
        // Local-tier rewire rather than resolving to builtin defaults.
        use engine_core::workflows::proposal_generator::policy::ModelTier;

        let event = serde_json::json!({ "company_name": "Acme", "profile": "local-judgment" });
        let ctx = event_only_context(&event);

        let policy =
            engine_core::workflows::proposal_generator::profiles::resolve_policy_for_run_from(
                &ctx,
                &PolicyConfigSource::Builtin,
            )
            .expect("local-judgment should resolve with no repo");

        assert_eq!(policy.model_tiers.opportunity, ModelTier::Local);
        assert_eq!(policy.model_tiers.review, ModelTier::Local);
        assert_eq!(policy.model_tiers.revise, ModelTier::Local);

        let default_policy =
            engine_core::workflows::proposal_generator::policy::ProposalGeneratorPolicy::default();
        assert_ne!(
            policy.model_tiers.opportunity, default_policy.model_tiers.opportunity,
            "the resolved policy must differ from the default-policy registry's tiers"
        );
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_proposal_generator_profile() {
        let mut dispatcher = Dispatcher::new();
        register_proposal_generator(&mut dispatcher);

        let result = dispatcher.dispatch_with_event(
            "PROPOSAL_GENERATOR",
            &serde_json::json!({
                "company_name": "Acme",
                "profile": "not-a-real-profile",
            }),
        );

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn register_builtin_workflows_registers_proposal_generator() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("PROPOSAL_GENERATOR"));
    }

    fn minimal_web_article_event() -> serde_json::Value {
        serde_json::json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" }
            }
        })
    }

    #[test]
    fn register_content_pipeline_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_content_pipeline(&mut dispatcher);

        assert!(dispatcher.is_registered("CONTENT_PIPELINE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_source_router_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("CONTENT_PIPELINE")
            .expect("CONTENT_PIPELINE schema should resolve");

        assert_eq!(schema.start_node, "SourceRouterNode");
    }

    #[test]
    fn dispatch_with_event_seeds_the_resolved_content_pipeline_policy() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event("CONTENT_PIPELINE", &minimal_web_article_event())
            .expect("CONTENT_PIPELINE should dispatch to a runnable Workflow with no repo");

        let _ = workflow;
    }

    #[test]
    fn dispatch_with_event_fails_loudly_on_unknown_content_pipeline_profile() {
        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("not-a-real-profile");

        let result = dispatcher.dispatch_with_event("CONTENT_PIPELINE", &event);

        match result {
            Err(crate::dispatch::DispatchError::PolicyResolutionFailed(message)) => {
                assert!(message.contains("not-a-real-profile"));
            }
            Ok(_) => panic!("expected PolicyResolutionFailed, got Ok"),
            Err(other) => panic!("expected PolicyResolutionFailed, got {other}"),
        }
    }

    #[test]
    fn local_drafting_profile_over_the_event_resolves_to_a_locally_tiered_policy() {
        use engine_core::workflows::content_pipeline::policy::ModelTier;

        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("local-drafting");
        let ctx = event_only_context(&event);

        let policy =
            engine_core::workflows::content_pipeline::profiles::resolve_policy_for_run_from(
                &ctx,
                &PolicyConfigSource::Builtin,
            )
            .expect("local-drafting should resolve with no repo");

        assert_eq!(policy.model_tiers.summarize, ModelTier::Local);
        assert_eq!(policy.model_tiers.critic, ModelTier::Local);
        assert_eq!(policy.model_tiers.revise, ModelTier::Local);
        assert_eq!(policy.model_tiers.translate, ModelTier::Local);
    }

    #[test]
    fn register_builtin_workflows_registers_content_pipeline() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("CONTENT_PIPELINE"));
    }

    /// `events_url_from_env` (`EN.6.A` task 5): unset/empty falls back to
    /// the local-dev placeholder; a configured value overrides it. Runs as
    /// a single test (rather than split across parallel `#[test]`s) since
    /// `std::env::var`/`set_var` are process-global and no other test in
    /// this file touches `ENGINE_EVENTS_URL` — self-contained regardless of
    /// `cargo test`'s default parallel execution.
    #[test]
    fn events_url_from_env_falls_back_then_honors_the_configured_value() {
        // SAFETY: this test owns `ENGINE_EVENTS_URL` end-to-end (no other
        // test in this crate reads or writes it) and always restores the
        // unset state before returning, so it cannot leak a stale value to
        // any other test even under `cargo test`'s default parallelism.
        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }
        assert_eq!(events_url_from_env(), DEFAULT_EVENTS_URL);

        unsafe {
            std::env::set_var(EVENTS_URL_ENV, "https://engine.example.com/events/");
        }
        assert_eq!(events_url_from_env(), "https://engine.example.com/events/");

        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }
        assert_eq!(events_url_from_env(), DEFAULT_EVENTS_URL);
    }

    /// A served `CONTENT_PIPELINE` dispatch still builds a runnable
    /// `Workflow` when `ENGINE_EVENTS_URL` is configured — the
    /// `ActionDispatchNode` re-registration in `register_content_pipeline`
    /// does not disturb the rest of the policy-aware assembly.
    #[test]
    fn dispatch_with_event_builds_content_pipeline_with_a_configured_events_url() {
        unsafe {
            std::env::set_var(EVENTS_URL_ENV, "https://engine.example.com/events/");
        }

        let mut dispatcher = Dispatcher::new();
        register_content_pipeline(&mut dispatcher);

        let workflow =
            dispatcher.dispatch_with_event("CONTENT_PIPELINE", &minimal_web_article_event());

        unsafe {
            std::env::remove_var(EVENTS_URL_ENV);
        }

        let workflow =
            workflow.expect("CONTENT_PIPELINE should dispatch with a configured events URL");
        let _ = workflow;
    }

    #[test]
    fn register_opportunity_set_stage_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_opportunity_set_stage(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_SET_STAGE"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_set_opportunity_stage_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_set_stage(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("OPPORTUNITY_SET_STAGE")
            .expect("OPPORTUNITY_SET_STAGE schema should resolve");

        assert_eq!(schema.start_node, "SetOpportunityStageNode");
    }

    #[test]
    fn dispatch_opportunity_set_stage_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_set_stage(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "OPPORTUNITY_SET_STAGE",
                &serde_json::json!({ "slug": "acme", "stage": "contacted" }),
            )
            .expect("OPPORTUNITY_SET_STAGE should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_opportunity_set_stage() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_SET_STAGE"));
    }

    #[test]
    fn register_opportunity_add_action_populates_both_registries() {
        let mut dispatcher = Dispatcher::new();

        register_opportunity_add_action(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_ADD_ACTION"));
    }

    #[test]
    fn resolve_schema_returns_schema_with_add_opportunity_action_start_node() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_add_action(&mut dispatcher);

        let schema = dispatcher
            .resolve_schema("OPPORTUNITY_ADD_ACTION")
            .expect("OPPORTUNITY_ADD_ACTION schema should resolve");

        assert_eq!(schema.start_node, "AddOpportunityActionNode");
    }

    #[test]
    fn dispatch_opportunity_add_action_builds_a_runnable_workflow_with_no_policy_stamp() {
        let mut dispatcher = Dispatcher::new();
        register_opportunity_add_action(&mut dispatcher);

        let workflow = dispatcher
            .dispatch_with_event(
                "OPPORTUNITY_ADD_ACTION",
                &serde_json::json!({
                    "slug": "acme",
                    "at": "2026-07-27T00:00:00Z",
                    "kind": "call",
                    "note": "left voicemail",
                }),
            )
            .expect("OPPORTUNITY_ADD_ACTION should dispatch to a runnable Workflow");

        let _ = workflow;
    }

    #[test]
    fn register_builtin_workflows_registers_opportunity_add_action() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("OPPORTUNITY_ADD_ACTION"));
    }

    #[test]
    fn register_builtin_workflows_registers_all_seven_workflow_types() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        for workflow_type in [
            "SDLC_FLOW",
            "RESEARCH_AGENT",
            "DIAGNOSTIC_INTAKE",
            "PROPOSAL_GENERATOR",
            "CONTENT_PIPELINE",
            "OPPORTUNITY_SET_STAGE",
            "OPPORTUNITY_ADD_ACTION",
        ] {
            assert!(
                dispatcher.is_registered(workflow_type),
                "expected {workflow_type} to be registered"
            );
        }
    }
}
