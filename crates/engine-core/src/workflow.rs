//! The `Workflow` pointer-walk runner: seeds every node PENDING, walks
//! node-to-node via `WorkflowSchema::next_after` (`connections[0]`) for plain
//! nodes or via `Router::route(ctx)` (which may return an undeclared runtime
//! back-edge) for routers, and stamps each `NodeRun` RUNNING then
//! SUCCESS/FAILED + timing around the framework-owned `node_context` envelope.
//!
//! The `on_progress` callback is the injected persistence seam (contract-facing
//! Postgres wiring lands in EN.1.C) — this block only defines its signature and
//! invokes it at node boundaries.

use std::collections::HashMap;

use chrono::Utc;
use engine_contract::{NodeRun, NodeRunStatus, TaskContext};

use crate::budget::{Budget, BudgetDecision, BudgetHaltReason, BudgetLedger};
use crate::cancellation::{stamp_cancelled, CancellationToken};
use crate::node::NodeRegistry;
use crate::schema::WorkflowSchema;
use crate::validate::{ValidationError, WorkflowValidator};

/// The `TaskContext::metadata` key under which a budget-halted run's reason
/// is recorded — see [`stamp_budget_halt`]. Sibling to
/// `cancellation::CANCELLATION_METADATA_KEY`.
pub const BUDGET_METADATA_KEY: &str = "budget";

/// The `TaskContext::metadata` key under which every run's workflow-agnostic
/// [`policy::telemetry::RunTelemetry`] snapshot is recorded — see
/// [`stamp_run_telemetry`]. Sibling to `CANCELLATION_METADATA_KEY`/
/// `BUDGET_METADATA_KEY`.
///
/// [`policy::telemetry::RunTelemetry`]: crate::policy::telemetry::RunTelemetry
pub const RUN_TELEMETRY_METADATA_KEY: &str = "run_telemetry";

/// Optional cancellation/budget wiring for [`Workflow::run_with`]. Every
/// field defaults to `None`, matching [`Workflow::run`]'s behavior exactly —
/// no token means no cancellation check, no budget means no gate.
#[derive(Default)]
pub struct RunOptions {
    /// Checked at each node boundary before dispatching the next node. On
    /// cancellation the walk stops, the cancelled marker is stamped into
    /// `ctx.metadata` (D6), a final `on_progress` snapshot is emitted, and
    /// `run_with` returns `Ok(ctx)` — not an `Err` — so a cancelled run is
    /// distinguishable from a failed one.
    pub cancellation_token: Option<CancellationToken>,
    /// Consulted before dispatching each node via `BudgetLedger::check`. On
    /// halt the walk stops, the reason is stamped into `ctx.metadata`, and a
    /// final `on_progress` snapshot is emitted. `None` means no gate at all
    /// (existing `run` callers keep their unmodified behavior).
    pub budget: Option<Budget>,
}

/// Stamps `metadata` with the budget-halt marker:
/// `{ "budget": { "halted": true, "reason": { "cap": ..., "spent": ...,
/// "limit": ... } } }`. Mirrors `cancellation::stamp_cancelled`'s shape.
fn stamp_budget_halt(metadata: &mut serde_json::Value, reason: BudgetHaltReason) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    metadata[BUDGET_METADATA_KEY] = serde_json::json!({
        "halted": true,
        "reason": reason.to_json(),
    });
}

/// The injected persistence seam, invoked with a snapshot of the `TaskContext`
/// at each node boundary (initial seed, and after every node transition).
/// This block defines the signature only — EN.1.C wires it to Postgres.
pub type OnProgress<'a> = Box<dyn FnMut(&TaskContext) + 'a>;

/// A runnable workflow: the node registry (identity -> `Node` impl) paired
/// with the declarative `WorkflowSchema` describing the graph shape.
pub struct Workflow {
    registry: NodeRegistry,
    schema: WorkflowSchema,
    seeded_nodes: HashMap<String, serde_json::Value>,
}

/// Error returned by `Workflow::run` for conditions outside a node's own
/// `NodeError` (e.g. an unresolvable start node or a dangling connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowError {
    pub message: String,
}

impl WorkflowError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WorkflowError {}

impl Workflow {
    pub fn new(registry: NodeRegistry, schema: WorkflowSchema) -> Self {
        Self {
            registry,
            schema,
            seeded_nodes: HashMap::new(),
        }
    }

    /// Run `WorkflowValidator::validate` against `registry`/`schema` before
    /// constructing. Use this constructor when the declared graph must be
    /// guaranteed structurally sound (BFS reachability, DFS cycle check that
    /// skips router edges, and router-only fan-out) before any node runs.
    ///
    /// `Workflow::new` stays infallible and unvalidated so EN.1.A callers and
    /// tests keep compiling unchanged.
    pub fn new_validated(
        registry: NodeRegistry,
        schema: WorkflowSchema,
    ) -> Result<Self, ValidationError> {
        WorkflowValidator::validate(&registry, &schema)?;
        Ok(Self {
            registry,
            schema,
            seeded_nodes: HashMap::new(),
        })
    }

    /// Seed entries into the run's initial `ctx.nodes` before the walk starts.
    /// EN.5.D uses this to carry the policy resolved **once per run at
    /// dispatch** into the run via `policy::RESOLVED_POLICY_IDENTITY`, so no
    /// node re-resolves it (and re-reads `harness.json`) inside `process()`.
    #[must_use]
    pub fn with_seeded_nodes(mut self, seeded: HashMap<String, serde_json::Value>) -> Self {
        self.seeded_nodes = seeded;
        self
    }

    /// Run the workflow to completion (or first failure).
    ///
    /// `event` seeds `TaskContext::event`; all nodes declared in the schema are
    /// seeded PENDING in `node_runs` before the walk starts, and the initial
    /// snapshot is emitted via `on_progress` before the first node runs. The
    /// pointer-walk starts at the schema's start node. For a non-router node
    /// the walk follows `connections[0]`; for a router (`Node::as_router()`
    /// returns `Some`) the walk instead calls `Router::route(&ctx)` to choose
    /// the next identity at runtime — which may be an identity outside the
    /// router's declared `connections` (a retry/back-edge). A router returning
    /// `None` from `route` ends the walk. A node returning `Err` is stamped
    /// FAILED and halts the walk (the accumulated `TaskContext` is still
    /// returned).
    pub async fn run(
        &self,
        event: serde_json::Value,
        on_progress: OnProgress<'_>,
    ) -> Result<TaskContext, WorkflowError> {
        self.run_with(event, on_progress, RunOptions::default())
            .await
    }

    /// Like [`Workflow::run`], but with optional cancellation and budget-gate
    /// wiring (EN.2.B task 3). `RunOptions::default()` (both fields `None`)
    /// behaves exactly like `run` — no token means no cancellation check, no
    /// budget means no gate, no metadata change, no behavior change.
    ///
    /// Both checks happen at the node boundary, before dispatching the next
    /// node: cancellation is checked first, then the budget gate. On either
    /// halt the walk stops, the reason is stamped into `ctx.metadata`, a
    /// final `on_progress` snapshot is emitted, and this returns `Ok(ctx)` —
    /// nodes not yet reached stay `Pending`. After each node completes
    /// successfully, its `NodeRun.usage` is folded into the budget ledger
    /// alongside any `"cost_usd"` the node wrote to its own `ctx.nodes`
    /// output (EN.4.0 task 6), so `Budget::max_cost_usd` gates a run the
    /// same way `Budget::max_total_tokens` already does.
    pub async fn run_with(
        &self,
        event: serde_json::Value,
        mut on_progress: OnProgress<'_>,
        options: RunOptions,
    ) -> Result<TaskContext, WorkflowError> {
        let mut ctx = TaskContext {
            event,
            nodes: self.seeded_nodes.clone(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };

        // Seed every declared node PENDING before anything runs.
        for identity in self.schema.nodes.keys() {
            ctx.node_runs.insert(
                identity.clone(),
                NodeRun {
                    status: NodeRunStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    error: None,
                    input: None,
                    usage: None,
                },
            );
        }
        on_progress(&ctx);

        let mut current = Some(self.schema.start_node.clone());
        let mut ledger = BudgetLedger::new();

        while let Some(identity) = current {
            if let Some(token) = &options.cancellation_token {
                if token.is_cancelled() {
                    stamp_cancelled(&mut ctx.metadata);
                    stamp_run_telemetry(&mut ctx, &self.schema.start_node);
                    on_progress(&ctx);
                    return Ok(ctx);
                }
            }

            if let BudgetDecision::Halt(reason) = ledger.check(options.budget.as_ref()) {
                stamp_budget_halt(&mut ctx.metadata, reason);
                stamp_run_telemetry(&mut ctx, &self.schema.start_node);
                on_progress(&ctx);
                return Ok(ctx);
            }

            let node = self.registry.get(&identity).ok_or_else(|| {
                WorkflowError::new(format!("no node registered for identity '{identity}'"))
            })?;

            // Routers choose their next identity at runtime via `route(ctx)`
            // (possibly an undeclared back-edge); plain nodes keep walking
            // the statically declared `connections[0]`. Resolve this before
            // `node_context` consumes `ctx` so the router sees the context
            // as it stood on entry to this node.
            let router_next = node
                .as_router()
                .map(|router| crate::routing::dispatch_route(router, &ctx));

            let (next_ctx, failed) = node_context(node, ctx, &mut on_progress).await;
            ctx = next_ctx;

            if failed {
                break;
            }

            if let Some(run) = ctx.node_runs.get(&identity) {
                let cost_usd = node_cost_usd(&ctx, &identity);
                ledger.record(run.usage.as_ref(), cost_usd);
            }

            current = match router_next {
                Some(next) => next,
                None => self.schema.next_after(&identity).map(str::to_string),
            };
        }

        stamp_run_telemetry(&mut ctx, &self.schema.start_node);
        Ok(ctx)
    }
}

/// Stamp a workflow-agnostic [`policy::telemetry::RunTelemetry`] snapshot
/// into `metadata[RUN_TELEMETRY_METADATA_KEY]`, harvested from `ctx` alone
/// (`EN.5.D` task 10) — previously this only happened inside the
/// `#[ignore]`d profile-ranking experiments (or a workflow's own
/// hand-rolled `finalize_outcomes`), so a served run's `TaskContext` carried
/// no telemetry of its own at all.
///
/// Graph-agnostic: every identity `ctx.nodes` carries by this point in the
/// run is passed as its own verdict/cost/model-tier stage, so this needs no
/// per-workflow stage list — `policy::telemetry::{review_verdicts,
/// total_cost_usd, observed_model_tiers}` simply find nothing to report for
/// an identity whose output carries none of `"verdict"`/`"cost_usd"`/
/// `"transport"`. `total_attempts`/`total_retries`/`tasks_passed`/
/// `tasks_failed` stay `0` here (not derivable without workflow-specific
/// state); a workflow that tracks those (e.g. SDLC's `SDLCState`) still
/// computes its own precise `RunOutcomes` via its own `finalize_outcomes` —
/// that write is not disturbed by this one, they sit at different `ctx`
/// locations (`ctx.nodes["WrapUpNode"]` vs `ctx.metadata`).
///
/// Never fails the run: a `RunTelemetry` that somehow won't serialize is
/// simply not stamped, rather than turning an otherwise-successful run into
/// a `WorkflowError`.
fn stamp_run_telemetry(ctx: &mut TaskContext, start_node_identity: &str) {
    let identities: Vec<String> = ctx.nodes.keys().cloned().collect();
    let stages: Vec<&str> = identities.iter().map(String::as_str).collect();

    let inputs = crate::policy::telemetry::RunTelemetryInputs {
        start_node_identity,
        verdict_stages: &stages,
        cost_bearing_stages: &stages,
        model_stages: &stages,
        total_attempts: 0,
        total_retries: 0,
        tasks_passed: 0,
        tasks_failed: 0,
        model_tier_used: std::collections::BTreeMap::new(),
    };
    let telemetry = crate::policy::telemetry::harvest(ctx, Utc::now(), inputs);

    if let Ok(value) = serde_json::to_value(&telemetry) {
        if !ctx.metadata.is_object() {
            ctx.metadata = serde_json::json!({});
        }
        ctx.metadata[RUN_TELEMETRY_METADATA_KEY] = value;
    }
}

/// Reads a completed node's dollar cost out of its own `ctx.nodes[identity]`
/// output, the same `"cost_usd"` field shape `ClaudeCodeStep` writes (and
/// `policy::telemetry::total_cost_usd` reads for SDLC's cost-bearing
/// stages). `None` when the node's output has no such field (non-LLM nodes,
/// or an LLM node whose SDK call reported no cost) — folded into the
/// [`BudgetLedger`] alongside token usage so `Budget::max_cost_usd` gates a
/// run the same way `Budget::max_total_tokens` already does.
fn node_cost_usd(ctx: &TaskContext, identity: &str) -> Option<f64> {
    ctx.nodes.get(identity)?.get("cost_usd")?.as_f64()
}

/// The framework-owned envelope around a single node's `process` call: stamps
/// `NodeRun` RUNNING + `started_at` on entry, then SUCCESS + `completed_at` on
/// `Ok` or FAILED + `completed_at` + `error` on `Err`, invoking `on_progress`
/// after each transition. Returns the updated `TaskContext` and whether the
/// node failed (so the caller knows to halt the walk).
async fn node_context(
    node: &dyn crate::node::Node,
    mut ctx: TaskContext,
    on_progress: &mut OnProgress<'_>,
) -> (TaskContext, bool) {
    let identity = node.name().to_string();

    ctx.node_runs
        .entry(identity.clone())
        .and_modify(|run| {
            run.status = NodeRunStatus::Running;
            run.started_at = Some(Utc::now());
        })
        .or_insert_with(|| NodeRun {
            status: NodeRunStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        });
    on_progress(&ctx);

    // `Node::process` only hands the context back on `Ok`; keep a pre-call
    // snapshot so the FAILED transition still has a `TaskContext` to stamp
    // and return on `Err`.
    let pre_call_ctx = ctx.clone();

    match node.process(ctx).await {
        Ok(mut ok_ctx) => {
            if let Some(run) = ok_ctx.node_runs.get_mut(&identity) {
                run.status = NodeRunStatus::Success;
                run.completed_at = Some(Utc::now());
            }
            on_progress(&ok_ctx);
            (ok_ctx, false)
        }
        Err(err) => {
            let mut err_ctx = pre_call_ctx;
            if let Some(run) = err_ctx.node_runs.get_mut(&identity) {
                run.status = NodeRunStatus::Failed;
                run.completed_at = Some(Utc::now());
                run.error = Some(err.message.clone());
            }
            on_progress(&err_ctx);
            (err_ctx, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, NodeError};

    struct SuccessNode;

    #[async_trait::async_trait]
    impl Node for SuccessNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SuccessNode"
        }
    }

    struct FailNode;

    #[async_trait::async_trait]
    impl Node for FailNode {
        async fn process(&self, _ctx: TaskContext) -> Result<TaskContext, NodeError> {
            Err(NodeError::new("boom"))
        }

        fn name(&self) -> &str {
            "FailNode"
        }
    }

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn node_context_stamps_success_transition() {
        let node = SuccessNode;
        let mut ctx = empty_context();
        ctx.node_runs.insert(
            "SuccessNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let mut on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        let (out, failed) = node_context(&node, ctx, &mut on_progress).await;
        drop(on_progress);

        assert!(!failed);
        let run = out.node_runs.get("SuccessNode").expect("run present");
        assert_eq!(run.status, NodeRunStatus::Success);
        assert!(run.started_at.is_some());
        assert!(run.completed_at.is_some());
        assert!(run.started_at.unwrap() <= run.completed_at.unwrap());
        assert!(run.error.is_none());

        // Two on_progress calls: entering RUNNING, then exiting SUCCESS.
        let snapshots = snapshots.borrow();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(
            snapshots[0].node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Running
        );
        assert_eq!(
            snapshots[1].node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Success
        );
    }

    #[tokio::test]
    async fn node_context_stamps_failure_transition() {
        let node = FailNode;
        let mut ctx = empty_context();
        ctx.node_runs.insert(
            "FailNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );

        let mut on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let (out, failed) = node_context(&node, ctx, &mut on_progress).await;

        assert!(failed);
        let run = out.node_runs.get("FailNode").expect("run present");
        assert_eq!(run.status, NodeRunStatus::Failed);
        assert!(run.started_at.is_some());
        assert!(run.completed_at.is_some());
        assert_eq!(run.error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn run_seeds_all_nodes_pending_before_first_run() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SuccessNode", nodes);

        let workflow = Workflow::new(registry, schema);

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        let result = workflow.run(serde_json::json!({}), on_progress).await;

        assert!(result.is_ok());
        // First snapshot is the initial PENDING seed, before any node runs.
        let snapshots = snapshots.borrow();
        let first = &snapshots[0];
        assert_eq!(
            first.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );
        assert!(first
            .node_runs
            .get("SuccessNode")
            .unwrap()
            .started_at
            .is_none());
    }

    #[tokio::test]
    async fn run_halts_walk_on_failure() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(FailNode));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "FailNode".to_string(),
            crate::schema::NodeConfig::new("FailNode", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "FailNode", nodes);

        let workflow = Workflow::new(registry, schema);
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let result = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .unwrap();

        assert_eq!(
            result.node_runs.get("FailNode").unwrap().status,
            NodeRunStatus::Failed
        );
        assert_eq!(
            result.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );
        assert!(!result.nodes.contains_key("SuccessNode"));
    }

    /// Reads a seeded entry at `ctx.nodes["X"]` and echoes it back into its
    /// own output, proving the seed is visible to the start node.
    struct SeedReaderNode;

    #[async_trait::async_trait]
    impl Node for SeedReaderNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            let seen = ctx.nodes.get("X").cloned();
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "saw": seen }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SeedReaderNode"
        }
    }

    #[tokio::test]
    async fn unseeded_workflow_starts_with_empty_nodes() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SuccessNode", nodes);

        let workflow = Workflow::new(registry, schema);

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .unwrap();

        let snapshots = snapshots.borrow();
        let first = &snapshots[0];
        assert!(first.nodes.is_empty());
    }

    #[tokio::test]
    async fn seeded_entry_is_visible_in_the_first_snapshot() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SuccessNode", nodes);

        let mut seeded = HashMap::new();
        seeded.insert("X".to_string(), serde_json::json!({ "a": 1 }));
        let workflow = Workflow::new(registry, schema).with_seeded_nodes(seeded);

        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::<TaskContext>::new()));
        let snapshots_handle = snapshots.clone();
        let on_progress: OnProgress<'_> =
            Box::new(move |c: &TaskContext| snapshots_handle.borrow_mut().push(c.clone()));

        workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .unwrap();

        let snapshots = snapshots.borrow();
        let first = &snapshots[0];
        assert_eq!(first.nodes.get("X").unwrap()["a"], serde_json::json!(1));
    }

    #[tokio::test]
    async fn seeded_entry_is_readable_by_the_start_node() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SeedReaderNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "SeedReaderNode".to_string(),
            crate::schema::NodeConfig::new("SeedReaderNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SeedReaderNode", nodes);

        let mut seeded = HashMap::new();
        seeded.insert("X".to_string(), serde_json::json!({ "a": 1 }));
        let workflow = Workflow::new(registry, schema).with_seeded_nodes(seeded);

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let result = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .unwrap();

        assert_eq!(
            result.nodes.get("SeedReaderNode").unwrap()["saw"],
            serde_json::json!({ "a": 1 })
        );
    }

    /// Two-node fixture reused by task 10's `stamp_run_telemetry` tests:
    /// `start_node -> SuccessNode` (terminal), wired via `connections[0]`.
    fn two_node_workflow() -> Workflow {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "start_node".to_string(),
            crate::schema::NodeConfig::new("start_node", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        registry.register(Box::new(SuccessNode2));
        let schema = WorkflowSchema::new("linear", "start_node", nodes);
        Workflow::new(registry, schema)
    }

    /// A second distinctly-named `SuccessNode` registered under the
    /// `"start_node"` identity, so `two_node_workflow`'s two schema entries
    /// each have a distinct registered `Node`.
    struct SuccessNode2;

    #[async_trait::async_trait]
    impl Node for SuccessNode2 {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "start_node"
        }
    }

    /// `RunTelemetry` deserialized out of `metadata[RUN_TELEMETRY_METADATA_KEY]`
    /// — panics (failing the test) if the key is absent or unparsable.
    fn stamped_run_telemetry(ctx: &TaskContext) -> crate::policy::telemetry::RunTelemetry {
        let value = ctx
            .metadata
            .get(RUN_TELEMETRY_METADATA_KEY)
            .unwrap_or_else(|| panic!("metadata[{RUN_TELEMETRY_METADATA_KEY:?}] not stamped"));
        serde_json::from_value(value.clone())
            .expect("stamped run_telemetry deserializes into RunTelemetry")
    }

    #[tokio::test]
    async fn every_run_stamps_run_telemetry_into_metadata() {
        let workflow = two_node_workflow();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let ctx = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .expect("run should succeed");

        // Both nodes should have completed successfully.
        assert_eq!(
            ctx.node_runs.get("start_node").unwrap().status,
            NodeRunStatus::Success
        );
        assert_eq!(
            ctx.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Success
        );

        stamped_run_telemetry(&ctx);
    }

    #[tokio::test]
    async fn cancelled_run_still_stamps_run_telemetry_alongside_the_cancelled_marker() {
        let workflow = two_node_workflow();

        // Pre-cancelled: the walk halts at the very first node boundary
        // before dispatching `start_node`.
        let token = CancellationToken::new();
        token.cancel();

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: Some(token),
            budget: None,
        };

        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("a cancelled run returns Ok, not Err");

        assert!(ctx
            .metadata
            .get(crate::cancellation::CANCELLATION_METADATA_KEY)
            .is_some());
        stamped_run_telemetry(&ctx);
    }

    #[tokio::test]
    async fn budget_halted_run_still_stamps_run_telemetry_alongside_the_halt_marker() {
        let workflow = two_node_workflow();

        // A zero-token cap halts before the very first node dispatches: an
        // empty ledger's `0 >= 0` trips immediately.
        let budget = Budget {
            max_total_tokens: Some(0),
            max_cost_usd: None,
        };

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: Some(budget),
        };

        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("a budget-halted run returns Ok, not Err");

        assert!(ctx.metadata.get(BUDGET_METADATA_KEY).is_some());
        stamped_run_telemetry(&ctx);
    }
}
