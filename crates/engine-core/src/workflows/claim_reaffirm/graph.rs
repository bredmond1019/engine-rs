//! Assembles the declared `WorkflowSchema` + `NodeRegistry` for the
//! `CLAIM_REAFFIRM` workflow (`EN.6.L` task 3).
//!
//! Declared graph shape:
//!
//! ```text
//! LoadClaimsNode -> ClaimQueueRouterNode -> { ClaimRecallNode -> JudgeClaimNode
//!                       -> SaveVerdictNode -> (loop) ClaimQueueRouterNode
//!                     | RenderReportNode }
//! ```
//!
//! The loop's back-edge (`SaveVerdictNode -> ClaimQueueRouterNode`) is a
//! plain declared connection, not a router edge — `ClaimQueueRouterNode`'s
//! own two out-edges (`ClaimRecallNode`/`RenderReportNode`) are the ones
//! skipped by `WorkflowValidator`'s cycle check (D42: a router's declared
//! connections are consulted only at runtime via `Router::route`, so a
//! cycle formed purely of router out-edges is not a static-graph defect).
//! `SaveVerdictNode -> ClaimQueueRouterNode` is a genuine non-router edge,
//! exactly the shape `sdlc_flow::task_loop`'s
//! `SaveStateNode -> TaskQueueRouterNode` back-edge takes (this workflow's
//! copied idiom, per the spec's Context Pointers) — `check_cycles`
//! (`validate.rs`) still walks it, but a `Router::route` result is never
//! itself a cycle-forming declared edge, so `Workflow::new_validated`
//! passes.
//!
//! No `ParallelNode` anywhere in this graph (OR.K3-inherited hard
//! constraint, load-bearing) — `queue-drain-no-parallel-node` below is a
//! standing regression guard, not merely documentation.

use std::collections::HashMap;

use crate::node::NodeRegistry;
use crate::nodes::brain_client::BrainConfig;
use crate::schema::{NodeConfig, WorkflowSchema};
use crate::workflow::Workflow;

use super::judge::{ClaimRecallNode, JudgeClaimNode};
use super::load_claims::LoadClaimsNode;
use super::queue_router::ClaimQueueRouterNode;
use super::render_report::RenderReportNode;
use super::save_verdict::SaveVerdictNode;

/// The `CLAIM_REAFFIRM` workflow's declared identity/type name, used both
/// to register the workflow (`crates/engine-serve/src/workflows.rs`) and
/// as `WorkflowSchema::workflow_type`.
pub const WORKFLOW_TYPE: &str = "CLAIM_REAFFIRM";

/// Build the declared `WorkflowSchema` for `CLAIM_REAFFIRM`. See this
/// module's doc comment for the full shape.
#[must_use]
pub fn schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();

    nodes.insert(
        "LoadClaimsNode".to_string(),
        NodeConfig::new("LoadClaimsNode", vec!["ClaimQueueRouterNode".to_string()]),
    );
    nodes.insert(
        "ClaimQueueRouterNode".to_string(),
        NodeConfig::new(
            "ClaimQueueRouterNode",
            vec![
                "ClaimRecallNode".to_string(),
                "RenderReportNode".to_string(),
            ],
        ),
    );
    nodes.insert(
        "ClaimRecallNode".to_string(),
        NodeConfig::new("ClaimRecallNode", vec!["JudgeClaimNode".to_string()]),
    );
    nodes.insert(
        "JudgeClaimNode".to_string(),
        NodeConfig::new("JudgeClaimNode", vec!["SaveVerdictNode".to_string()]),
    );
    nodes.insert(
        "SaveVerdictNode".to_string(),
        NodeConfig::new("SaveVerdictNode", vec!["ClaimQueueRouterNode".to_string()]),
    );
    nodes.insert(
        "RenderReportNode".to_string(),
        NodeConfig::new("RenderReportNode", vec![]),
    );

    WorkflowSchema::new(WORKFLOW_TYPE, "LoadClaimsNode", nodes)
}

/// Build a fresh `NodeRegistry` with every node identity in [`schema`]
/// registered, using `config` for [`ClaimRecallNode`]'s underlying
/// `RecallNode` (`EN.6.K`'s Brain seam) and every other node's default
/// (real-subprocess/real-filesystem) configuration. Tests build their own
/// registry with stubbed seams instead of calling this directly.
#[must_use]
pub fn registry(config: BrainConfig) -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(LoadClaimsNode::new()));
    registry.register(Box::new(ClaimQueueRouterNode::new()));
    registry.register(Box::new(ClaimRecallNode::new(config)));
    registry.register(Box::new(JudgeClaimNode::new()));
    registry.register(Box::new(SaveVerdictNode::new()));
    registry.register(Box::new(RenderReportNode::new()));
    registry
}

/// Build the fully-assembled, structurally-validated `CLAIM_REAFFIRM`
/// [`Workflow`] — [`registry`] + [`schema`], via `Workflow::new_validated`
/// so assembly itself proves the declared graph is sound (reachability +
/// cycle-freedom over non-router edges + no dangling identities).
///
/// # Panics
/// Panics if the declared graph fails `WorkflowValidator::validate` — this
/// would be a programming error in this module, not a runtime condition
/// callers should recover from (mirrors `recall::graph::workflow`).
#[must_use]
pub fn workflow(config: BrainConfig) -> Workflow {
    Workflow::new_validated(registry(config), schema())
        .expect("CLAIM_REAFFIRM declared graph must pass WorkflowValidator::validate")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BrainConfig {
        BrainConfig::new("http://localhost:8000", None)
    }

    #[test]
    fn schema_declares_every_node_identity() {
        let schema = schema();
        for identity in [
            "LoadClaimsNode",
            "ClaimQueueRouterNode",
            "ClaimRecallNode",
            "JudgeClaimNode",
            "SaveVerdictNode",
            "RenderReportNode",
        ] {
            assert!(
                schema.nodes.contains_key(identity),
                "schema missing declared node {identity}"
            );
        }
    }

    #[test]
    fn schema_start_node_is_load_claims() {
        assert_eq!(schema().start_node, "LoadClaimsNode");
    }

    #[test]
    fn queue_router_declares_both_router_targets() {
        let schema = schema();
        let router = schema.nodes.get("ClaimQueueRouterNode").expect("present");
        assert!(router.connections.contains(&"ClaimRecallNode".to_string()));
        assert!(router.connections.contains(&"RenderReportNode".to_string()));
    }

    #[test]
    fn loop_back_edge_is_declared() {
        let schema = schema();
        let save = schema.nodes.get("SaveVerdictNode").expect("present");
        assert_eq!(
            save.connections,
            vec!["ClaimQueueRouterNode".to_string()],
            "the queue-drain loop's back-edge"
        );
    }

    #[test]
    fn registry_registers_every_declared_node() {
        let registry = registry(test_config());
        for identity in [
            "LoadClaimsNode",
            "ClaimQueueRouterNode",
            "ClaimRecallNode",
            "JudgeClaimNode",
            "SaveVerdictNode",
            "RenderReportNode",
        ] {
            assert!(
                registry.get(identity).is_some(),
                "registry missing node {identity}"
            );
        }
    }

    #[test]
    fn workflow_new_validated_succeeds() {
        // `Workflow::new_validated` panics/errors internally if the graph
        // is structurally unsound — reaching this assertion at all proves
        // reachability, cycle-freedom (over non-router edges), and that
        // every declared connection resolves to a registered node.
        let _workflow = workflow(test_config());
    }

    #[test]
    fn no_parallel_node_anywhere_in_this_module_tree() {
        // Standing regression guard for the OR.K3-inherited hard
        // constraint (no `ParallelNode` — last-write-wins merge,
        // documented broken for N instances; serial queue-drain only).
        // Greps this workflow's own source files rather than the whole
        // crate, so it fails loudly and specifically to this workflow if
        // ever violated. Skips: comment lines (`//`-prefixed, including
        // this literal doc/discussion text — this very check would
        // otherwise trip on its own module doc comment and this test's own
        // source, both of which *name* the forbidden identifier while
        // discussing the constraint) and everything from `#[cfg(test)]`
        // onward (this test module itself, for the same reason).
        let forbidden_ident = ["Parallel", "Node"].concat();
        let this_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/workflows/claim_reaffirm");
        for entry in std::fs::read_dir(this_dir).expect("read claim_reaffirm dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).expect("read source file");
            let production_code = contents.split("#[cfg(test)]").next().unwrap_or(&contents);
            for line in production_code.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                assert!(
                    !line.contains(&forbidden_ident),
                    "{}: production code references {forbidden_ident} — OR.K3 forbids it \
                     for this workflow (last-write-wins merge, documented broken for N \
                     instances): {line:?}",
                    path.display()
                );
            }
        }
    }
}
