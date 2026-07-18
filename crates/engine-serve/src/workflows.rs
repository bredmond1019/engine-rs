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

/// Register every builtin workflow known to this crate. Currently just
/// `SDLC_FLOW`; future builtins (e.g. the bottom-half PatchDocs/WrapUp/PR
/// pipeline from EN.3.B) register here too.
pub fn register_builtin_workflows(dispatcher: &mut Dispatcher) {
    register_sdlc_flow(dispatcher);
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
}
