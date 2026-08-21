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
use uuid::Uuid;

use crate::budget::{Budget, BudgetDecision, BudgetHaltReason, BudgetLedger};
use crate::cancellation::{stamp_cancelled, CancellationToken};
use crate::node::NodeRegistry;
use crate::schema::WorkflowSchema;
use crate::suspend::{self, stamp_suspended, PauseSignal, SuspendReason, Suspension};
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

/// The `TaskContext::metadata` key under which the engine's run UUID (the
/// `events.id` that `engine-serve` mints for this dispatch) is stamped — see
/// [`stamp_run_id`]/[`read_run_id`]. Sibling to `BUDGET_METADATA_KEY`/
/// `CANCELLATION_METADATA_KEY`. Plumbed so a `sdlc-flow-state.json` artifact
/// can be joined back to the engine run that produced it (EN.6.J).
pub const RUN_ID_METADATA_KEY: &str = "run_id";

/// Everything [`Workflow::run_from`] needs to continue a suspended run: the
/// rehydrated `TaskContext` (already carrying its resolved policy and every
/// completed `NodeRun`), the identity to resume at, and the budget ledger
/// snapshot to resume spend-tracking from (rather than zero).
pub struct ResumeState {
    pub ctx: TaskContext,
    pub at_identity: String,
    pub ledger: BudgetLedger,
}

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
    /// Checked at the loop top, after cancellation and after the budget gate
    /// (EN.6.F task 4): a paused signal stops the walk before the next node
    /// dispatches, exactly like cancellation/budget do, but stamps the
    /// suspension marker (`suspend::stamp_suspended`) instead. `None` means
    /// no pause check at all — existing `run`/`run_with` callers keep their
    /// unmodified behavior.
    pub pause_signal: Option<PauseSignal>,
    /// The engine's run UUID (EN.6.J) — stamped into `ctx.metadata` under
    /// [`RUN_ID_METADATA_KEY`] before the walk starts (`run_with` after
    /// `seed_context`, `run_from` after `stamp_resumed`), so every node
    /// boundary onward carries it and it round-trips into the committed
    /// `sdlc-flow-state.json`. `None` means no stamp and no metadata
    /// change at all — existing `run`/`run_with`/`run_from` callers keep
    /// byte-identical behavior.
    pub run_id: Option<Uuid>,
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

/// Stamps `metadata` with the run's engine UUID under [`RUN_ID_METADATA_KEY`]
/// as a hyphenated string. Mirrors `stamp_budget_halt`'s object-guard shape:
/// a non-object `metadata` (e.g. the default `{}` — always true in practice,
/// but guarded for safety) is reset to an empty object first.
fn stamp_run_id(metadata: &mut serde_json::Value, run_id: Uuid) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    metadata[RUN_ID_METADATA_KEY] = serde_json::json!(run_id.to_string());
}

/// Reads back the run id stamped by [`stamp_run_id`], if any. Returns `None`
/// for a non-object `metadata`, a missing key, or a non-string value — never
/// panics.
pub fn read_run_id(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get(RUN_ID_METADATA_KEY)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Reads a run's `campaign_id`, if any, for tracing instrumentation
/// (`EN.11.I` task 2). Mirrors `engine_serve::live_state::read_campaign_id`'s
/// two-location resolution order exactly — reimplemented here rather than
/// imported because `engine-core` cannot depend on `engine-serve` in
/// production code (the dependency runs the other way: `engine-serve` embeds
/// `engine-core`; `engine-serve` is only a `[dev-dependencies]` of this crate,
/// for tests).
///
/// Checked in order:
/// 1. `ctx.event["campaign_id"]` — a child `SDLC_FLOW` run's wire seam
///    (`EN.11.E` task 2).
/// 2. `ctx.nodes[NODE_NAME]["campaign_id"]` — the parent `ORCHESTRATION`
///    run's own node result (`EN.11.E` task 3), keyed by the SAME constant
///    `OrchestrationRunNode` itself registers under, not a hand-copied
///    string literal.
///
/// A non-string or unparsable value reads as `None`, matching
/// [`read_run_id`]'s defensive shape — never an error, never an empty
/// string.
pub(crate) fn read_campaign_id(ctx: &TaskContext) -> Option<String> {
    ctx.event
        .get("campaign_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            ctx.nodes
                .get(crate::workflows::orchestration::graph::NODE_NAME)
                .and_then(|node| node.get("campaign_id"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string)
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

    /// Drops `seeded_nodes` (EN.6.F task 4). The resume path MUST call this:
    /// a rehydrated `TaskContext` already carries the resolved policy under
    /// `policy::RESOLVED_POLICY_IDENTITY` (stamped once at the original
    /// dispatch), and a factory rebuilt for the resume must never
    /// re-seed/overwrite it — critical for `SDLC_FLOW`, whose factory
    /// re-resolves policy via `PolicyConfigSource::Worktree(cwd)` and would
    /// otherwise silently replace the run's original policy with whatever
    /// the resuming process's cwd resolves today.
    #[must_use]
    pub fn without_seeded_nodes(mut self) -> Self {
        self.seeded_nodes = HashMap::new();
        self
    }

    /// Pre-flight check for a resume point: `true` iff `identity` is
    /// registered in this workflow's node registry. Lets a resume handler
    /// turn an unresolvable `resume_at` into a 4xx before spawning the walk,
    /// rather than discovering it as a `WorkflowError` inside a spawned task
    /// nobody awaits.
    pub fn has_node(&self, identity: &str) -> bool {
        self.registry.contains(identity)
    }

    /// `entry().or_insert` only: adds `Pending` entries for schema nodes the
    /// rehydrated `ctx` has never heard of (schema drift between suspend and
    /// resume), and never clobbers an existing `NodeRun` — a resumed run's
    /// already-completed nodes must keep their original `started_at`/
    /// `completed_at` timestamps untouched.
    fn seed_missing_pending(&self, ctx: &mut TaskContext) {
        for identity in self.schema.nodes.keys() {
            ctx.node_runs.entry(identity.clone()).or_insert(NodeRun {
                status: NodeRunStatus::Pending,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            });
        }
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
        on_progress: OnProgress<'_>,
        options: RunOptions,
    ) -> Result<TaskContext, WorkflowError> {
        let mut ctx = self.seed_context(event);
        if let Some(run_id) = options.run_id {
            stamp_run_id(&mut ctx.metadata, run_id);
        }
        self.walk(
            ctx,
            Some(self.schema.start_node.clone()),
            BudgetLedger::new(),
            on_progress,
            options,
        )
        .await
    }

    /// Continue a suspended run (EN.6.F task 4): rehydrates `state.ctx`,
    /// seeds `Pending` `NodeRun`s for any schema node the rehydrated context
    /// has never heard of (schema drift), stamps the resumed marker, and
    /// walks forward from `state.at_identity` — never re-running an
    /// already-completed node, and never re-seeding `self.seeded_nodes`
    /// (call [`Workflow::without_seeded_nodes`] first if this workflow was
    /// rebuilt with a policy resolved for a *different* run).
    pub async fn run_from(
        &self,
        state: ResumeState,
        on_progress: OnProgress<'_>,
        options: RunOptions,
    ) -> Result<TaskContext, WorkflowError> {
        let mut ctx = state.ctx;
        self.seed_missing_pending(&mut ctx);
        suspend::stamp_resumed(&mut ctx.metadata);
        if let Some(run_id) = options.run_id {
            stamp_run_id(&mut ctx.metadata, run_id);
        }
        self.walk(
            ctx,
            Some(state.at_identity),
            state.ledger,
            on_progress,
            options,
        )
        .await
    }

    /// Build a fresh [`TaskContext`] for a new run: `event` seeds
    /// `TaskContext::event`, `self.seeded_nodes` seeds `ctx.nodes`, and every
    /// node declared in the schema is seeded PENDING in `node_runs` before
    /// anything runs. Extracted from `run_with` verbatim (EN.6.F task 3) so
    /// `run_from` can rehydrate a stored `TaskContext` instead of building a
    /// new one, while still sharing the walk loop below.
    fn seed_context(&self, event: serde_json::Value) -> TaskContext {
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

        ctx
    }

    /// The pointer-walk loop itself, extracted from `run_with` verbatim
    /// (EN.6.F task 3): starts at `current` (rather than always
    /// `self.schema.start_node`) and drives the given `ctx`/`ledger` forward
    /// node-by-node. This is the shared core both `run_with` (fresh context,
    /// fresh ledger, start node) and a future `run_from` (rehydrated context,
    /// rehydrated ledger, stored pointer) drive.
    #[tracing::instrument(
        name = "workflow.walk",
        skip_all,
        fields(run_id = tracing::field::Empty, campaign_id = tracing::field::Empty)
    )]
    async fn walk(
        &self,
        mut ctx: TaskContext,
        mut current: Option<String>,
        mut ledger: BudgetLedger,
        mut on_progress: OnProgress<'_>,
        options: RunOptions,
    ) -> Result<TaskContext, WorkflowError> {
        // Record the two instrumented fields (EN.11.I task 2) once, up front:
        // `run_id` is always available by this point (`run_with`/`run_from`
        // stamp it into `ctx.metadata` before calling `walk`); `campaign_id`
        // is available here for a child `SDLC_FLOW` run (carried on the
        // event from the start) but NOT yet for the parent `ORCHESTRATION`
        // run, which only learns its own campaign id as a side effect of its
        // single node's own `process()` — that run has one node, so there is
        // no second dispatch for a re-record to help. `read_campaign_id`
        // already treats "not known yet" as `None`, never a fabricated value.
        let span = tracing::Span::current();
        if let Some(run_id) = read_run_id(&ctx.metadata) {
            span.record("run_id", run_id.as_str());
        }
        if let Some(campaign_id) = read_campaign_id(&ctx) {
            span.record("campaign_id", campaign_id.as_str());
        }

        on_progress(&ctx);

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

            // Checked after cancellation and after the budget gate,
            // deliberately: a ledger already over cap should halt truthfully
            // (D6) rather than suspend into an immediate re-halt on resume.
            // `identity` is the node about to run and has NOT run yet, so it
            // becomes the resume point verbatim.
            if let Some(sig) = &options.pause_signal {
                if sig.is_paused() {
                    self.finish_suspended(
                        &mut ctx,
                        Some(identity.clone()),
                        SuspendReason::OperatorPause,
                        None,
                        &ledger,
                    );
                    stamp_run_telemetry(&mut ctx, &self.schema.start_node);
                    on_progress(&ctx);
                    return Ok(ctx);
                }
            }

            let node = self.registry.get(&identity).ok_or_else(|| {
                WorkflowError::new(format!("no node registered for identity '{identity}'"))
            })?;

            let (next_ctx, failed) = node_context(node, ctx, &mut on_progress).await;
            ctx = next_ctx;

            if failed {
                break;
            }

            // Routers choose their next identity at runtime via `route(ctx)`
            // (possibly an undeclared back-edge); plain nodes keep walking
            // the statically declared `connections[0]`. Resolved *after*
            // `node_context` so the router sees the context as it stood on
            // exit from this node — including anything this node's own
            // `process()` just stored under its own identity (e.g.
            // `content_pipeline::source_router::SourceRouterNode`, whose
            // `route()` reads back the envelope/policy its own `process()`
            // stamped this same walk step; EN.5.A task 13's e2e is the first
            // suite to drive a self-referential router through the real
            // walk loop and caught this — every other router in the
            // registry has a pure-passthrough `process()`, so this ordering
            // change is behavior-preserving for them: their route() only
            // ever reads a *different*, already-completed node's output).
            let router_next = node
                .as_router()
                .map(|router| crate::routing::dispatch_route(router, &ctx));

            if let Some(run) = ctx.node_runs.get(&identity) {
                let cost_usd = node_cost_usd(&ctx, &identity);
                ledger.record(run.usage.as_ref(), cost_usd);
            }

            current = match router_next.clone() {
                Some(next) => next,
                None => self.schema.next_after(&identity).map(str::to_string),
            };

            // A `SuspendNode` requested suspension from inside `process()`.
            // The resume pointer is the *successor* of the node that just
            // ran, already computed above as `current` — guarded so
            // suspending at the graph's last node simply completes the run
            // rather than suspending with an unresolvable `None` pointer (in
            // that case `current` is already `None` and the loop exits
            // normally below).
            if suspend::suspension_requested(&ctx.metadata) && current.is_some() {
                self.finish_suspended(
                    &mut ctx,
                    current.clone(),
                    SuspendReason::SuspendNode,
                    Some(&identity),
                    &ledger,
                );
                stamp_run_telemetry(&mut ctx, &self.schema.start_node);
                on_progress(&ctx);
                return Ok(ctx);
            }
        }

        stamp_run_telemetry(&mut ctx, &self.schema.start_node);
        Ok(ctx)
    }

    /// The single place that writes the suspension marker — where operator
    /// pause and `SuspendNode` become one thing (EN.6.F task 4). No-ops
    /// (leaves the run to complete rather than suspending) when `resume_at`
    /// is `None`, so a caller that hasn't already guarded for "nothing to
    /// resume to" stays safe.
    fn finish_suspended(
        &self,
        ctx: &mut TaskContext,
        resume_at: Option<String>,
        reason: SuspendReason,
        origin: Option<&str>,
        ledger: &BudgetLedger,
    ) {
        let Some(resume_at) = resume_at else {
            return;
        };
        stamp_suspended(
            &mut ctx.metadata,
            Suspension {
                resume_at: &resume_at,
                reason,
                origin_identity: origin,
                ledger,
            },
        );
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
pub(crate) fn node_cost_usd(ctx: &TaskContext, identity: &str) -> Option<f64> {
    ctx.nodes.get(identity)?.get("cost_usd")?.as_f64()
}

/// The framework-owned envelope around a single node's `process` call: stamps
/// `NodeRun` RUNNING + `started_at` on entry, then SUCCESS + `completed_at` on
/// `Ok` or FAILED + `completed_at` + `error` on `Err`, invoking `on_progress`
/// after each transition. Returns the updated `TaskContext` and whether the
/// node failed (so the caller knows to halt the walk).
///
/// `#[instrument]` here — not on `Node::process` itself (`EN.11.I` task 2):
/// this is the ONE dispatch site every node passes through, so instrumenting
/// it gives every node a span without touching any of the trait's ~6
/// in-tree impls or the growing set of workflow node types. The span
/// nests inside `Workflow::walk`'s span, so an event fired from inside
/// `node.process()` carries this span's `node` field alongside `walk`'s
/// `run_id`/`campaign_id` fields — `tracing`'s span-list formatting
/// attaches every ancestor span's recorded fields to a descendant event,
/// not just the immediate parent's.
///
/// **EN.11.I task 5:** the block's own acceptance criterion (`jq -e
/// 'select(.run_id==$ID) | .node'` returns every node of a run, in order)
/// reads `run_id`/`node` as TOP-LEVEL keys on each JSON line. `#[instrument]`
/// fields are only ever nested under the JSON formatter's `"span"`/`"spans"`
/// objects — there is no `tracing-subscriber` option that flattens a span's
/// *inherited* fields into an event's own top-level field set, only
/// `flatten_event` for an event's OWN fields (`engine_serve::init_tracing`
/// sets it). So each dispatch explicitly emits ONE event of its own —
/// `info!` on success, the existing `error!` on failure — carrying `node`
/// and `run_id` (and `campaign_id`, when the run has one) as literal event
/// fields, not merely relying on span inheritance. This is what task 2's
/// `instrumentation::CaptureLayer` deliberately did NOT pin (its own doc
/// comment says so) — it proves propagation; this proves the wire shape.
#[tracing::instrument(name = "workflow.node.dispatch", skip(node, ctx, on_progress), fields(node = %node.name()))]
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

    // Read once, from the pre-call snapshot, before `ctx` is moved into
    // `node.process`: both branches below need the same values, and
    // `read_run_id`/`read_campaign_id` are the same defensive readers
    // `Workflow::walk` uses (never a third implementation — task 2's own
    // rule still applies here). `Option<&str>` fields are OMITTED by
    // `tracing` when `None`, never recorded as an empty string — matching
    // task 2's `campaign_id` convention exactly.
    let run_id = read_run_id(&pre_call_ctx.metadata);
    let campaign_id = read_campaign_id(&pre_call_ctx);

    match node.process(ctx).await {
        Ok(mut ok_ctx) => {
            if let Some(run) = ok_ctx.node_runs.get_mut(&identity) {
                run.status = NodeRunStatus::Success;
                run.completed_at = Some(Utc::now());
            }
            tracing::info!(
                node = %identity,
                run_id = run_id.as_deref(),
                campaign_id = campaign_id.as_deref(),
                "node dispatched"
            );
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
            tracing::error!(
                node = %identity,
                run_id = run_id.as_deref(),
                campaign_id = campaign_id.as_deref(),
                error = %err.message,
                "node failed"
            );
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
            pause_signal: None,
            run_id: None,
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
            pause_signal: None,
            run_id: None,
        };

        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("a budget-halted run returns Ok, not Err");

        assert!(ctx.metadata.get(BUDGET_METADATA_KEY).is_some());
        stamped_run_telemetry(&ctx);
    }

    // -- EN.6.J task 2: run_id plumbing via RunOptions ---------------------

    #[tokio::test]
    async fn run_with_stamps_run_id_when_provided() {
        let workflow = two_node_workflow();
        let run_id = Uuid::new_v4();

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: None,
            run_id: Some(run_id),
        };

        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("run should succeed");

        assert_eq!(read_run_id(&ctx.metadata), Some(run_id.to_string()));
    }

    #[tokio::test]
    async fn run_with_default_options_stamps_no_run_id() {
        let workflow = two_node_workflow();

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, RunOptions::default())
            .await
            .expect("run should succeed");

        // `RunOptions::default()` (no run_id) leaves the run_id key entirely
        // absent -- existing metadata (e.g. `run_telemetry`, stamped by every
        // run regardless) is otherwise untouched by this knob.
        assert!(ctx.metadata.get(RUN_ID_METADATA_KEY).is_none());
        assert_eq!(read_run_id(&ctx.metadata), None);
    }

    #[test]
    fn read_run_id_returns_none_for_empty_or_non_object_metadata() {
        assert_eq!(read_run_id(&serde_json::json!({})), None);
        assert_eq!(read_run_id(&serde_json::json!(null)), None);
        assert_eq!(read_run_id(&serde_json::json!("not an object")), None);
    }

    // -- EN.6.F task 4: run_from, the pause check, finish_suspended -------

    /// A node whose `process()` calls `suspend::request_suspension` on its
    /// own way out, exercising the post-node `SuspendNode` finalization path
    /// without depending on the real `SuspendNode` (a later task).
    struct RequestSuspendNode;

    #[async_trait::async_trait]
    impl Node for RequestSuspendNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            crate::suspend::request_suspension(&mut ctx.metadata);
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "RequestSuspendNode"
        }
    }

    #[test]
    fn run_options_default_has_no_pause_signal() {
        let options = RunOptions::default();
        assert!(options.pause_signal.is_none());
    }

    #[tokio::test]
    async fn paused_signal_stops_the_walk_at_the_loop_top() {
        let workflow = two_node_workflow();

        let signal = PauseSignal::new();
        signal.pause();

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: Some(signal),
            run_id: None,
        };

        let ctx = workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("a paused run returns Ok, not Err");

        // The next node ("start_node") never dispatched -- still Pending.
        assert_eq!(
            ctx.node_runs.get("start_node").unwrap().status,
            NodeRunStatus::Pending
        );
        assert_eq!(
            ctx.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );

        let suspension =
            crate::suspend::read_suspension(&ctx.metadata).expect("suspension marker present");
        assert!(suspension.suspended);
        assert_eq!(suspension.resume_at.as_deref(), Some("start_node"));
        assert_eq!(
            suspension.reason,
            Some(crate::suspend::SuspendReason::OperatorPause)
        );
    }

    /// `start -> RequestSuspendNode -> SuccessNode` (terminal): the requesting
    /// node is not the last node, so `resume_at` should be its declared
    /// successor.
    fn suspend_then_success_workflow() -> Workflow {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(RequestSuspendNode));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "RequestSuspendNode".to_string(),
            crate::schema::NodeConfig::new("RequestSuspendNode", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            crate::schema::NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "RequestSuspendNode", nodes);
        Workflow::new(registry, schema)
    }

    #[tokio::test]
    async fn suspension_requested_after_a_node_yields_its_successor_as_resume_at() {
        let workflow = suspend_then_success_workflow();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let ctx = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .expect("run should return Ok when it suspends");

        assert_eq!(
            ctx.node_runs.get("RequestSuspendNode").unwrap().status,
            NodeRunStatus::Success
        );
        assert_eq!(
            ctx.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Pending
        );

        let suspension =
            crate::suspend::read_suspension(&ctx.metadata).expect("suspension marker present");
        assert!(suspension.suspended);
        assert_eq!(suspension.resume_at.as_deref(), Some("SuccessNode"));
        assert_eq!(
            suspension.reason,
            Some(crate::suspend::SuspendReason::SuspendNode)
        );
        assert_eq!(
            suspension.origin_identity.as_deref(),
            Some("RequestSuspendNode")
        );
    }

    #[tokio::test]
    async fn suspension_requested_at_the_last_node_completes_the_run_instead() {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(RequestSuspendNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "RequestSuspendNode".to_string(),
            crate::schema::NodeConfig::new("RequestSuspendNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "RequestSuspendNode", nodes);
        let workflow = Workflow::new(registry, schema);

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});

        let ctx = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .expect("run should complete normally");

        assert_eq!(
            ctx.node_runs.get("RequestSuspendNode").unwrap().status,
            NodeRunStatus::Success
        );
        assert!(
            !crate::suspend::is_suspended(&ctx.metadata),
            "suspending at the last node must complete the run, not suspend it"
        );
    }

    #[tokio::test]
    async fn run_from_starts_at_the_given_identity_without_re_seeding_completed_nodes() {
        let workflow = two_node_workflow();

        // Simulate a rehydrated ctx: `start_node` already completed, its
        // NodeRun timestamps fixed and distinguishable from "just now".
        let fixed_completed_at = Utc::now() - chrono::Duration::hours(1);
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        ctx.node_runs.insert(
            "start_node".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: Some(fixed_completed_at),
                completed_at: Some(fixed_completed_at),
                error: None,
                input: None,
                usage: None,
            },
        );

        let state = ResumeState {
            ctx,
            at_identity: "SuccessNode".to_string(),
            ledger: BudgetLedger::new(),
        };

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let out = workflow
            .run_from(state, on_progress, RunOptions::default())
            .await
            .expect("run_from should complete");

        // The already-completed node's timestamps are untouched -- it was
        // never re-run.
        let start_run = out.node_runs.get("start_node").unwrap();
        assert_eq!(start_run.status, NodeRunStatus::Success);
        assert_eq!(start_run.completed_at, Some(fixed_completed_at));

        // Resume actually ran the target node.
        assert_eq!(
            out.node_runs.get("SuccessNode").unwrap().status,
            NodeRunStatus::Success
        );
        assert!(out.nodes.contains_key("SuccessNode"));

        // `run_from` seeds missing schema nodes as Pending via
        // `seed_missing_pending` -- here every schema node was already
        // present, so nothing new was added, but the call must not panic
        // or clobber the existing entries either.
        assert_eq!(out.node_runs.len(), 2);
    }

    #[tokio::test]
    async fn run_from_stamps_run_id_after_stamp_resumed() {
        let workflow = two_node_workflow();
        let run_id = Uuid::new_v4();

        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };

        let state = ResumeState {
            ctx,
            at_identity: "start_node".to_string(),
            ledger: BudgetLedger::new(),
        };

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: None,
            run_id: Some(run_id),
        };
        let out = workflow
            .run_from(state, on_progress, options)
            .await
            .expect("run_from should complete");

        assert_eq!(read_run_id(&out.metadata), Some(run_id.to_string()));
    }

    #[tokio::test]
    async fn without_seeded_nodes_clears_seeded_nodes_and_has_node_reports_registry_membership() {
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

        assert!(workflow.has_node("SuccessNode"));
        assert!(!workflow.has_node("NoSuchNode"));

        let workflow = workflow.without_seeded_nodes();

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let ctx = workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .expect("run should succeed");

        // The seeded "X" entry is gone -- without_seeded_nodes took effect.
        assert!(!ctx.nodes.contains_key("X"));
    }

    // -- EN.11.I task 2: run_id/campaign_id span instrumentation -----------

    mod instrumentation {
        //! A minimal `tracing_subscriber::Layer` that records, for every
        //! emitted event, the union of that event's own fields with every
        //! ancestor span's recorded fields (walking root-to-leaf so a
        //! descendant span's field can shadow an ancestor's same-named
        //! field, matching how the real JSON formatter's `spans` list
        //! reads). This is deliberately independent of the JSON wire shape
        //! task 5 pins — it proves span-field *propagation*, which is this
        //! task's job, without depending on a formatter's exact output.
        use super::*;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::layer::Context;
        use tracing_subscriber::registry::LookupSpan;
        use tracing_subscriber::Layer;

        #[derive(Default, Clone)]
        struct FieldMap(HashMap<String, String>);

        impl Visit for FieldMap {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.0
                    .insert(field.name().to_string(), format!("{value:?}"));
            }

            fn record_str(&mut self, field: &Field, value: &str) {
                self.0.insert(field.name().to_string(), value.to_string());
            }
        }

        /// Every event this layer has observed, in emission order, as its
        /// fully-resolved (span-inherited) field map.
        #[derive(Default)]
        pub(super) struct Captured(Mutex<Vec<HashMap<String, String>>>);

        impl Captured {
            pub(super) fn snapshot(&self) -> Vec<HashMap<String, String>> {
                self.0.lock().unwrap().clone()
            }
        }

        pub(super) struct CaptureLayer(pub(super) Arc<Captured>);

        impl<S> Layer<S> for CaptureLayer
        where
            S: tracing::Subscriber + for<'a> LookupSpan<'a>,
        {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                id: &tracing::span::Id,
                ctx: Context<'_, S>,
            ) {
                let span = ctx.span(id).expect("span must exist in on_new_span");
                let mut fields = FieldMap::default();
                attrs.record(&mut fields);
                span.extensions_mut().insert(fields);
            }

            fn on_record(
                &self,
                id: &tracing::span::Id,
                values: &tracing::span::Record<'_>,
                ctx: Context<'_, S>,
            ) {
                let span = ctx.span(id).expect("span must exist in on_record");
                let mut extensions = span.extensions_mut();
                if let Some(fields) = extensions.get_mut::<FieldMap>() {
                    values.record(fields);
                }
            }

            fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
                let mut merged = HashMap::new();
                if let Some(scope) = ctx.event_scope(event) {
                    for span in scope.from_root() {
                        if let Some(fields) = span.extensions().get::<FieldMap>() {
                            merged.extend(fields.0.clone());
                        }
                    }
                }
                let mut own = FieldMap::default();
                event.record(&mut own);
                merged.extend(own.0);
                self.0 .0.lock().unwrap().push(merged);
            }
        }

        /// Sets `subscriber` as the default for the CURRENT thread only
        /// (`tracing::subscriber::set_default`, not the process-wide
        /// `set_global_default`) so these tests never collide with each
        /// other or with `engine_serve::init_tracing`'s own
        /// idempotent-global-install test — `cargo nextest run`'s
        /// process-per-test model (CLAUDE.md standing rule 7) makes even a
        /// global install safe here, but a thread-local default is the
        /// correct scope regardless of test runner.
        pub(super) fn capturing() -> (Arc<Captured>, tracing::subscriber::DefaultGuard) {
            let captured = Arc::new(Captured::default());
            let layer = CaptureLayer(captured.clone());
            let subscriber = tracing_subscriber::registry().with(layer);
            let guard = tracing::subscriber::set_default(subscriber);
            (captured, guard)
        }
    }

    use tracing_subscriber::layer::SubscriberExt;

    /// A node whose `process()` emits one `tracing::info!` event, tagged
    /// with a `marker` field — used to prove that an event fired from
    /// *inside* a node's own `process()` inherits the ancestor spans'
    /// recorded fields (`workflow.walk`'s `run_id`/`campaign_id` and
    /// `workflow.node.dispatch`'s `node`) without doing anything
    /// instrumentation-aware itself.
    struct EventEmittingNode;

    #[async_trait::async_trait]
    impl Node for EventEmittingNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            tracing::info!(marker = "node-ran", "EventEmittingNode processed");
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "EventEmittingNode"
        }
    }

    #[tokio::test]
    async fn walk_span_attaches_run_id_and_node_to_events_emitted_by_a_node() {
        let (captured, _guard) = instrumentation::capturing();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(EventEmittingNode));
        let mut nodes = HashMap::new();
        nodes.insert(
            "EventEmittingNode".to_string(),
            crate::schema::NodeConfig::new("EventEmittingNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "EventEmittingNode", nodes);
        let workflow = Workflow::new(registry, schema);

        let run_id = Uuid::new_v4();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: None,
            run_id: Some(run_id),
        };
        workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("run should succeed");

        let events = captured.snapshot();
        let node_event = events
            .iter()
            .find(|f| f.get("marker").map(String::as_str) == Some("node-ran"))
            .expect("EventEmittingNode's event must have been captured");

        // The two fields this task instruments: `run_id` (recorded on
        // `Workflow::walk`'s span) and `node` (recorded on
        // `node_context`'s per-dispatch span) both reach an event fired
        // from deep inside a plain node's own `process()` — proving the
        // span hierarchy, not just direct field passing, does the work.
        assert_eq!(
            node_event.get("run_id").map(String::as_str),
            Some(run_id.to_string().as_str())
        );
        assert_eq!(
            node_event.get("node").map(String::as_str),
            Some("EventEmittingNode")
        );
    }

    #[tokio::test]
    async fn walk_span_attaches_campaign_id_when_the_event_carries_one() {
        let (captured, _guard) = instrumentation::capturing();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(EventEmittingNode));
        let mut nodes = HashMap::new();
        nodes.insert(
            "EventEmittingNode".to_string(),
            crate::schema::NodeConfig::new("EventEmittingNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "EventEmittingNode", nodes);
        let workflow = Workflow::new(registry, schema);

        let campaign_id = Uuid::new_v4();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        workflow
            .run(
                serde_json::json!({ "campaign_id": campaign_id.to_string() }),
                on_progress,
            )
            .await
            .expect("run should succeed");

        let events = captured.snapshot();
        let node_event = events
            .iter()
            .find(|f| f.get("marker").map(String::as_str) == Some("node-ran"))
            .expect("EventEmittingNode's event must have been captured");

        assert_eq!(
            node_event.get("campaign_id").map(String::as_str),
            Some(campaign_id.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn a_run_with_no_campaign_never_stamps_an_empty_campaign_id_field() {
        let (captured, _guard) = instrumentation::capturing();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(EventEmittingNode));
        let mut nodes = HashMap::new();
        nodes.insert(
            "EventEmittingNode".to_string(),
            crate::schema::NodeConfig::new("EventEmittingNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "EventEmittingNode", nodes);
        let workflow = Workflow::new(registry, schema);

        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        workflow
            .run(serde_json::json!({}), on_progress)
            .await
            .expect("run should succeed");

        let events = captured.snapshot();
        let node_event = events
            .iter()
            .find(|f| f.get("marker").map(String::as_str) == Some("node-ran"))
            .expect("EventEmittingNode's event must have been captured");

        // `Empty` fields that were never `record`ed are simply absent from
        // the map — never present-but-empty-string. This is the negative
        // control: it would fail exactly the way a stray
        // `campaign_id = ""` regression would, and it currently passes
        // because `read_campaign_id` correctly returns `None` here.
        assert!(!node_event.contains_key("campaign_id"));
    }

    /// A node whose `process()` crosses the `spawn_blocking` boundary
    /// itself, exactly the way `OrchestrationRunNode::process`
    /// (`crate::workflows::orchestration::graph`) does: capture the calling
    /// thread's current span AND dispatcher, move both into the blocking
    /// closure, and re-establish both there before emitting anything. This
    /// exercises the identical propagation pattern this task added to
    /// `graph.rs`'s real `spawn_blocking` call site, without needing that
    /// node's much heavier `RepoRegistry`/roadmap-dir/lane-chain setup.
    struct SpawnBlockingEventNode;

    #[async_trait::async_trait]
    impl Node for SpawnBlockingEventNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            let current_span = tracing::Span::current();
            let current_dispatch = tracing::dispatcher::get_default(|d| d.clone());
            tokio::task::spawn_blocking(move || {
                tracing::dispatcher::with_default(&current_dispatch, || {
                    let _guard = current_span.enter();
                    tracing::info!(marker = "blocking-ran", "emitted inside spawn_blocking");
                });
            })
            .await
            .map_err(|err| NodeError::new(format!("blocking task panicked: {err}")))?;

            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SpawnBlockingEventNode"
        }
    }

    /// The sibling to [`SpawnBlockingEventNode`] with the propagation
    /// DELIBERATELY omitted — neither the span nor the dispatcher is
    /// carried across the `spawn_blocking` boundary. This is what proves
    /// the positive test below is actually sensitive to the fix rather
    /// than passing for an unrelated reason: without propagation, the
    /// blocking closure's event either never reaches the test's
    /// thread-local subscriber (no dispatcher) or reaches it with an
    /// empty span context (no span) — either way, `run_id` comes back
    /// absent.
    struct SpawnBlockingEventNodeWithoutPropagation;

    #[async_trait::async_trait]
    impl Node for SpawnBlockingEventNodeWithoutPropagation {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            tokio::task::spawn_blocking(move || {
                tracing::info!(marker = "blocking-ran-unpropagated", "no context carried");
            })
            .await
            .map_err(|err| NodeError::new(format!("blocking task panicked: {err}")))?;

            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SpawnBlockingEventNodeWithoutPropagation"
        }
    }

    #[tokio::test]
    async fn spawn_blocking_propagation_keeps_run_id_on_an_event_fired_from_the_blocking_thread() {
        let (captured, _guard) = instrumentation::capturing();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SpawnBlockingEventNode));
        let mut nodes = HashMap::new();
        nodes.insert(
            "SpawnBlockingEventNode".to_string(),
            crate::schema::NodeConfig::new("SpawnBlockingEventNode", vec![]),
        );
        let schema = WorkflowSchema::new("single", "SpawnBlockingEventNode", nodes);
        let workflow = Workflow::new(registry, schema);

        let run_id = Uuid::new_v4();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: None,
            run_id: Some(run_id),
        };
        workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("run should succeed");

        let events = captured.snapshot();
        let blocking_event = events
            .iter()
            .find(|f| f.get("marker").map(String::as_str) == Some("blocking-ran"))
            .expect(
                "the spawn_blocking closure's event must have reached the test subscriber \
                 via the propagated dispatcher",
            );

        assert_eq!(
            blocking_event.get("run_id").map(String::as_str),
            Some(run_id.to_string().as_str())
        );
        assert_eq!(
            blocking_event.get("node").map(String::as_str),
            Some("SpawnBlockingEventNode")
        );
    }

    /// The negative control for the test above: proves the positive
    /// assertion is actually exercising the propagation fix, not merely
    /// something that would pass regardless. WITHOUT the span/dispatcher
    /// carried across `spawn_blocking`, the blocking closure's event never
    /// reaches this thread-local test subscriber at all (no propagated
    /// dispatcher), so nothing with `marker == "blocking-ran-unpropagated"`
    /// is ever captured — exactly the "silently-empty" failure mode this
    /// task exists to prevent, caught here rather than assumed.
    #[tokio::test]
    async fn without_propagation_the_blocking_events_context_is_lost() {
        let (captured, _guard) = instrumentation::capturing();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(SpawnBlockingEventNodeWithoutPropagation));
        let mut nodes = HashMap::new();
        nodes.insert(
            "SpawnBlockingEventNodeWithoutPropagation".to_string(),
            crate::schema::NodeConfig::new("SpawnBlockingEventNodeWithoutPropagation", vec![]),
        );
        let schema =
            WorkflowSchema::new("single", "SpawnBlockingEventNodeWithoutPropagation", nodes);
        let workflow = Workflow::new(registry, schema);

        let run_id = Uuid::new_v4();
        let on_progress: OnProgress<'_> = Box::new(|_c: &TaskContext| {});
        let options = RunOptions {
            cancellation_token: None,
            budget: None,
            pause_signal: None,
            run_id: Some(run_id),
        };
        workflow
            .run_with(serde_json::json!({}), on_progress, options)
            .await
            .expect("run should succeed");

        let events = captured.snapshot();
        assert!(
            !events
                .iter()
                .any(|f| f.get("marker").map(String::as_str) == Some("blocking-ran-unpropagated")),
            "an event fired without span/dispatcher propagation should never reach the \
             thread-local test subscriber set on the ORIGINAL thread — this pins the exact \
             failure mode the propagation fix in graph.rs's spawn_blocking call site prevents"
        );
    }
}
