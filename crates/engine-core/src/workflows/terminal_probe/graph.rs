//! The declared `WorkflowSchema` / `NodeRegistry` / `Workflow` assembly for
//! `TERMINAL_PROBE` (`EN.9.D` task 5), following `harvest_approve/graph.rs`
//! verbatim.
//!
//! Declared graph shape — two nodes, no router:
//!
//! ```text
//! TerminalSessionNode -> TerminalObserveNode
//! ```
//!
//! `TerminalSessionNode` is the start node; `TerminalObserveNode` is
//! terminal (no forward connections). `TerminalObserveNode`'s
//! `session_input` is left unbound — it defaults to
//! `terminal::session::NODE_NAME`, exactly the identity this graph
//! registers `TerminalSessionNode` under, so the default read-preference
//! resolves correctly with no explicit binding needed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use term_core::driver::{TerminalDriver, TmuxDriver};

use crate::node::NodeRegistry;
use crate::nodes::terminal::{TerminalObserveNode, TerminalSessionNode};
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

/// The live-tmux driver timeout `registry()`'s default `TmuxDriver` uses.
/// Mirrors `term_core::driver`'s own `DEFAULT_TMUX_TIMEOUT` order of
/// magnitude; kept local so this module does not need to reach past
/// `term_core::driver`'s public surface for a private constant.
const DEFAULT_DRIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// `TERMINAL_PROBE`'s registered workflow type string.
pub const TERMINAL_PROBE_WORKFLOW_TYPE: &str = "TERMINAL_PROBE";

/// Build the declared `WorkflowSchema` for `TERMINAL_PROBE`:
/// `TerminalSessionNode` (start) -> `TerminalObserveNode` (terminal).
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        crate::nodes::terminal::session::NODE_NAME.to_string(),
        NodeConfig::new(
            crate::nodes::terminal::session::NODE_NAME,
            vec![crate::nodes::terminal::observe::NODE_NAME.to_string()],
        ),
    );
    nodes.insert(
        crate::nodes::terminal::observe::NODE_NAME.to_string(),
        NodeConfig::new(crate::nodes::terminal::observe::NODE_NAME, vec![]),
    );
    WorkflowSchema::new(
        TERMINAL_PROBE_WORKFLOW_TYPE,
        crate::nodes::terminal::session::NODE_NAME,
        nodes,
    )
}

/// Build a fresh `NodeRegistry` for `TERMINAL_PROBE` with the live
/// `TmuxDriver` seam — real `tmux` process invocations. Tests and
/// `engine-serve`'s registration use [`registry_with`] with a
/// `StubTerminalDriver`/shared driver instead.
#[must_use]
pub fn registry() -> NodeRegistry {
    let driver: Arc<dyn TerminalDriver> = Arc::new(TmuxDriver::new(DEFAULT_DRIVER_TIMEOUT));
    registry_with(driver)
}

/// Build a `NodeRegistry` for `TERMINAL_PROBE` with an injected
/// `TerminalDriver` seam, shared by both nodes — the single tmux server
/// both `TerminalSessionNode` and `TerminalObserveNode` must talk to.
#[must_use]
pub fn registry_with(driver: Arc<dyn TerminalDriver>) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TerminalSessionNode::new(driver.clone())));
    registry.register(Box::new(TerminalObserveNode::new(driver)));
    registry
}

/// Build the runnable `TERMINAL_PROBE` `Workflow`: [`registry`] paired with
/// [`schema`], constructed via `Workflow::new_validated` so assembly fails
/// loudly if the declared graph is not structurally sound.
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from.
#[must_use]
pub fn workflow() -> Workflow {
    Workflow::new_validated(registry(), schema())
        .expect("TERMINAL_PROBE declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::WorkflowValidator;
    use engine_contract::TaskContext;
    use term_core::driver::StubTerminalDriver;

    fn stub_registry() -> NodeRegistry {
        registry_with(Arc::new(StubTerminalDriver::new()))
    }

    #[test]
    fn schema_passes_validation() {
        let schema = schema();
        let registry = stub_registry();
        WorkflowValidator::validate(&registry, &schema).expect("declared graph should validate");
    }

    #[test]
    fn workflow_type_matches_schema() {
        assert_eq!(schema().workflow_type, TERMINAL_PROBE_WORKFLOW_TYPE);
    }

    #[test]
    fn start_node_is_terminal_session_node() {
        assert_eq!(
            schema().start_node,
            crate::nodes::terminal::session::NODE_NAME
        );
    }

    #[test]
    fn declared_graph_is_exactly_session_to_observe() {
        let schema = schema();
        assert_eq!(schema.nodes.len(), 2);

        let session_config = schema
            .nodes
            .get(crate::nodes::terminal::session::NODE_NAME)
            .expect("session node should be declared");
        assert_eq!(
            session_config.connections,
            vec![crate::nodes::terminal::observe::NODE_NAME.to_string()]
        );

        let observe_config = schema
            .nodes
            .get(crate::nodes::terminal::observe::NODE_NAME)
            .expect("observe node should be declared");
        assert!(
            observe_config.connections.is_empty(),
            "TerminalObserveNode should be terminal"
        );
    }

    #[test]
    fn registry_contains_exactly_the_two_expected_nodes() {
        let registry = stub_registry();
        assert!(registry.contains(crate::nodes::terminal::session::NODE_NAME));
        assert!(registry.contains(crate::nodes::terminal::observe::NODE_NAME));
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn workflow_builds_without_panicking() {
        let driver: Arc<dyn TerminalDriver> = Arc::new(StubTerminalDriver::new());
        let registry = registry_with(driver);
        let schema = schema();
        let _workflow = Workflow::new_validated(registry, schema)
            .expect("TERMINAL_PROBE declared graph must pass WorkflowValidator::validate");
    }

    /// Live-registry constructor smoke test: `registry()` (the
    /// `TmuxDriver`-backed default) builds without panicking and without
    /// spawning any tmux process (constructing a `TmuxDriver` is inert
    /// until a node actually calls it).
    #[test]
    fn live_registry_builds_without_panicking() {
        let registry = registry();
        assert_eq!(registry.len(), 2);
    }

    #[tokio::test]
    async fn end_to_end_run_against_a_stubbed_driver_reuses_session_on_reentry() {
        use term_core::driver::StubOutcome;

        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("idle prompt".to_string()));
        let registry = registry_with(driver.clone());
        let session_node = registry
            .get(crate::nodes::terminal::session::NODE_NAME)
            .expect("session node registered");
        let observe_node = registry
            .get(crate::nodes::terminal::observe::NODE_NAME)
            .expect("observe node registered");

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            + Duration::from_secs(3600).as_millis() as u64;
        let session_name = crate::nodes::terminal::identity::session_name_for(
            "probe-run-1",
            crate::nodes::terminal::session::NODE_NAME,
        );
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(format!(
                "probe-run-1:{session_name}:{}:{far_future}",
                crate::nodes::terminal::session::NODE_NAME
            )),
        );

        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.metadata["run_id"] = serde_json::json!("probe-run-1");

        let ctx = session_node.process(ctx).await.expect("session node ok");
        let ctx = observe_node.process(ctx).await.expect("observe node ok");

        assert!(ctx
            .nodes
            .get(crate::nodes::terminal::observe::NODE_NAME)
            .expect("observe result stamped")
            .get("state")
            .is_some());
    }
}
