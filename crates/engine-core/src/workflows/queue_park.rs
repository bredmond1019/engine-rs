//! `workflows::queue_park` — EN.17.J task 4: the run/park/await/inject/resume
//! loop that makes a `SuspendReason::HeavyWorkQueue` suspension (EN.17.J
//! tasks 1-3) transparent to a caller that just wants the workflow to run to
//! completion, rather than a terminal state it must itself know how to
//! un-park.
//!
//! Every OTHER suspension reason (`OperatorPause`, `SuspendNode`) really is a
//! stopping point today — an operator or an orchestrator decides if/when to
//! resume it, on its own schedule, possibly never. `HeavyWorkQueue` is
//! different: nothing but the parked job's own completion should end the
//! park, and the caller (an `ORCHESTRATION` child's `FlowRunner`, or
//! `engine-serve`'s `spawn_run`) wants exactly the same "runs to completion"
//! contract `Workflow::run_with`/`run_from` already promise for every other
//! case. [`drive`] is that contract, restored: it wraps `run_with`/`run_from`
//! and, on every `HeavyWorkQueue` suspension, awaits the parked job through
//! the injected [`HeavyJobLookup`] seam and loops back into `run_from` at the
//! suspension's own `resume_at` — instead of handing the caller a suspended
//! ctx it would otherwise have to know to treat specially. A non-queue-park
//! suspension (or no suspension at all, or a structural `WorkflowError`)
//! passes straight through untouched: `drive` is a strict superset of
//! calling `run_with`/`run_from` directly, never a replacement with
//! different behavior for the cases those already handle.
//!
//! **`HeavyJobLookup` is an injectable seam, not the concrete
//! `coord::heavy_work::HeavyWorkQueue`.** This module's own unit tests below
//! drive the loop end-to-end against an in-file stub — no real admission, no
//! filesystem, no `brain.toml` — matching this task's own acceptance
//! criteria. Bridging a real job produced by `TestTaskNode`
//! (`workflows::sdlc_flow::task_loop`) into this seam — and, on the
//! production side, actually carrying that job's real check-run output
//! through to the point `drive` can look it up (today's `TestTaskNode`
//! discards its own `JobHandle` immediately after submitting, EN.17.J task 3)
//! — is a follow-on concern for the callers that actually own a
//! `HeavyWorkQueue`-backed job (EN.17.J tasks 5/6), outside this task's own
//! declared files.

use std::sync::Arc;

use async_trait::async_trait;
use engine_contract::TaskContext;
use uuid::Uuid;

use crate::budget::BudgetLedger;
use crate::cancellation::{stamp_cancelled, CancellationToken};
use crate::suspend::{self, SuspendReason};
use crate::workflow::{OnProgress, ResumeState, RunOptions, Workflow, WorkflowError};

/// The `ctx.nodes` key `TestTaskNode` writes its output under, inline or
/// queue-parked alike (EN.17.J task 3) — the same identity string
/// `put_result(&mut ctx, "TestTaskNode", ...)` uses in
/// `workflows::sdlc_flow::task_loop`.
const TEST_TASK_NODE_IDENTITY: &str = "TestTaskNode";

/// The `ctx.metadata` key under which a queue-parked job's correlation
/// bookkeeping lives — `{"job_id": ..., "class": ..., "state": ...}`,
/// stamped by `TestTaskNode` under `TestDispatch::QueuePark` (EN.17.J task
/// 3). `drive` reads `job_id` from here and, on the job's completion,
/// rewrites `state` to `"done"`.
const HEAVY_WORK_METADATA_KEY: &str = "heavy_work";

/// Where a queue-parked job's outcome is looked up once [`drive`] starts
/// awaiting it, and where a still-`Queued` job is told to stop before it is
/// ever admitted. Injectable — decoupled from `coord::heavy_work`'s on-disk
/// job store — so this module's own unit tests can drive the loop against a
/// fully in-process stub, and so a future production bridge over the real
/// `HeavyWorkQueue` can implement this same seam without `queue_park`
/// depending on its admission/filesystem internals.
#[async_trait]
pub trait HeavyJobLookup: Send + Sync {
    /// Waits for `job_id` to reach a terminal state and returns the JSON to
    /// write under `ctx.nodes["TestTaskNode"]` — the SAME key set an inline
    /// `TestTaskNode` output carries (`all_passed`, `check_results`,
    /// `failure_summary`, `test_depth`, `check_source`, `excluded_checks`,
    /// `heavy_work`). Never returns before the job actually finishes; a
    /// caller that wants to give up early races this against cancellation
    /// instead (see [`drive`]'s own `tokio::select!`).
    async fn await_outcome(&self, job_id: Uuid) -> serde_json::Value;

    /// Marks `job_id` cancelled IF AND ONLY IF it is still `Queued` (never
    /// admitted) — the job's own work must never run once this returns for
    /// a job that was still queued. A no-op for a job that has already been
    /// admitted (its work is already running and this seam has no way to
    /// interrupt it mid-flight) or has already reached a terminal state.
    async fn cancel_if_queued(&self, job_id: Uuid);
}

/// Which starting point [`drive`] begins (or resumes) a walk from — the same
/// two-way fork `engine-serve::suspend::RunStart` already models for a
/// spawned run's fresh-trigger/resume split (`Fresh`/`Resume`), reused here
/// so a caller holding either one hands it to `drive` with no translation
/// beyond bundling in the `on_progress`/`options`/`heavy_work` seams
/// `run_with`/`run_from` would otherwise take as separate arguments.
pub enum RunStart<'a> {
    /// A brand-new run: everything [`Workflow::run_with`] needs.
    Fresh {
        event: serde_json::Value,
        on_progress: OnProgress<'a>,
        options: RunOptions,
        /// The shared queue [`drive`] looks up a queue-parked job against,
        /// for every suspension this run (and every resume it drives itself
        /// through) produces.
        heavy_work: Arc<dyn HeavyJobLookup>,
    },
    /// A resume: everything [`Workflow::run_from`] needs.
    Resume {
        state: ResumeState,
        on_progress: OnProgress<'a>,
        options: RunOptions,
        heavy_work: Arc<dyn HeavyJobLookup>,
    },
}

/// `RunOptions` carries no `Clone` derive (EN.6.F), but every one of its
/// fields individually is `Clone`/`Copy` — `drive`'s loop needs a fresh
/// `RunOptions` for each `run_with`/`run_from` call it makes across
/// potentially many queue-park segments, while the original stays available
/// for the next segment.
fn clone_options(options: &RunOptions) -> RunOptions {
    RunOptions {
        cancellation_token: options.cancellation_token.clone(),
        budget: options.budget,
        pause_signal: options.pause_signal.clone(),
        run_id: options.run_id,
    }
}

/// Re-borrows an owned `OnProgress` so it can be handed to `run_with`/
/// `run_from` more than once across `drive`'s own loop. Each call needs its
/// own owned `Box<dyn FnMut>`, but the underlying sink (the caller's real
/// persistence/SSE fan-out, or a no-op) must stay the SAME one across every
/// segment of the run — never rebuilt per segment, and never silently
/// dropped between segments the way a fresh `Box::new(|_| {})` per call
/// would.
fn reborrow<'b, 'a: 'b>(on_progress: &'b mut OnProgress<'a>) -> OnProgress<'b> {
    Box::new(move |ctx: &TaskContext| on_progress(ctx))
}

/// Read `job_id` back from a ctx's `metadata.heavy_work.job_id`, as stamped
/// by `TestTaskNode` under `TestDispatch::QueuePark` (EN.17.J task 3).
/// `None` on absent/malformed metadata — a defensive read, never a panic.
fn read_job_id(ctx: &TaskContext) -> Option<Uuid> {
    ctx.metadata
        .get(HEAVY_WORK_METADATA_KEY)
        .and_then(|v| v.get("job_id"))
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Run (or resume) `workflow` via `start`, then keep driving it through every
/// `SuspendReason::HeavyWorkQueue` suspension it produces — looking each
/// parked job up on `heavy_work`, racing that against `cancel`, injecting the
/// outcome, and resuming at the suspension's own `resume_at` — until the
/// walk reaches a terminal state (success, failure, budget halt, operator
/// pause, a `SuspendNode` suspension, or this call's own cancellation).
///
/// A non-queue-park suspension (or no suspension at all) is returned exactly
/// as `run_with`/`run_from` produced it — no interaction with `heavy_work`
/// is ever attempted for those cases. A structural `WorkflowError` (an
/// unresolvable node identity, surfaced by `run_with`/`run_from` themselves)
/// propagates immediately via `?`, same as calling either directly.
pub async fn drive<'a>(
    workflow: &Workflow,
    start: RunStart<'a>,
    cancel: &CancellationToken,
) -> Result<TaskContext, WorkflowError> {
    let (mut ctx, mut on_progress, options, heavy_work) = match start {
        RunStart::Fresh {
            event,
            mut on_progress,
            options,
            heavy_work,
        } => {
            let call_options = clone_options(&options);
            let ctx = workflow
                .run_with(event, reborrow(&mut on_progress), call_options)
                .await?;
            (ctx, on_progress, options, heavy_work)
        }
        RunStart::Resume {
            state,
            mut on_progress,
            options,
            heavy_work,
        } => {
            let call_options = clone_options(&options);
            let ctx = workflow
                .run_from(state, reborrow(&mut on_progress), call_options)
                .await?;
            (ctx, on_progress, options, heavy_work)
        }
    };

    loop {
        // Not suspended at all (success, failure, budget halt) -- done.
        let Some(suspension) = suspend::read_suspension(&ctx.metadata) else {
            return Ok(ctx);
        };
        if !suspension.suspended {
            return Ok(ctx);
        }

        // A non-queue-park suspension (operator pause, a `SuspendNode`) is
        // this call's own terminal state -- pass it through untouched, and
        // never touch `heavy_work` for it.
        if suspension.reason != Some(SuspendReason::HeavyWorkQueue) {
            return Ok(ctx);
        }

        // Both of these should always be present for a real
        // `HeavyWorkQueue` suspension (`TestTaskNode` stamps both before
        // requesting it) -- but a malformed/foreign marker fails closed by
        // returning the parked ctx unchanged rather than panicking or
        // looping forever.
        let Some(resume_at) = suspension.resume_at.clone() else {
            return Ok(ctx);
        };
        let Some(job_id) = read_job_id(&ctx) else {
            return Ok(ctx);
        };

        tokio::select! {
            outcome = heavy_work.await_outcome(job_id) => {
                ctx.nodes.insert(TEST_TASK_NODE_IDENTITY.to_string(), outcome);
                ctx.metadata[HEAVY_WORK_METADATA_KEY]["state"] = serde_json::json!("done");
                suspend::stamp_resumed(&mut ctx.metadata);

                let ledger = suspension
                    .ledger
                    .map(|snap| BudgetLedger::from_parts(snap.total_tokens, snap.total_cost_usd))
                    .unwrap_or_else(|| BudgetLedger::from_context(&ctx));

                let resume_state = ResumeState {
                    ctx,
                    at_identity: resume_at,
                    ledger,
                };
                let call_options = clone_options(&options);
                ctx = workflow
                    .run_from(resume_state, reborrow(&mut on_progress), call_options)
                    .await?;
            }
            _ = cancel.cancelled() => {
                // Still queued (never admitted) -- the job's own work must
                // never run for it. Already-admitted/finished jobs are a
                // no-op here; either way this call's own loop stops now.
                heavy_work.cancel_if_queued(job_id).await;
                stamp_cancelled(&mut ctx.metadata);
                return Ok(ctx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, NodeError, NodeRegistry};
    use crate::schema::{NodeConfig, WorkflowSchema};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::sync::Notify;

    // -- Test nodes -----------------------------------------------------

    /// Requests suspension with a non-`HeavyWorkQueue` reason on its way
    /// out -- the "this suspension isn't ours" case `drive` must pass
    /// through untouched.
    struct RequestOperatorPauseNode;

    #[async_trait::async_trait]
    impl Node for RequestOperatorPauseNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            suspend::request_suspension_with_reason(
                &mut ctx.metadata,
                SuspendReason::OperatorPause,
            );
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "RequestOperatorPauseNode"
        }
    }

    /// Requests a `HeavyWorkQueue` suspension mirroring `TestTaskNode`'s own
    /// EN.17.J task 3 write shape: `ctx.metadata.heavy_work = {job_id,
    /// class, state: "queued"}`, then `request_suspension_with_reason`. Uses
    /// a FIXED job id (rather than minting a fresh one per call, as the real
    /// `TestTaskNode` does) so this file's own tests can pre-wire a stub
    /// queue to resolve/cancel that exact id.
    struct RequestHeavyWorkQueueNode {
        job_id: Uuid,
    }

    #[async_trait::async_trait]
    impl Node for RequestHeavyWorkQueueNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes.insert(
                self.name().to_string(),
                serde_json::json!({ "queued": true, "job_id": self.job_id.to_string() }),
            );
            ctx.metadata[HEAVY_WORK_METADATA_KEY] = serde_json::json!({
                "job_id": self.job_id.to_string(),
                "class": "test",
                "state": "queued",
            });
            suspend::request_suspension_with_reason(
                &mut ctx.metadata,
                SuspendReason::HeavyWorkQueue,
            );
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "RequestHeavyWorkQueueNode"
        }
    }

    /// The terminal node every test schema below resumes into.
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

    // -- Schema builders --------------------------------------------------

    fn operator_pause_workflow() -> Workflow {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(RequestOperatorPauseNode));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "RequestOperatorPauseNode".to_string(),
            NodeConfig::new("RequestOperatorPauseNode", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "RequestOperatorPauseNode", nodes);
        Workflow::new(registry, schema)
    }

    fn heavy_work_queue_workflow(job_id: Uuid) -> Workflow {
        let mut registry = NodeRegistry::new();
        registry.register(Box::new(RequestHeavyWorkQueueNode { job_id }));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "RequestHeavyWorkQueueNode".to_string(),
            NodeConfig::new("RequestHeavyWorkQueueNode", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "RequestHeavyWorkQueueNode", nodes);
        Workflow::new(registry, schema)
    }

    fn noop_progress<'a>() -> OnProgress<'a> {
        Box::new(|_ctx: &TaskContext| {})
    }

    // -- Stub `HeavyJobLookup` -------------------------------------------

    /// A test-only stand-in for the real `coord::heavy_work::HeavyWorkQueue`:
    /// exactly the two operations `drive` needs from "the shared
    /// HeavyWorkQueue" -- awaiting one job's outcome, and cancelling one
    /// that never got admitted -- with none of the real queue's on-disk
    /// admission machinery. `outcome_ready` is a `Notify` a test fires to
    /// simulate the job's completion arriving strictly AFTER `drive` has
    /// already started awaiting it (proving a real await, not a poll that
    /// happens to already have an answer).
    struct StubQueue {
        job_id: Uuid,
        outcome: serde_json::Value,
        outcome_ready: Notify,
        /// Set once `await_outcome` for `job_id` actually resolves -- this
        /// module's stand-in for "the job's work closure ran".
        resolved: AtomicBool,
        /// Set if this job is still `Queued` when `cancel_if_queued` fires.
        cancelled_while_queued: AtomicBool,
        /// Number of times `await_outcome` was invoked at all -- lets the
        /// "no queue interaction attempted" test assert zero calls.
        await_calls: AtomicUsize,
    }

    impl StubQueue {
        fn new(job_id: Uuid, outcome: serde_json::Value) -> Arc<Self> {
            Arc::new(Self {
                job_id,
                outcome,
                outcome_ready: Notify::new(),
                resolved: AtomicBool::new(false),
                cancelled_while_queued: AtomicBool::new(false),
                await_calls: AtomicUsize::new(0),
            })
        }

        /// Simulates the queued job finishing -- a test calls this only
        /// after confirming `drive` has already begun its own await.
        fn complete(&self) {
            self.outcome_ready.notify_one();
        }
    }

    #[async_trait]
    impl HeavyJobLookup for StubQueue {
        async fn await_outcome(&self, job_id: Uuid) -> serde_json::Value {
            self.await_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                job_id, self.job_id,
                "must look up the id drive read from ctx.metadata"
            );
            self.outcome_ready.notified().await;
            self.resolved.store(true, Ordering::SeqCst);
            self.outcome.clone()
        }

        async fn cancel_if_queued(&self, job_id: Uuid) {
            assert_eq!(job_id, self.job_id);
            // In this stub, "still queued" means `await_outcome` has not
            // yet resolved -- there is no separate admission state to
            // model, since this seam has no notion of admission at all.
            if !self.resolved.load(Ordering::SeqCst) {
                self.cancelled_while_queued.store(true, Ordering::SeqCst);
            }
        }
    }

    /// A `HeavyJobLookup` that panics on ANY call -- the positive proof for
    /// "no queue interaction attempted" on a non-queue-park suspension.
    struct PanicIfTouchedQueue;

    #[async_trait]
    impl HeavyJobLookup for PanicIfTouchedQueue {
        async fn await_outcome(&self, _job_id: Uuid) -> serde_json::Value {
            panic!("await_outcome must never be called for a non-HeavyWorkQueue suspension");
        }

        async fn cancel_if_queued(&self, _job_id: Uuid) {
            panic!("cancel_if_queued must never be called for a non-HeavyWorkQueue suspension");
        }
    }

    // -- Tests -------------------------------------------------------------

    #[tokio::test]
    async fn drive_returns_non_queue_suspension_unchanged() {
        for reason in [SuspendReason::OperatorPause, SuspendReason::SuspendNode] {
            let workflow = operator_pause_workflow();
            let cancel = CancellationToken::new();

            // Build the ctx the DIRECT `run_with` call would produce, for
            // comparison, using the SAME reason this iteration targets --
            // `RequestOperatorPauseNode` always requests `OperatorPause`,
            // so drive `SuspendNode` through the plain `request_suspension`
            // path instead (its own default), matching the workflow shape.
            let direct = if reason == SuspendReason::SuspendNode {
                let mut registry = NodeRegistry::new();
                struct RequestPlainSuspendNode;
                #[async_trait::async_trait]
                impl Node for RequestPlainSuspendNode {
                    async fn process(
                        &self,
                        mut ctx: TaskContext,
                    ) -> Result<TaskContext, NodeError> {
                        suspend::request_suspension(&mut ctx.metadata);
                        Ok(ctx)
                    }
                    fn name(&self) -> &str {
                        "RequestPlainSuspendNode"
                    }
                }
                registry.register(Box::new(RequestPlainSuspendNode));
                registry.register(Box::new(SuccessNode));
                let mut nodes = HashMap::new();
                nodes.insert(
                    "RequestPlainSuspendNode".to_string(),
                    NodeConfig::new("RequestPlainSuspendNode", vec!["SuccessNode".to_string()]),
                );
                nodes.insert(
                    "SuccessNode".to_string(),
                    NodeConfig::new("SuccessNode", vec![]),
                );
                let schema = WorkflowSchema::new("linear", "RequestPlainSuspendNode", nodes);
                let plain_workflow = Workflow::new(registry, schema);
                let direct_ctx = plain_workflow
                    .run(serde_json::json!({}), noop_progress())
                    .await
                    .expect("direct run_with should return Ok");

                let queue = PanicIfTouchedQueue;
                let start = RunStart::Fresh {
                    event: serde_json::json!({}),
                    on_progress: noop_progress(),
                    options: RunOptions::default(),
                    heavy_work: Arc::new(queue),
                };
                let driven_ctx = drive(&plain_workflow, start, &cancel)
                    .await
                    .expect("drive should return Ok");

                (direct_ctx, driven_ctx)
            } else {
                let direct_ctx = workflow
                    .run(serde_json::json!({}), noop_progress())
                    .await
                    .expect("direct run_with should return Ok");

                let queue = PanicIfTouchedQueue;
                let start = RunStart::Fresh {
                    event: serde_json::json!({}),
                    on_progress: noop_progress(),
                    options: RunOptions::default(),
                    heavy_work: Arc::new(queue),
                };
                let driven_ctx = drive(&workflow, start, &cancel)
                    .await
                    .expect("drive should return Ok");

                (direct_ctx, driven_ctx)
            };

            let (direct_ctx, driven_ctx) = direct;

            let direct_suspension =
                suspend::read_suspension(&direct_ctx.metadata).expect("direct run must suspend");
            let driven_suspension =
                suspend::read_suspension(&driven_ctx.metadata).expect("drive must suspend too");

            assert_eq!(driven_suspension.suspended, direct_suspension.suspended);
            assert_eq!(driven_suspension.reason, direct_suspension.reason);
            assert_eq!(driven_suspension.reason, Some(reason));
            assert_eq!(driven_suspension.resume_at, direct_suspension.resume_at);
            assert_eq!(driven_ctx.node_runs.len(), direct_ctx.node_runs.len());
        }
    }

    #[tokio::test]
    async fn drive_resumes_after_a_queue_parked_job_completes() {
        let job_id = Uuid::new_v4();
        let workflow = heavy_work_queue_workflow(job_id);
        let cancel = CancellationToken::new();

        let outcome = serde_json::json!({
            "all_passed": true,
            "check_results": [],
            "failure_summary": "",
            "test_depth": "full",
            "check_source": "harness",
            "excluded_checks": [],
            "heavy_work": { "mode": "enabled", "job_id": job_id.to_string() },
        });
        let queue = StubQueue::new(job_id, outcome.clone());
        let queue_for_completer = Arc::clone(&queue);

        // Fire the job's completion only once `drive` has provably started
        // awaiting it: `await_outcome` bumps `await_calls` synchronously
        // before it ever `.await`s on `notified()`, so spin-waiting on that
        // counter first (rather than firing `complete()` unconditionally
        // before `drive` even runs) is what proves a real await rather than
        // a value that happened to already be there.
        let completer = tokio::spawn(async move {
            while queue_for_completer.await_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            queue_for_completer.complete();
        });

        let start = RunStart::Fresh {
            event: serde_json::json!({}),
            on_progress: noop_progress(),
            options: RunOptions::default(),
            heavy_work: queue.clone(),
        };
        let ctx = drive(&workflow, start, &cancel)
            .await
            .expect("drive should return Ok once the walk completes");

        completer.await.expect("completer task should not panic");

        assert!(queue.resolved.load(Ordering::SeqCst));
        assert_eq!(queue.await_calls.load(Ordering::SeqCst), 1);

        // The walk actually resumed at `SuccessNode` (the suspension's own
        // `resume_at`) and ran to completion -- not suspended any more.
        let suspension = suspend::read_suspension(&ctx.metadata);
        assert!(
            suspension.map(|s| s.suspended).unwrap_or(false) == false,
            "walk must no longer be suspended once resumed to completion"
        );
        assert!(
            ctx.nodes.contains_key("SuccessNode"),
            "SuccessNode must have run after resume"
        );

        // The job's outcome was injected under the SAME key `TestTaskNode`'s
        // own inline output uses.
        assert_eq!(ctx.nodes.get(TEST_TASK_NODE_IDENTITY), Some(&outcome));
    }

    #[tokio::test]
    async fn drive_cancellation_while_queued_never_lets_the_job_run() {
        let job_id = Uuid::new_v4();
        let workflow = heavy_work_queue_workflow(job_id);
        let cancel = CancellationToken::new();

        // Never completes on its own -- the only way this test's `drive`
        // call returns is via the cancellation branch.
        let queue = StubQueue::new(job_id, serde_json::json!({ "unused": true }));

        // Already cancelled before `drive` even starts awaiting: since the
        // stub's `await_outcome` future never resolves on its own, the
        // `tokio::select!` can only ever resolve via `cancel.cancelled()`,
        // deterministically.
        cancel.cancel();

        let start = RunStart::Fresh {
            event: serde_json::json!({}),
            on_progress: noop_progress(),
            options: RunOptions::default(),
            heavy_work: queue.clone(),
        };
        let ctx = drive(&workflow, start, &cancel)
            .await
            .expect("drive should return Ok on cancellation, not Err");

        assert!(
            !queue.resolved.load(Ordering::SeqCst),
            "the job's own outcome/work must never have been observed as resolved"
        );
        assert!(
            queue.cancelled_while_queued.load(Ordering::SeqCst),
            "cancel_if_queued must have fired for a job that was still queued"
        );
        assert!(
            !ctx.nodes.contains_key("SuccessNode"),
            "the walk must never have resumed past the park"
        );

        let cancellation = ctx
            .metadata
            .get("cancellation")
            .expect("drive must stamp the crate's existing cancellation marker");
        assert_eq!(cancellation["cancelled"], serde_json::json!(true));

        // Still carries the (untouched) suspension marker -- `drive` never
        // pretends the walk un-parked itself.
        let suspension = suspend::read_suspension(&ctx.metadata).expect("suspension marker kept");
        assert!(suspension.suspended);
        assert_eq!(suspension.reason, Some(SuspendReason::HeavyWorkQueue));
    }

    #[tokio::test]
    async fn drive_resumes_repeatedly_across_two_queue_park_segments() {
        // A schema where the FIRST node parks, resumes into a SECOND node
        // that also parks (a distinct job id), and only then reaches
        // `SuccessNode` -- proving `drive`'s loop, not just one iteration
        // of it, and that a fresh `RunOptions`/`on_progress` reborrow is
        // used correctly on the second segment too.
        struct TwiceParkingNode {
            identity: &'static str,
            job_id: Uuid,
        }

        #[async_trait::async_trait]
        impl Node for TwiceParkingNode {
            async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
                ctx.metadata[HEAVY_WORK_METADATA_KEY] = serde_json::json!({
                    "job_id": self.job_id.to_string(),
                    "class": "test",
                    "state": "queued",
                });
                suspend::request_suspension_with_reason(
                    &mut ctx.metadata,
                    SuspendReason::HeavyWorkQueue,
                );
                Ok(ctx)
            }

            fn name(&self) -> &str {
                self.identity
            }
        }

        let job_a = Uuid::new_v4();
        let job_b = Uuid::new_v4();

        let mut registry = NodeRegistry::new();
        registry.register(Box::new(TwiceParkingNode {
            identity: "ParkA",
            job_id: job_a,
        }));
        registry.register(Box::new(TwiceParkingNode {
            identity: "ParkB",
            job_id: job_b,
        }));
        registry.register(Box::new(SuccessNode));

        let mut nodes = HashMap::new();
        nodes.insert(
            "ParkA".to_string(),
            NodeConfig::new("ParkA", vec!["ParkB".to_string()]),
        );
        nodes.insert(
            "ParkB".to_string(),
            NodeConfig::new("ParkB", vec!["SuccessNode".to_string()]),
        );
        nodes.insert(
            "SuccessNode".to_string(),
            NodeConfig::new("SuccessNode", vec![]),
        );
        let schema = WorkflowSchema::new("linear", "ParkA", nodes);
        let workflow = Workflow::new(registry, schema);

        let progress_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recording_progress = {
            let progress_calls = Arc::clone(&progress_calls);
            move |ctx: &TaskContext| {
                let mut keys: Vec<String> = ctx.nodes.keys().cloned().collect();
                keys.sort();
                progress_calls.lock().unwrap().push(keys.join(","));
            }
        };

        struct TwoJobQueue {
            job_a: Uuid,
            job_b: Uuid,
            resolved: Mutex<Vec<Uuid>>,
        }

        #[async_trait]
        impl HeavyJobLookup for TwoJobQueue {
            async fn await_outcome(&self, job_id: Uuid) -> serde_json::Value {
                assert!(job_id == self.job_a || job_id == self.job_b);
                self.resolved.lock().unwrap().push(job_id);
                serde_json::json!({ "all_passed": true })
            }

            async fn cancel_if_queued(&self, _job_id: Uuid) {
                panic!("no cancellation expected in this test");
            }
        }

        let queue = Arc::new(TwoJobQueue {
            job_a,
            job_b,
            resolved: Mutex::new(Vec::new()),
        });
        let cancel = CancellationToken::new();

        let start = RunStart::Fresh {
            event: serde_json::json!({}),
            on_progress: Box::new(recording_progress),
            options: RunOptions::default(),
            heavy_work: queue.clone(),
        };
        let ctx = drive(&workflow, start, &cancel)
            .await
            .expect("drive should run both park segments to completion");

        assert_eq!(*queue.resolved.lock().unwrap(), vec![job_a, job_b]);
        assert!(ctx.nodes.contains_key("SuccessNode"));
        let suspension = suspend::read_suspension(&ctx.metadata);
        assert!(!suspension.map(|s| s.suspended).unwrap_or(false));

        // `on_progress` (the SAME underlying sink, reborrowed) was actually
        // invoked across BOTH segments -- not silently dropped after the
        // first `run_with` call the way a fresh no-op per segment would
        // hide.
        assert!(
            progress_calls.lock().unwrap().len() > 1,
            "on_progress must be invoked across every segment of the driven walk"
        );
    }

    #[tokio::test]
    async fn drive_resume_start_variant_continues_an_already_suspended_run() {
        // Exercises `RunStart::Resume`: a run resumed straight INTO the
        // queue-park node (as if it had been suspended upstream for some
        // other reason and this resume happens to land on
        // `RequestHeavyWorkQueueNode`) is driven through that park and
        // finishes -- proving `drive` handles `Resume` identically to
        // `Fresh` once the walk is underway, not merely as a special case
        // of a `Fresh` run it happens to have produced itself.
        let job_id = Uuid::new_v4();
        let workflow = heavy_work_queue_workflow(job_id);
        let cancel = CancellationToken::new();

        let seed_ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let resume_state = ResumeState {
            ctx: seed_ctx,
            at_identity: "RequestHeavyWorkQueueNode".to_string(),
            ledger: BudgetLedger::new(),
        };

        let outcome = serde_json::json!({ "all_passed": true, "resumed": true });
        let queue = StubQueue::new(job_id, outcome.clone());
        let queue_for_completer = Arc::clone(&queue);
        let completer = tokio::spawn(async move {
            while queue_for_completer.await_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            queue_for_completer.complete();
        });

        let start = RunStart::Resume {
            state: resume_state,
            on_progress: noop_progress(),
            options: RunOptions::default(),
            heavy_work: queue.clone(),
        };
        let ctx = drive(&workflow, start, &cancel)
            .await
            .expect("drive should resume and complete");
        completer.await.expect("completer must not panic");

        assert!(ctx.nodes.contains_key("SuccessNode"));
        assert_eq!(ctx.nodes.get(TEST_TASK_NODE_IDENTITY), Some(&outcome));
    }
}
