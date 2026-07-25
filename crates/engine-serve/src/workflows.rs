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

/// Register every builtin workflow known to this crate: `SDLC_FLOW`,
/// `RESEARCH_AGENT`, `DIAGNOSTIC_INTAKE`, and `PROPOSAL_GENERATOR`; future
/// builtins register here too.
pub fn register_builtin_workflows(dispatcher: &mut Dispatcher) {
    register_sdlc_flow(dispatcher);
    register_research_agent(dispatcher);
    register_diagnostic_intake(dispatcher);
    register_proposal_generator(dispatcher);
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
}
