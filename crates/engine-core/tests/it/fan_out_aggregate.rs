//! `EN.6.G` task 3 — end-to-end integration test driving `FanOutNode` and
//! `AggregateNode` (task 1) through a real `Workflow` run, rather than
//! hand-calling the two nodes in sequence the way `nodes::fan_out`'s and
//! `nodes::aggregate`'s own unit tests do.
//!
//! Builds a small linear graph — `FanOut -> Aggregate -> PersistStub` —
//! where `FanOut` expands one incoming context into N concurrent identical
//! `SourceNode` instances (proving no last-write-wins collision survives a
//! real `Workflow::run`, not just a hand-called `.process()`), `Aggregate`
//! joins their N distinct results into one deterministically-ordered array,
//! and `PersistStub` (standing in for the real
//! `workflows::content_pipeline::PersistToBrainNode` — this block does not
//! touch that node) reads the joined array and stamps exactly one merged
//! digest payload.

use std::collections::HashMap;

use engine_contract::TaskContext;
use engine_core::node::{Node, NodeError, NodeRegistry};
use engine_core::nodes::aggregate::AggregateNode;
use engine_core::nodes::fan_out::FanOutNode;
use engine_core::schema::{NodeConfig, WorkflowSchema};
use engine_core::workflow::Workflow;
use serde_json::{json, Value};

const WORKFLOW_TYPE: &str = "FAN_OUT_AGGREGATE_FIXTURE";
const SOURCE_COUNT: usize = 3;

/// A trivial source node — mirrors `nodes::fan_out::tests::SourceNode` —
/// deliberately stamping `ctx.nodes` under its own default `name()` so a
/// real `Workflow::run` (not a hand-called `.process()`) is the thing
/// proving `with_identity` prevents the last-write-wins collision.
struct SourceNode {
    value: Value,
}

#[async_trait::async_trait]
impl Node for SourceNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes
            .insert(self.name().to_string(), self.value.clone());
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "SourceNode"
    }
}

/// Stands in for `workflows::content_pipeline::PersistToBrainNode` — reads
/// the upstream `AggregateNode`'s joined array off `ctx.nodes["Aggregate"]`
/// and stamps exactly one merged digest payload, the shape a real persist
/// node would POST to Synapse's ingest endpoint (D51: no such POST happens
/// in this test — it is a stub).
struct PersistStubNode;

#[async_trait::async_trait]
impl Node for PersistStubNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let digest = ctx.nodes.get("Aggregate").cloned().ok_or_else(|| {
            NodeError::new("PersistStubNode: missing upstream 'Aggregate' result")
        })?;
        ctx.nodes.insert(
            self.name().to_string(),
            json!({ "posted": true, "digest": digest }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "PersistToBrainNode"
    }
}

fn fixture_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        "FanOut".to_string(),
        NodeConfig::new("FanOut", vec!["Aggregate".to_string()]),
    );
    nodes.insert(
        "Aggregate".to_string(),
        NodeConfig::new("Aggregate", vec!["PersistToBrainNode".to_string()]),
    );
    nodes.insert(
        "PersistToBrainNode".to_string(),
        NodeConfig::new("PersistToBrainNode", vec![]),
    );
    WorkflowSchema::new(WORKFLOW_TYPE, "FanOut", nodes)
}

fn fixture_workflow() -> Workflow {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(FanOutNode::new(
        "FanOut",
        "Source",
        SOURCE_COUNT,
        |i| {
            Box::new(SourceNode {
                value: json!({ "i": i }),
            }) as Box<dyn Node>
        },
    )));
    registry.register(Box::new(AggregateNode::for_fan_out(
        "Aggregate",
        "Source",
        SOURCE_COUNT,
    )));
    registry.register(Box::new(PersistStubNode));

    Workflow::new_validated(registry, fixture_schema())
        .expect("fan-out/aggregate fixture graph should validate")
}

#[tokio::test]
async fn fan_out_to_aggregate_to_persist_produces_one_merged_digest_payload_for_n_sources() {
    let workflow = fixture_workflow();

    let ctx = workflow
        .run(json!({}), Box::new(|_| {}))
        .await
        .expect("fixture run should succeed");

    // (1) No last-write-wins collision survived a real `Workflow::run`: all
    // N distinct branch identities are present, and the shared type-name
    // key that the old (pre-`with_identity`) merge would have collided on
    // never appears.
    for i in 0..SOURCE_COUNT {
        let key = FanOutNode::branch_identity("Source", i);
        assert_eq!(ctx.nodes.get(&key), Some(&json!({ "i": i })));
    }
    assert!(!ctx.nodes.contains_key("SourceNode"));

    // (2) Exactly one merged digest payload, joining all N sources in
    // deterministic (index) order — not `HashMap` iteration order.
    let persisted = ctx
        .nodes
        .get("PersistToBrainNode")
        .expect("PersistToBrainNode stub should have run");
    assert_eq!(
        persisted,
        &json!({
            "posted": true,
            "digest": [{ "i": 0 }, { "i": 1 }, { "i": 2 }],
        })
    );

    // Every declared node in the graph ran successfully.
    for name in ["FanOut", "Aggregate", "PersistToBrainNode"] {
        assert_eq!(
            ctx.node_runs.get(name).map(|r| r.status),
            Some(engine_contract::NodeRunStatus::Success),
            "node {name} should have run to success"
        );
    }
}
