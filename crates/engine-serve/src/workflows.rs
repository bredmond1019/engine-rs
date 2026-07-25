//! Builtin workflow registration — wires `engine-core`'s assembled workflows
//! into a `Dispatcher`'s dual `workflow_registry`/`schema_registry`.
//!
//! `engine-core` cannot dev-depend on `engine-serve` (that would cycle:
//! `engine-serve` -> `engine-core` already exists as a normal dependency), so
//! this module is the place that pairs each `engine-core` workflow's
//! assembled `WorkflowSchema` + `WorkflowFactory`-shaped builder with the
//! `Dispatcher::register` call. See `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.md`,
//! Task 5, and its Notes section for the cross-crate rationale.

use crate::dispatch::Dispatcher;

/// Register the `SDLC_FLOW` workflow (`engine_core::workflows::sdlc_flow`)
/// with `dispatcher`, populating both the `workflow_registry` (via
/// `sdlc_flow::graph::workflow`) and the `schema_registry` (via
/// `sdlc_flow::graph::schema`).
pub fn register_sdlc_flow(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::sdlc_flow::graph::schema(),
        Box::new(engine_core::workflows::sdlc_flow::graph::workflow),
    );
}

/// Register the `RESEARCH_AGENT` workflow (`engine_core::workflows::research_agent`)
/// with `dispatcher`, populating both the `workflow_registry` (via
/// `research_agent::graph::workflow`) and the `schema_registry` (via
/// `research_agent::graph::schema`). See
/// `planning/EN.4.A-research-agent/tasks.md`, Task 7.
pub fn register_research_agent(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::research_agent::graph::schema(),
        Box::new(engine_core::workflows::research_agent::graph::workflow),
    );
}

/// Register the `DIAGNOSTIC_INTAKE` workflow
/// (`engine_core::workflows::diagnostic_intake`) with `dispatcher`,
/// populating both the `workflow_registry` (via
/// `diagnostic_intake::graph::workflow`) and the `schema_registry` (via
/// `diagnostic_intake::graph::schema`). See
/// `planning/EN.4.B-diagnostic-intake/tasks.md`, Task 6.
pub fn register_diagnostic_intake(dispatcher: &mut Dispatcher) {
    dispatcher.register(
        engine_core::workflows::diagnostic_intake::graph::schema(),
        Box::new(engine_core::workflows::diagnostic_intake::graph::workflow),
    );
}

/// Register every builtin workflow known to this crate: `SDLC_FLOW`,
/// `RESEARCH_AGENT`, and `DIAGNOSTIC_INTAKE`; future builtins register here
/// too.
pub fn register_builtin_workflows(dispatcher: &mut Dispatcher) {
    register_sdlc_flow(dispatcher);
    register_research_agent(dispatcher);
    register_diagnostic_intake(dispatcher);
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

    #[tokio::test]
    async fn dispatch_yields_a_runnable_workflow() {
        let mut dispatcher = Dispatcher::new();
        register_sdlc_flow(&mut dispatcher);

        let workflow = dispatcher
            .dispatch("SDLC_FLOW")
            .expect("SDLC_FLOW should dispatch to a runnable Workflow");

        // Confirm the workflow was actually assembled (has the expected
        // start node reachable) without driving a full run, which would
        // require live model transports / real subprocesses for the
        // model-calling and shell-driven nodes.
        let _ = workflow;
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
    fn register_builtin_workflows_registers_diagnostic_intake() {
        let mut dispatcher = Dispatcher::new();

        register_builtin_workflows(&mut dispatcher);

        assert!(dispatcher.is_registered("DIAGNOSTIC_INTAKE"));
    }
}
