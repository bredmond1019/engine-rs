//! Hermetic `engine-core` suspend/resume suite (`EN.6.F` task 6).
//!
//! Every fixture here is a small in-memory graph of counting nodes -- no
//! network, no model calls -- exercising the three things this block added
//! to `Workflow`: the durable walk pointer (`ResumeState`/`run_from`), the
//! rehydratable `BudgetLedger`, and the `PauseSignal`/`SuspendNode`
//! convergence onto `metadata.suspension` (`crate::suspend`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use engine_contract::{EventsRow, NodeRun, NodeRunStatus, TaskContext, Usage};
use engine_core::nodes::SuspendNode;
use engine_core::workflow::ResumeState;
use engine_core::{
    is_suspended, read_suspension, request_suspension, Budget, BudgetLedger, CancellationToken,
    Node, NodeConfig, NodeError, NodeRegistry, OnProgress, PauseSignal, Router, RunOptions,
    SuspendReason, Workflow, WorkflowSchema, BUDGET_METADATA_KEY, CANCELLATION_METADATA_KEY,
};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// A node that increments a shared counter every time it runs, stamps a
/// trivial `ctx.nodes[identity]` output, optionally reports token usage
/// (mirroring `ClaudeCodeStep`'s pattern), and optionally runs an
/// arbitrary side effect on its way out (e.g. pausing a shared
/// `PauseSignal` from *inside* `process`).
type OnProcessHook = Arc<dyn Fn(&mut TaskContext) + Send + Sync>;

struct TrackedNode {
    identity: String,
    run_count: Arc<AtomicUsize>,
    tokens: u64,
    on_process: Option<OnProcessHook>,
}

impl TrackedNode {
    fn new(identity: impl Into<String>, run_count: Arc<AtomicUsize>) -> Self {
        Self {
            identity: identity.into(),
            run_count,
            tokens: 0,
            on_process: None,
        }
    }

    #[must_use]
    fn with_tokens(mut self, tokens: u64) -> Self {
        self.tokens = tokens;
        self
    }

    #[must_use]
    fn with_on_process(mut self, f: impl Fn(&mut TaskContext) + Send + Sync + 'static) -> Self {
        self.on_process = Some(Arc::new(f));
        self
    }
}

#[async_trait::async_trait]
impl Node for TrackedNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.run_count.fetch_add(1, Ordering::SeqCst);
        ctx.nodes
            .insert(self.identity.clone(), serde_json::json!({ "ran": true }));

        if self.tokens > 0 {
            let usage = Usage {
                input_tokens: Some(self.tokens),
                output_tokens: Some(0),
                model: "test-model".to_string(),
            };
            ctx.node_runs
                .entry(self.identity.clone())
                .and_modify(|run| run.usage = Some(usage));
        }

        if let Some(f) = &self.on_process {
            f(&mut ctx);
        }

        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.identity
    }
}

/// A router whose next-node identity is a fixed string (ignoring
/// `ctx` entirely) -- proves the resume pointer honors a *router's*
/// runtime choice rather than re-deriving it.
struct TrackedRouterNode {
    identity: String,
    run_count: Arc<AtomicUsize>,
    next: String,
}

#[async_trait::async_trait]
impl Node for TrackedRouterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.run_count.fetch_add(1, Ordering::SeqCst);
        ctx.nodes
            .insert(self.identity.clone(), serde_json::json!({ "ran": true }));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.identity
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for TrackedRouterNode {
    fn route(&self, _ctx: &TaskContext) -> Option<String> {
        Some(self.next.clone())
    }
}

/// A self-looping router: increments `ctx.nodes[identity]["iter"]` each
/// visit, requests suspension exactly once (at `iter == 2`), and keeps
/// looping back to itself until `iter == 4`. Proves the loop counter -- an
/// entry in `ctx.nodes`, not `Workflow` state -- survives rehydration.
struct LoopCounterNode {
    identity: String,
}

impl LoopCounterNode {
    fn iter_of(&self, ctx: &TaskContext) -> u64 {
        ctx.nodes
            .get(&self.identity)
            .and_then(|v| v.get("iter"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl Node for LoopCounterNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let new_iter = self.iter_of(&ctx) + 1;
        ctx.nodes.insert(
            self.identity.clone(),
            serde_json::json!({ "iter": new_iter }),
        );
        if new_iter == 2 {
            request_suspension(&mut ctx.metadata);
        }
        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.identity
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for LoopCounterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        if self.iter_of(ctx) < 4 {
            Some(self.identity.clone())
        } else {
            None
        }
    }
}

/// Builds a purely linear `WorkflowSchema` over `order`, wiring each
/// identity's `connections[0]` to the next entry (the last is terminal).
fn linear_schema(order: &[&str]) -> WorkflowSchema {
    let mut nodes = HashMap::new();
    for (i, id) in order.iter().enumerate() {
        let connections = match order.get(i + 1) {
            Some(next) => vec![(*next).to_string()],
            None => vec![],
        };
        nodes.insert(id.to_string(), NodeConfig::new(*id, connections));
    }
    WorkflowSchema::new("linear", order[0], nodes)
}

fn empty_ctx() -> TaskContext {
    TaskContext {
        event: serde_json::json!({}),
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

fn pending_run() -> NodeRun {
    NodeRun {
        status: NodeRunStatus::Pending,
        started_at: None,
        completed_at: None,
        error: None,
        input: None,
        usage: None,
    }
}

fn noop() -> OnProgress<'static> {
    Box::new(|_ctx: &TaskContext| {})
}

fn events_row_for(workflow_type: &str, event: serde_json::Value, ctx: &TaskContext) -> EventsRow {
    let now = Utc::now();
    EventsRow {
        id: uuid::Uuid::new_v4(),
        workflow_type: workflow_type.to_string(),
        data: event,
        task_context: ctx.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Resolves a `ResumeState` straight off a just-suspended `TaskContext`'s
/// own marker -- the shape every real caller (the HTTP resume handler)
/// follows: read `resume_at` + the ledger snapshot, never hand-roll them.
fn resume_state_from(ctx: TaskContext) -> ResumeState {
    let suspension = read_suspension(&ctx.metadata).expect("suspension marker present");
    let resume_at = suspension.resume_at.clone().expect("resume_at present");
    let ledger_snap = suspension.ledger.expect("ledger snapshot present");
    ResumeState {
        ctx,
        at_identity: resume_at,
        ledger: BudgetLedger::from_parts(ledger_snap.total_tokens, ledger_snap.total_cost_usd),
    }
}

// ---------------------------------------------------------------------------
// 1. suspend_node_stamps_marker_and_leaves_downstream_pending
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suspend_node_stamps_marker_and_leaves_downstream_pending() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    registry.register(Box::new(TrackedNode::new("B", count_b.clone())));

    let schema = linear_schema(&["A", "Suspend", "B"]);
    let workflow = Workflow::new(registry, schema);

    let ctx = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("run should return Ok when it suspends");

    assert_eq!(
        ctx.node_runs.get("A").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs.get("Suspend").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs.get("B").unwrap().status,
        NodeRunStatus::Pending
    );
    assert_eq!(count_b.load(Ordering::SeqCst), 0);

    let suspension = read_suspension(&ctx.metadata).expect("suspension marker present");
    assert!(suspension.suspended);
    assert_eq!(suspension.resume_at.as_deref(), Some("B"));
    assert_eq!(suspension.reason, Some(SuspendReason::SuspendNode));
    assert!(suspension.ledger.is_some());
    assert_eq!(count_a.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 2. run_from_completes_without_rerunning_completed_nodes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_from_completes_without_rerunning_completed_nodes() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_c = Arc::new(AtomicUsize::new(0));
    let count_d = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    registry.register(Box::new(TrackedNode::new("B", count_b.clone())));
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    registry.register(Box::new(TrackedNode::new("C", count_c.clone())));
    registry.register(Box::new(TrackedNode::new("D", count_d.clone())));

    let schema = linear_schema(&["A", "B", "Suspend", "C", "D"]);
    let workflow = Workflow::new(registry, schema);

    let suspended = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("initial run suspends");

    let a_completed_at = suspended.node_runs.get("A").unwrap().completed_at;
    let b_completed_at = suspended.node_runs.get("B").unwrap().completed_at;
    assert!(a_completed_at.is_some());
    assert!(b_completed_at.is_some());

    let resumed = workflow
        .run_from(resume_state_from(suspended), noop(), RunOptions::default())
        .await
        .expect("resume should complete");

    assert_eq!(
        resumed.node_runs.get("A").unwrap().completed_at,
        a_completed_at,
        "A's completed_at must be byte-identical after resume"
    );
    assert_eq!(
        resumed.node_runs.get("B").unwrap().completed_at,
        b_completed_at,
        "B's completed_at must be byte-identical after resume"
    );
    assert_eq!(
        resumed.node_runs.get("C").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(
        resumed.node_runs.get("D").unwrap().status,
        NodeRunStatus::Success
    );

    assert_eq!(count_a.load(Ordering::SeqCst), 1, "A must not re-run");
    assert_eq!(count_b.load(Ordering::SeqCst), 1, "B must not re-run");
    assert_eq!(count_c.load(Ordering::SeqCst), 1, "C must run exactly once");
    assert_eq!(count_d.load(Ordering::SeqCst), 1, "D must run exactly once");
}

// ---------------------------------------------------------------------------
// 3. resumed_run_round_trips_to_an_events_row_identical_in_shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resumed_run_round_trips_to_an_events_row_identical_in_shape() {
    // An uninterrupted linear run.
    let count_a1 = Arc::new(AtomicUsize::new(0));
    let count_b1 = Arc::new(AtomicUsize::new(0));
    let count_c1 = Arc::new(AtomicUsize::new(0));
    let mut plain_registry = NodeRegistry::new();
    plain_registry.register(Box::new(TrackedNode::new("A", count_a1)));
    plain_registry.register(Box::new(TrackedNode::new("B", count_b1)));
    plain_registry.register(Box::new(TrackedNode::new("C", count_c1)));
    let plain_workflow = Workflow::new(plain_registry, linear_schema(&["A", "B", "C"]));
    let plain_ctx = plain_workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("plain run succeeds");
    let plain_row = events_row_for("PLAIN", serde_json::json!({}), &plain_ctx);

    // A suspend + resume run over an equivalent shape.
    let count_a2 = Arc::new(AtomicUsize::new(0));
    let count_b2 = Arc::new(AtomicUsize::new(0));
    let count_c2 = Arc::new(AtomicUsize::new(0));
    let mut suspend_registry = NodeRegistry::new();
    suspend_registry.register(Box::new(TrackedNode::new("A", count_a2)));
    suspend_registry.register(Box::new(TrackedNode::new("B", count_b2)));
    suspend_registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    suspend_registry.register(Box::new(TrackedNode::new("C", count_c2)));
    let suspend_workflow =
        Workflow::new(suspend_registry, linear_schema(&["A", "B", "Suspend", "C"]));

    let suspended = suspend_workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("suspends");
    let resumed = suspend_workflow
        .run_from(resume_state_from(suspended), noop(), RunOptions::default())
        .await
        .expect("resume completes");
    let resumed_row = events_row_for("RESUMED", serde_json::json!({}), &resumed);

    let plain_value = serde_json::to_value(&plain_row).unwrap();
    let resumed_value = serde_json::to_value(&resumed_row).unwrap();
    let mut plain_keys: Vec<&String> = plain_value.as_object().unwrap().keys().collect();
    let mut resumed_keys: Vec<&String> = resumed_value.as_object().unwrap().keys().collect();
    plain_keys.sort();
    resumed_keys.sort();
    assert_eq!(
        plain_keys, resumed_keys,
        "EventsRow top-level key sets must match"
    );

    for identity in ["A", "B", "C"] {
        assert_eq!(
            plain_row
                .task_context
                .node_runs
                .get(identity)
                .unwrap()
                .status,
            NodeRunStatus::Success,
            "plain run: {identity} should be Success"
        );
        assert_eq!(
            resumed_row
                .task_context
                .node_runs
                .get(identity)
                .unwrap()
                .status,
            NodeRunStatus::Success,
            "resumed run: {identity} should be Success"
        );
    }

    // The resumed row's own shape round-trips cleanly through JSON -- the
    // real persistence path.
    let json_str = serde_json::to_string(&resumed_row).expect("EventsRow should serialize");
    let round_tripped: EventsRow =
        serde_json::from_str(&json_str).expect("EventsRow should deserialize");
    assert_eq!(round_tripped, resumed_row);
}

// ---------------------------------------------------------------------------
// 4. operator_pause_stops_at_loop_top_with_next_node_still_pending
// ---------------------------------------------------------------------------

#[tokio::test]
async fn operator_pause_stops_at_loop_top_with_next_node_still_pending() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    registry.register(Box::new(TrackedNode::new("B", count_b)));
    let workflow = Workflow::new(registry, linear_schema(&["A", "B"]));

    let signal = PauseSignal::new();
    signal.pause();
    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: Some(signal),
        run_id: None,
    };

    let ctx = workflow
        .run_with(serde_json::json!({}), noop(), options)
        .await
        .expect("a paused run returns Ok, not Err");

    assert_eq!(
        ctx.node_runs.get("A").unwrap().status,
        NodeRunStatus::Pending
    );
    assert_eq!(
        ctx.node_runs.get("B").unwrap().status,
        NodeRunStatus::Pending
    );
    assert_eq!(count_a.load(Ordering::SeqCst), 0, "A never dispatched");

    let suspension = read_suspension(&ctx.metadata).expect("suspension marker present");
    assert_eq!(suspension.resume_at.as_deref(), Some("A"));
    assert_eq!(suspension.reason, Some(SuspendReason::OperatorPause));
}

// ---------------------------------------------------------------------------
// 5. pause_set_from_inside_a_node_does_not_interrupt_that_node
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_set_from_inside_a_node_does_not_interrupt_that_node() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let signal = PauseSignal::new();
    let signal_for_node = signal.clone();

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(
        TrackedNode::new("A", count_a).with_on_process(move |_ctx| signal_for_node.pause()),
    ));
    registry.register(Box::new(TrackedNode::new("B", count_b)));
    let workflow = Workflow::new(registry, linear_schema(&["A", "B"]));

    let options = RunOptions {
        cancellation_token: None,
        budget: None,
        pause_signal: Some(signal),
        run_id: None,
    };
    let ctx = workflow
        .run_with(serde_json::json!({}), noop(), options)
        .await
        .expect("a paused run returns Ok, not Err");

    let a_run = ctx.node_runs.get("A").unwrap();
    assert_eq!(
        a_run.status,
        NodeRunStatus::Success,
        "the node that paused itself must still finish Success"
    );
    assert!(ctx.nodes.contains_key("A"), "A's output must be present");
    assert_eq!(
        ctx.node_runs.get("B").unwrap().status,
        NodeRunStatus::Pending
    );

    let suspension = read_suspension(&ctx.metadata).expect("suspension marker present");
    assert_eq!(suspension.resume_at.as_deref(), Some("B"));
    assert_eq!(suspension.reason, Some(SuspendReason::OperatorPause));
}

// ---------------------------------------------------------------------------
// 6. cancel_beats_pause_when_both_are_set
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_beats_pause_when_both_are_set() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    let workflow = Workflow::new(registry, linear_schema(&["A"]));

    let token = CancellationToken::new();
    token.cancel();
    let signal = PauseSignal::new();
    signal.pause();

    let options = RunOptions {
        cancellation_token: Some(token),
        budget: None,
        pause_signal: Some(signal),
        run_id: None,
    };
    let ctx = workflow
        .run_with(serde_json::json!({}), noop(), options)
        .await
        .expect("a cancelled run returns Ok, not Err");

    assert!(
        ctx.metadata.get(CANCELLATION_METADATA_KEY).is_some(),
        "cancellation must be the marker that wins"
    );
    assert!(
        !is_suspended(&ctx.metadata),
        "cancellation must pre-empt the suspension marker"
    );
    assert_eq!(count_a.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// 7. budget_halt_beats_pause
// ---------------------------------------------------------------------------

#[tokio::test]
async fn budget_halt_beats_pause() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    let workflow = Workflow::new(registry, linear_schema(&["A"]));

    let signal = PauseSignal::new();
    signal.pause();
    let options = RunOptions {
        cancellation_token: None,
        budget: Some(Budget {
            max_total_tokens: Some(0),
            max_cost_usd: None,
        }),
        pause_signal: Some(signal),
        run_id: None,
    };
    let ctx = workflow
        .run_with(serde_json::json!({}), noop(), options)
        .await
        .expect("a budget-halted run returns Ok, not Err");

    assert!(
        ctx.metadata.get(BUDGET_METADATA_KEY).is_some(),
        "budget halt must be the marker that wins"
    );
    assert!(
        !is_suspended(&ctx.metadata),
        "budget halt must pre-empt the suspension marker"
    );
    assert_eq!(count_a.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// 8. resume_after_a_router_does_not_rerun_the_router
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_after_a_router_does_not_rerun_the_router() {
    let router_count = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedRouterNode {
        identity: "Router".to_string(),
        run_count: router_count.clone(),
        next: "Suspend".to_string(),
    }));
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    registry.register(Box::new(TrackedNode::new("B", count_b.clone())));

    let mut nodes = HashMap::new();
    nodes.insert("Router".to_string(), NodeConfig::new("Router", vec![]));
    nodes.insert(
        "Suspend".to_string(),
        NodeConfig::new("Suspend", vec!["B".to_string()]),
    );
    nodes.insert("B".to_string(), NodeConfig::new("B", vec![]));
    let schema = WorkflowSchema::new("router-suspend", "Router", nodes);
    let workflow = Workflow::new(registry, schema);

    let suspended = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("suspends");
    assert_eq!(router_count.load(Ordering::SeqCst), 1);

    let resumed = workflow
        .run_from(resume_state_from(suspended), noop(), RunOptions::default())
        .await
        .expect("resume completes");

    assert_eq!(
        router_count.load(Ordering::SeqCst),
        1,
        "the router must not re-run on resume -- the stored pointer is honored"
    );
    assert_eq!(
        resumed.node_runs.get("B").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(count_b.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 9. resume_inside_a_loop_preserves_the_iteration_counter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_inside_a_loop_preserves_the_iteration_counter() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(LoopCounterNode {
        identity: "Loop".to_string(),
    }));

    let mut nodes = HashMap::new();
    nodes.insert("Loop".to_string(), NodeConfig::new("Loop", vec![]));
    let schema = WorkflowSchema::new("loop", "Loop", nodes);
    let workflow = Workflow::new(registry, schema);

    let suspended = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("suspends mid-loop");
    assert_eq!(
        suspended.nodes.get("Loop").unwrap()["iter"],
        serde_json::json!(2)
    );
    let suspension = read_suspension(&suspended.metadata).expect("suspension marker present");
    assert!(suspension.suspended);
    assert_eq!(suspension.resume_at.as_deref(), Some("Loop"));

    let resumed = workflow
        .run_from(resume_state_from(suspended), noop(), RunOptions::default())
        .await
        .expect("resume completes");

    assert_eq!(
        resumed.nodes.get("Loop").unwrap()["iter"],
        serde_json::json!(4),
        "the loop counter must continue from its rehydrated value (2), not reset to 0"
    );
    assert!(!is_suspended(&resumed.metadata));
}

// ---------------------------------------------------------------------------
// 10. resumed_ledger_halts_on_a_cap_already_approached_pre_suspend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resumed_ledger_halts_on_a_cap_already_approached_pre_suspend() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_c = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a).with_tokens(80)));
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    registry.register(Box::new(
        TrackedNode::new("B", count_b.clone()).with_tokens(80),
    ));
    registry.register(Box::new(
        TrackedNode::new("C", count_c.clone()).with_tokens(80),
    ));

    let workflow = Workflow::new(registry, linear_schema(&["A", "Suspend", "B", "C"]));

    let budget = Budget {
        max_total_tokens: Some(100),
        max_cost_usd: None,
    };
    let options = RunOptions {
        cancellation_token: None,
        budget: Some(budget),
        pause_signal: None,
        run_id: None,
    };

    let suspended = workflow
        .run_with(serde_json::json!({}), noop(), options)
        .await
        .expect("initial run suspends before the budget trips");

    assert_eq!(
        suspended.node_runs.get("A").unwrap().status,
        NodeRunStatus::Success
    );
    let suspension = read_suspension(&suspended.metadata).expect("suspension marker present");
    let ledger_snap = suspension.ledger.expect("ledger snapshot present");
    assert_eq!(ledger_snap.total_tokens, 80);

    let options = RunOptions {
        cancellation_token: None,
        budget: Some(budget),
        pause_signal: None,
        run_id: None,
    };
    let resumed = workflow
        .run_from(resume_state_from(suspended), noop(), options)
        .await
        .expect("resume halts on the rehydrated ledger, not on a fresh one");

    assert_eq!(
        resumed.node_runs.get("B").unwrap().status,
        NodeRunStatus::Success,
        "B still ran -- the rehydrated ledger (80) was under the cap (100) at that boundary"
    );
    assert_eq!(
        resumed.node_runs.get("C").unwrap().status,
        NodeRunStatus::Pending,
        "C never dispatched -- 80 + 80 = 160 already at/over the 100 cap"
    );
    assert_eq!(count_c.load(Ordering::SeqCst), 0);
    assert!(resumed.metadata.get(BUDGET_METADATA_KEY).is_some());
}

// ---------------------------------------------------------------------------
// 11. run_from_does_not_apply_seeded_nodes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_from_does_not_apply_seeded_nodes() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a)));

    // Rebuilt for the resume with a DIFFERENT seeded value than whatever
    // the original run used -- e.g. `SDLC_FLOW`'s factory re-resolving
    // policy from a different working directory.
    let mut different_seed = HashMap::new();
    different_seed.insert("X".to_string(), serde_json::json!("different-run's-policy"));
    let workflow = Workflow::new(registry, linear_schema(&["A"])).with_seeded_nodes(different_seed);

    let mut ctx = empty_ctx();
    ctx.nodes
        .insert("X".to_string(), serde_json::json!("rehydrated-policy"));
    ctx.node_runs.insert("A".to_string(), pending_run());

    let state = ResumeState {
        ctx,
        at_identity: "A".to_string(),
        ledger: BudgetLedger::new(),
    };
    let resumed = workflow
        .run_from(state, noop(), RunOptions::default())
        .await
        .expect("resume completes");

    assert_eq!(
        resumed.nodes.get("X"),
        Some(&serde_json::json!("rehydrated-policy")),
        "the rehydrated ctx's value must win -- run_from never applies seeded_nodes"
    );
}

// ---------------------------------------------------------------------------
// 12. run_from_seeds_only_missing_pending_nodes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_from_seeds_only_missing_pending_nodes() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_c = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a)));
    registry.register(Box::new(TrackedNode::new("B", count_b.clone())));
    registry.register(Box::new(TrackedNode::new("C", count_c.clone())));
    let workflow = Workflow::new(registry, linear_schema(&["A", "B", "C"]));

    let fixed_completed_at = Utc::now() - chrono::Duration::hours(2);
    let mut ctx = empty_ctx();
    ctx.node_runs.insert(
        "A".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(fixed_completed_at),
            completed_at: Some(fixed_completed_at),
            error: None,
            input: None,
            usage: None,
        },
    );
    ctx.node_runs.insert("B".to_string(), pending_run());
    // "C" is deliberately absent from `ctx.node_runs` -- schema drift the
    // rehydrated context has never heard of.

    let state = ResumeState {
        ctx,
        at_identity: "C".to_string(),
        ledger: BudgetLedger::new(),
    };
    let resumed = workflow
        .run_from(state, noop(), RunOptions::default())
        .await
        .expect("resume completes");

    assert_eq!(resumed.node_runs.len(), 3);

    let a_run = resumed.node_runs.get("A").unwrap();
    assert_eq!(a_run.status, NodeRunStatus::Success);
    assert_eq!(
        a_run.completed_at,
        Some(fixed_completed_at),
        "A's existing NodeRun must be untouched"
    );

    let b_run = resumed.node_runs.get("B").unwrap();
    assert_eq!(
        b_run.status,
        NodeRunStatus::Pending,
        "B's existing NodeRun must be untouched -- it was never dispatched"
    );
    assert!(b_run.started_at.is_none());
    assert!(b_run.completed_at.is_none());
    assert_eq!(count_b.load(Ordering::SeqCst), 0);

    let c_run = resumed.node_runs.get("C").unwrap();
    assert_eq!(
        c_run.status,
        NodeRunStatus::Success,
        "C was seeded Pending by schema drift, then actually ran"
    );
    assert_eq!(count_c.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 13. suspend_at_the_last_node_completes_instead_of_suspending
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suspend_at_the_last_node_completes_instead_of_suspending() {
    let mut registry = NodeRegistry::new();
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));

    let mut nodes = HashMap::new();
    nodes.insert("Suspend".to_string(), NodeConfig::new("Suspend", vec![]));
    let schema = WorkflowSchema::new("single", "Suspend", nodes);
    let workflow = Workflow::new(registry, schema);

    let ctx = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("run should complete normally");

    assert_eq!(
        ctx.node_runs.get("Suspend").unwrap().status,
        NodeRunStatus::Success
    );
    assert!(
        !is_suspended(&ctx.metadata),
        "suspending at the graph's last node must complete the run, not suspend it"
    );
}

// ---------------------------------------------------------------------------
// 14. suspend_node_disabled_is_a_verified_no_op
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suspend_node_disabled_is_a_verified_no_op() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a.clone())));
    registry.register(Box::new(SuspendNode::new("Suspend"))); // enabled: false (default)
    registry.register(Box::new(TrackedNode::new("B", count_b.clone())));

    let workflow = Workflow::new(registry, linear_schema(&["A", "Suspend", "B"]));

    let ctx = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("run completes");

    assert_eq!(
        ctx.node_runs.get("A").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs.get("Suspend").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(
        ctx.node_runs.get("B").unwrap().status,
        NodeRunStatus::Success
    );
    assert!(!is_suspended(&ctx.metadata));
    assert_eq!(count_a.load(Ordering::SeqCst), 1);
    assert_eq!(count_b.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 15. task_context_survives_a_json_round_trip_between_suspend_and_resume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_context_survives_a_json_round_trip_between_suspend_and_resume() {
    let count_a = Arc::new(AtomicUsize::new(0));
    let count_b = Arc::new(AtomicUsize::new(0));
    let count_c = Arc::new(AtomicUsize::new(0));

    let mut registry = NodeRegistry::new();
    registry.register(Box::new(TrackedNode::new("A", count_a)));
    registry.register(Box::new(TrackedNode::new("B", count_b)));
    registry.register(Box::new(SuspendNode::new("Suspend").with_enabled(true)));
    registry.register(Box::new(TrackedNode::new("C", count_c.clone())));

    let workflow = Workflow::new(registry, linear_schema(&["A", "B", "Suspend", "C"]));

    let suspended = workflow
        .run(serde_json::json!({}), noop())
        .await
        .expect("suspends");

    // Serialize the suspended context to a string and back -- the real
    // persistence path (`events.task_context`), not an in-memory handoff.
    let json_str = serde_json::to_string(&suspended).expect("TaskContext should serialize");
    let rehydrated: TaskContext =
        serde_json::from_str(&json_str).expect("TaskContext should deserialize");
    assert_eq!(rehydrated, suspended);

    let resumed = workflow
        .run_from(resume_state_from(rehydrated), noop(), RunOptions::default())
        .await
        .expect("resume completes from the deserialized context");

    assert_eq!(
        resumed.node_runs.get("C").unwrap().status,
        NodeRunStatus::Success
    );
    assert_eq!(count_c.load(Ordering::SeqCst), 1);
    assert!(!is_suspended(&resumed.metadata));
}
