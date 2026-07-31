//! Fixture 3-node linear workflow integration test for the budget gate
//! (EN.2.B task 3).
//!
//! `UsageNode` reports `NodeRun.usage` the same way `ClaudeCodeStep` does
//! (setting it on its own `ctx.node_runs` entry inside `process`).
//! `Workflow::run_with` folds each completed node's usage into the
//! `BudgetLedger` and consults it *before* dispatching the next node, so a
//! low cap reached by `start_node`'s spend halts the walk before `node2`
//! runs. The same fixture with no `Budget` configured runs to completion
//! unchanged.

use std::collections::HashMap;

use engine_contract::{NodeRunStatus, TaskContext, Usage};
use engine_core::{
    Budget, Node, NodeConfig, NodeError, NodeRegistry, RunOptions, Workflow, WorkflowSchema,
    BUDGET_METADATA_KEY,
};

/// A node that stamps a marker into `TaskContext::nodes` and reports token
/// usage on its own `NodeRun`, mirroring `ClaudeCodeStep`'s pattern.
/// Optionally also stamps a `"cost_usd"` field into its own `ctx.nodes`
/// output, the same shape `ClaudeCodeStep` writes from `Outcome::cost_usd`
/// (EN.4.0 task 6) — `Workflow::run_with` reads it back out to fold into
/// the `BudgetLedger` alongside token usage.
struct UsageNode {
    identity: &'static str,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
}

#[async_trait::async_trait]
impl Node for UsageNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut output = serde_json::json!({ "ran": self.identity });
        if let Some(cost_usd) = self.cost_usd {
            output["cost_usd"] = serde_json::json!(cost_usd);
        }
        ctx.nodes.insert(self.identity.to_string(), output);

        let usage = Usage {
            input_tokens: Some(self.input_tokens),
            output_tokens: Some(self.output_tokens),
            model: "test-model".to_string(),
        };
        ctx.node_runs
            .entry(self.identity.to_string())
            .and_modify(|run| run.usage = Some(usage));

        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

/// start_node -> node2 -> node3 (terminal), wired via `connections[0]` only.
/// Each node reports 60 tokens of usage (120 after two nodes).
fn linear_schema() -> WorkflowSchema {
    let mut nodes = HashMap::new();
    nodes.insert(
        "start_node".to_string(),
        NodeConfig::new("start_node", vec!["node2".to_string()]),
    );
    nodes.insert(
        "node2".to_string(),
        NodeConfig::new("node2", vec!["node3".to_string()]),
    );
    nodes.insert("node3".to_string(), NodeConfig::new("node3", vec![]));

    WorkflowSchema::new("linear-3", "start_node", nodes)
}

fn usage_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(UsageNode {
        identity: "start_node",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: None,
    }));
    registry.register(Box::new(UsageNode {
        identity: "node2",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: None,
    }));
    registry.register(Box::new(UsageNode {
        identity: "node3",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: None,
    }));
    registry
}

/// Same linear-3 shape as [`usage_registry`], but each node also reports
/// `$3.00` of cost via its own `ctx.nodes["<identity>"]["cost_usd"]`.
fn cost_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(UsageNode {
        identity: "start_node",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: Some(3.0),
    }));
    registry.register(Box::new(UsageNode {
        identity: "node2",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: Some(3.0),
    }));
    registry.register(Box::new(UsageNode {
        identity: "node3",
        input_tokens: 30,
        output_tokens: 30,
        cost_usd: Some(3.0),
    }));
    registry
}

#[tokio::test]
async fn low_budget_halts_before_the_node_that_would_exceed_it() {
    let workflow = Workflow::new(usage_registry(), linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    // start_node reports 60 tokens; a cap of 60 is reached exactly after
    // start_node completes, so the walk halts before node2 dispatches.
    let budget = Budget {
        max_total_tokens: Some(60),
        max_cost_usd: None,
    };

    let result = workflow
        .run_with(
            serde_json::json!({}),
            on_progress,
            RunOptions {
                cancellation_token: None,
                budget: Some(budget),
                pause_signal: None,
                run_id: None,
            },
        )
        .await
        .expect("a budget-halted run returns Ok, not Err");

    // start_node ran to completion.
    let start_run = result
        .node_runs
        .get("start_node")
        .expect("start_node run present");
    assert_eq!(start_run.status, NodeRunStatus::Success);
    assert!(result.nodes.contains_key("start_node"));

    // node2/node3 never dispatched: still Pending, no output written.
    for identity in ["node2", "node3"] {
        let run = result
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("{identity} run present"));
        assert_eq!(run.status, NodeRunStatus::Pending, "{identity} status");
        assert!(!result.nodes.contains_key(identity), "{identity} output");
    }

    // The halt reason is recorded in TaskContext::metadata, naming the cap
    // that tripped.
    let budget_meta = &result.metadata[BUDGET_METADATA_KEY];
    assert_eq!(budget_meta["halted"], serde_json::json!(true));
    assert_eq!(
        budget_meta["reason"]["cap"],
        serde_json::json!("max_total_tokens")
    );
    assert_eq!(budget_meta["reason"]["spent"], serde_json::json!(60));
    assert_eq!(budget_meta["reason"]["limit"], serde_json::json!(60));
}

#[tokio::test]
async fn no_budget_configured_runs_to_completion_unchanged() {
    let workflow = Workflow::new(usage_registry(), linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run_with(serde_json::json!({}), on_progress, RunOptions::default())
        .await
        .expect("run should complete");

    for identity in ["start_node", "node2", "node3"] {
        let run = result.node_runs.get(identity).expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
        assert!(result.nodes.contains_key(identity));
    }
    assert!(result.metadata.get(BUDGET_METADATA_KEY).is_none());
}

#[tokio::test]
async fn cost_cap_halts_once_accumulated_cost_exceeds_max_cost_usd() {
    // (EN.4.0 task 6) Each node reports $3.00 via its own `ctx.nodes`
    // output's `cost_usd` field; a $5.00 cap is reached (>=) only once
    // start_node ($3.00) *and* node2 ($3.00, totaling $6.00) have both
    // completed, so the walk halts before node3 dispatches — same
    // before/after-cap-node shape as the token-cap test above, just one
    // node later since the cap sits strictly between one and two nodes'
    // worth of cost.
    let workflow = Workflow::new(cost_registry(), linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let budget = Budget {
        max_total_tokens: None,
        max_cost_usd: Some(5.0),
    };

    let result = workflow
        .run_with(
            serde_json::json!({}),
            on_progress,
            RunOptions {
                cancellation_token: None,
                budget: Some(budget),
                pause_signal: None,
                run_id: None,
            },
        )
        .await
        .expect("a budget-halted run returns Ok, not Err");

    // start_node and node2 both ran to completion.
    for identity in ["start_node", "node2"] {
        let run = result
            .node_runs
            .get(identity)
            .unwrap_or_else(|| panic!("{identity} run present"));
        assert_eq!(run.status, NodeRunStatus::Success, "{identity} status");
        assert!(result.nodes.contains_key(identity), "{identity} output");
    }

    // node3 never dispatched: still Pending, no output written.
    let node3_run = result.node_runs.get("node3").expect("node3 run present");
    assert_eq!(node3_run.status, NodeRunStatus::Pending);
    assert!(!result.nodes.contains_key("node3"));

    let budget_meta = &result.metadata[BUDGET_METADATA_KEY];
    assert_eq!(budget_meta["halted"], serde_json::json!(true));
    assert_eq!(
        budget_meta["reason"]["cap"],
        serde_json::json!("max_cost_usd")
    );
    assert_eq!(budget_meta["reason"]["spent"], serde_json::json!(6.0));
    assert_eq!(budget_meta["reason"]["limit"], serde_json::json!(5.0));
}

#[tokio::test]
async fn no_max_cost_usd_configured_runs_to_completion_unchanged() {
    // (EN.4.0 task 6) Nodes still report cost via `ctx.nodes["<id>"]["cost_usd"]`
    // (folded into the ledger regardless), but with no `max_cost_usd` set the
    // cap never gates — behavior-additive, matching pre-task-6 behavior.
    let workflow = Workflow::new(cost_registry(), linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run_with(serde_json::json!({}), on_progress, RunOptions::default())
        .await
        .expect("run should complete");

    for identity in ["start_node", "node2", "node3"] {
        let run = result.node_runs.get(identity).expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
        assert!(result.nodes.contains_key(identity));
    }
    assert!(result.metadata.get(BUDGET_METADATA_KEY).is_none());
}

#[tokio::test]
async fn plain_run_entry_point_still_works_unmodified() {
    // The existing `Workflow::run(event, on_progress)` signature must keep
    // working for callers that predate this block (engine-serve, other
    // tests) — it delegates to `run_with` with `RunOptions::default()`.
    let workflow = Workflow::new(usage_registry(), linear_schema());
    let on_progress: engine_core::OnProgress<'_> = Box::new(|_c: &TaskContext| {});

    let result = workflow
        .run(serde_json::json!({}), on_progress)
        .await
        .expect("run should complete");

    for identity in ["start_node", "node2", "node3"] {
        let run = result.node_runs.get(identity).expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
    }
}
