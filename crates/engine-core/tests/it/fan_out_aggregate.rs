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
use engine_core::node::{Node, NodeError, NodeExt, NodeRegistry};
use engine_core::nodes::aggregate::{AggregateNode, MissingSource};
use engine_core::nodes::fan_out::FanOutNode;
use engine_core::parallel::{BranchFailure, ParallelNode};
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

// -- EN.ticket.parallel-node-partial-success task 4 ------------------------
//
// End-to-end coverage that `ParallelNode::BranchFailure::Tolerate` and
// `AggregateNode::MissingSource::Skip` are a matched pair through a real
// `Workflow::run` (not a hand-called `.process()` chain): a fan-out with one
// failing branch feeding a `Skip` aggregate completes with the survivors in
// declared order, while the same fan-out feeding the default `Fail`
// aggregate still fails the run — pinning that a `Tolerate` fan-out paired
// with a `Fail` aggregate reintroduces the original bug one node later.

const PARTIAL_SOURCE_COUNT: usize = 3;
/// The branch index that always fails in the partial-success fixtures below.
const FAILING_BRANCH_INDEX: usize = 1;

/// Always fails — stands in for a flaky branch in the `Tolerate` fixtures.
struct FailingSourceNode;

#[async_trait::async_trait]
impl Node for FailingSourceNode {
    async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Err(NodeError::new("simulated branch failure"))
    }

    fn name(&self) -> &str {
        "FailingSourceNode"
    }
}

/// Builds the `PARTIAL_SOURCE_COUNT` branches `FanOutNode` would have built
/// for `"Source"`, except branch [`FAILING_BRANCH_INDEX`] always fails —
/// each wrapped under exactly the identity `FanOutNode::branch_identity`
/// would assign, so `AggregateNode::for_fan_out("Aggregate", "Source", ..)`
/// reads the same keys a real `FanOutNode` would have produced.
fn partial_failure_branches() -> Vec<Box<dyn Node>> {
    (0..PARTIAL_SOURCE_COUNT)
        .map(|i| {
            let identity = FanOutNode::branch_identity("Source", i);
            if i == FAILING_BRANCH_INDEX {
                Box::new((Box::new(FailingSourceNode) as Box<dyn Node>).with_identity(identity))
                    as Box<dyn Node>
            } else {
                let instance = Box::new(SourceNode {
                    value: json!({ "i": i }),
                }) as Box<dyn Node>;
                Box::new(instance.with_identity(identity)) as Box<dyn Node>
            }
        })
        .collect()
}

/// A `FanOut -> Aggregate` fixture (no persist stage — this fixture is only
/// exercising the `ParallelNode`/`AggregateNode` pairing) where `FanOut` is a
/// `ParallelNode` running under `BranchFailure::Tolerate` with one failing
/// branch, and `Aggregate` runs under the given `missing_source` mode.
fn partial_failure_workflow(missing_source: MissingSource) -> Workflow {
    let mut nodes = HashMap::new();
    nodes.insert(
        "FanOut".to_string(),
        NodeConfig::new("FanOut", vec!["Aggregate".to_string()]),
    );
    nodes.insert(
        "Aggregate".to_string(),
        NodeConfig::new("Aggregate", vec![]),
    );
    let schema = WorkflowSchema::new("FAN_OUT_AGGREGATE_PARTIAL_FIXTURE", "FanOut", nodes);

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        ParallelNode::new("FanOut", partial_failure_branches())
            .with_branch_failure(BranchFailure::Tolerate),
    ));
    registry.register(Box::new(
        AggregateNode::for_fan_out("Aggregate", "Source", PARTIAL_SOURCE_COUNT)
            .with_missing_source(missing_source),
    ));

    Workflow::new_validated(registry, schema)
        .expect("partial-failure fixture graph should validate")
}

#[tokio::test]
async fn tolerate_fan_out_feeding_skip_aggregate_completes_with_survivors_in_declared_order() {
    let workflow = partial_failure_workflow(MissingSource::Skip);

    let ctx = workflow
        .run(json!({}), Box::new(|_| {}))
        .await
        .expect("Tolerate fan-out + Skip aggregate should complete the run");

    // The failing branch's identity never lands in ctx.nodes...
    let failing_identity = FanOutNode::branch_identity("Source", FAILING_BRANCH_INDEX);
    assert!(!ctx.nodes.contains_key(&failing_identity));

    // ...and the surviving branches arrive in declared order, not
    // `HashMap` iteration order, with the missing middle entry omitted
    // rather than reindexed.
    assert_eq!(
        ctx.nodes.get("Aggregate"),
        Some(&json!([{ "i": 0 }, { "i": 2 }]))
    );
}

#[tokio::test]
async fn tolerate_fan_out_feeding_fail_aggregate_still_fails_the_run() {
    // The mismatched pairing: a Tolerate fan-out's missing branch key is
    // exactly what a default-Fail aggregate hard-errors on, reintroducing
    // the original bug one node later. This must still fail, deliberately.
    //
    // `Workflow::walk` records a failed node's status on `ctx.node_runs`
    // rather than surfacing it as an `Err` from `Workflow::run` (see the
    // existing per-node `NodeRunStatus::Success` assertions above) — so
    // "still fails" is asserted at that same node-run boundary, not via
    // `Workflow::run`'s own `Result`.
    let workflow = partial_failure_workflow(MissingSource::Fail);

    let ctx = workflow
        .run(json!({}), Box::new(|_| {}))
        .await
        .expect("Workflow::run itself still completes; the failure is node-level");

    assert_eq!(
        ctx.node_runs.get("Aggregate").map(|r| r.status),
        Some(engine_contract::NodeRunStatus::Failed),
        "Tolerate fan-out + Fail aggregate must still fail the Aggregate node"
    );
    // The run never reached a "Aggregate" output key on ctx.nodes — the
    // node's hard-error path never inserts one.
    assert!(!ctx.nodes.contains_key("Aggregate"));
}
