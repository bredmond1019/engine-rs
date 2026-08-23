//! The three suspend/resume routes (EN.6.F task 11): `POST
//! /events/{run_id}/pause`, `POST /events/{event_id}/resume`, and `GET
//! /events/suspended`.
//!
//! **Route ordering matters.** `configure` (`crate::http::configure`) MUST
//! register `/events/suspended` before `/events/{event_id}` — actix-web
//! resolves routes first-registration-wins, so a literal path registered
//! after a `{event_id}` segment would never be reached; the uuid extractor
//! swallows it first.
//!
//! All three handlers gate on `X-API-Key` via [`crate::http::check_api_key`],
//! matching every other run route.

use std::path::{Path, PathBuf};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_core::workflow::ResumeState;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::checkpoint::{
    read_checkpoint, CheckpointError, ReadCheckpoint,
};
use engine_core::workflows::orchestration::integrate::{LaneLogEntry, LaneLogStatus};
use engine_core::workflows::CommandRunner;
use engine_core::{BudgetLedger, CancellationToken, PauseSignal};
use uuid::Uuid;

use crate::dispatch::DispatchError;
use crate::http::{check_api_key, default_budget_from_env, AppState};
use crate::suspend::{self, RunStart, SpawnedRun, SuspendedEntry, TakeForResume};

// ── `EN.11.H` task 4: campaign-level crash recovery ─────────────────────
//
// Everything above this section is `EN.6.F`'s SINGLE-RUN suspend/resume: a
// `Workflow` that suspended itself at a named node and is rehydrated by
// `resume_run`. A crashed CAMPAIGN (`kill -9` mid-chain) is a different
// shape of problem — the `ORCHESTRATION` workflow drives its whole chain
// inside one blocking `Node::process` call
// (`graph::OrchestrationRunNode::process`, via `integrate::integrate_chain`),
// so there is no suspended `TaskContext` to rehydrate: the process simply
// stopped existing, mid-loop, with nothing durable recording that beyond
// whatever `integrate_chain` had already written to
// `roadmap_dir/lane-log.jsonl` and `checkpoint-<campaign_id>.json`
// (`EN.11.H` task 1/2).
//
// Resuming such a campaign is therefore: read its checkpoint, work out
// which steps of the ORIGINAL chain are not yet integrated, reconcile any
// branch/worktree the crashed attempt at the next step may have left
// behind, and hand the caller the REMAINING chain to run through
// `integrate_chain` again under the SAME `campaign_id` — never re-running
// an already-integrated step (which would duplicate its `lane-log.jsonl`
// line, since `integrate_chain` itself has no "skip what the checkpoint
// already covers" logic; that is this module's job, not
// `integrate_chain`'s — see `checkpoint.rs`'s and `integrate.rs`'s own
// docs on this division).
//
// This module deliberately adds no new `AppState` field and no new HTTP
// route: `EN.11.E`'s `campaigns` field addition to `AppState`
// (`15d45e0`) broke `bastion`'s build for two days, undetected, because
// `bastion` constructs `AppState` directly. `resume <campaign>` (the
// operator-facing CLI verb this block is named for) is a pure function of
// a checkpoint and a chain — it needs no server state at all, so the
// safest shape is exactly that: free functions callers (a future
// `bastion` verb, or a route added later with its own `AppState` review)
// can call without engine-serve's `AppState` shape ever moving under them.

/// What resuming campaign `campaign_id` against `chain` — the FULL,
/// originally-resolved chain for this campaign's roadmap/lane, in the
/// same order the crashed run was given — should do next. Produced by
/// [`plan_campaign_resume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignResumeOutcome {
    /// No checkpoint has ever been written for this campaign — it either
    /// never ran, or never crossed its first block boundary. There is
    /// nothing this module can act on; resuming is a no-op.
    NoCheckpoint,
    /// The checkpoint's integrated-step count already covers the whole
    /// chain. Resuming a complete campaign is a no-op, not a re-run.
    AlreadyComplete,
    /// The block the chain would resume at was stopped deliberately — an
    /// operator abort (`EN.11.F`) or a tripped campaign budget ceiling —
    /// not by a crash. See [`plan_campaign_resume`]'s doc for how this is
    /// told apart from a real crash. Resume refuses to restart it rather
    /// than silently re-running a campaign the operator stopped on
    /// purpose.
    Aborted { block_id: String },
    /// Restart at `resume_at_index` (1-based, matching
    /// [`engine_core::workflows::orchestration::checkpoint::CheckpointStep::index`])
    /// with `remaining` — `chain` sliced to just the steps not yet
    /// recorded as integrated, in their original order.
    Plan {
        resume_at_index: u32,
        remaining: Vec<ChainStep>,
    },
}

impl CampaignResumeOutcome {
    /// A clear, operator-facing message for every outcome — AC: "Resuming
    /// a campaign that never crashed, or one already complete, is a no-op
    /// with a clear message rather than a re-run," extended here to cover
    /// every variant so a caller never has to invent its own wording.
    #[must_use]
    pub fn message(&self, campaign_id: Uuid) -> String {
        match self {
            CampaignResumeOutcome::NoCheckpoint => format!(
                "campaign {campaign_id} has no checkpoint on disk — nothing to resume (it \
                 never ran, or never crossed a block boundary)"
            ),
            CampaignResumeOutcome::AlreadyComplete => format!(
                "campaign {campaign_id} already integrated every step in its chain — nothing \
                 to resume"
            ),
            CampaignResumeOutcome::Aborted { block_id } => format!(
                "campaign {campaign_id} was stopped at block {block_id} by an operator abort \
                 or a budget halt, not a crash — refusing to resume it"
            ),
            CampaignResumeOutcome::Plan {
                resume_at_index,
                remaining,
            } => format!(
                "campaign {campaign_id} resumes at step {resume_at_index} ({} block(s) \
                 remaining)",
                remaining.len()
            ),
        }
    }
}

/// Resolve what resuming `campaign_id` should do, given `chain`.
///
/// `done` is the count of [`CheckpointStep`](engine_core::workflows::orchestration::checkpoint::CheckpointStep)
/// entries the checkpoint has recorded as `integrated`. Because
/// `integrate_chain` (`EN.11.H` task 2) appends a checkpoint step in
/// chain order immediately as each step finishes, `done` is exactly the
/// 0-based index of the first step NOT yet integrated — the "N+1" the
/// block record's acceptance criteria name.
///
/// # Abort detection
///
/// `EN.11.F` abort and this crash-recovery path share one observable
/// symptom: a chain that stopped before its last step. They differ in
/// HOW it stopped, not IF: a deliberate stop (an operator abort, or a
/// tripped campaign budget) always appends one
/// [`LaneLogStatus::Cancelled`] or [`LaneLogStatus::BudgetHalted`] line
/// naming the block that never started, written by `integrate_chain`'s
/// own cancellation/budget branches before it returns. A `kill -9` crash
/// writes NEITHER — the process dies mid-step with no chance to append
/// anything for the block it never finished. So: if the block this
/// campaign would resume at (`chain[done]`) has one of those two statuses
/// as the LAST lane-log line naming it, this was a deliberate stop, not a
/// crash, and resume must refuse it.
///
/// `LaneLogEntry` carries no `campaign_id` (it is a per-lane record, not
/// a per-campaign one) — this check is a per-block heuristic like the
/// rest of the lane log, using exactly the vocabulary `integrate_chain`
/// itself writes rather than a second definition of "aborted".
pub fn plan_campaign_resume(
    roadmap_dir: &Path,
    campaign_id: Uuid,
    chain: &[ChainStep],
) -> Result<CampaignResumeOutcome, CheckpointError> {
    let checkpoint = match read_checkpoint(roadmap_dir, campaign_id)? {
        ReadCheckpoint::Absent => return Ok(CampaignResumeOutcome::NoCheckpoint),
        ReadCheckpoint::Found(checkpoint) => checkpoint,
    };

    let done = checkpoint.steps.iter().filter(|s| s.integrated).count();
    if done >= chain.len() {
        return Ok(CampaignResumeOutcome::AlreadyComplete);
    }

    let next = &chain[done];
    if last_status_for_block(roadmap_dir, &next.block_id).is_some_and(|status| {
        matches!(
            status,
            LaneLogStatus::Cancelled | LaneLogStatus::BudgetHalted
        )
    }) {
        return Ok(CampaignResumeOutcome::Aborted {
            block_id: next.block_id.clone(),
        });
    }

    Ok(CampaignResumeOutcome::Plan {
        resume_at_index: done as u32 + 1,
        remaining: chain[done..].to_vec(),
    })
}

/// The LAST recorded [`LaneLogStatus`] for `block_id` in
/// `roadmap_dir/lane-log.jsonl`, if any. A missing file or an unparsable
/// line is treated as "nothing recorded" (`None`) rather than an error —
/// matching [`read_checkpoint`]'s "missing means absent, never an error"
/// contract: resuming a campaign whose lane log has not been written yet
/// must not fail merely because there is nothing there.
fn last_status_for_block(roadmap_dir: &Path, block_id: &str) -> Option<LaneLogStatus> {
    let contents = std::fs::read_to_string(roadmap_dir.join("lane-log.jsonl")).ok()?;
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<LaneLogEntry>(line).ok())
        .rfind(|entry| entry.block == block_id)
        .map(|entry| entry.status)
}

/// Best-effort git reconciliation for the branch/worktree a crashed
/// attempt at `block_id` may have left behind — run BEFORE re-dispatching
/// that block. Mirrors `SetupWorktreeNode`'s own default naming
/// (`sdlc/<block_id>`, `trees/<branch>`) so the branch this clears is
/// exactly the one a fresh `SDLC_FLOW` run for `block_id` would try to
/// create — this is what makes the AC true: "starts at block N+1 and does
/// NOT fail on the branch the crashed run already created."
///
/// Never fails: every call is `let _ = ...`, exactly like
/// `SetupWorktreeNode`'s own best-effort cleanup (`EN.11.H` task 3). A
/// crashed attempt may have left a worktree, a branch, both, or neither
/// (it might never have reached `SetupWorktreeNode` at all) —
/// reconciliation exists to clear the way for a fresh attempt, never to
/// become a NEW reason resume fails. Deliberately does not attempt to
/// salvage the branch's partial work: `EN.11.H`'s `out_of_scope` names
/// "mid-block resume" as a non-goal — resume restarts a block from
/// scratch at a block boundary, matching `EN.11.F`'s abort semantics.
pub fn reconcile_stale_branch(repo_cwd: &Path, block_id: &str, runner: &CommandRunner) {
    let branch = format!("sdlc/{block_id}");
    let worktree_path = PathBuf::from("trees").join(&branch);
    let worktree_path_str = worktree_path.to_string_lossy().into_owned();
    let _ = runner(
        "git",
        &["worktree", "remove", "--force", &worktree_path_str],
        repo_cwd,
    );
    let _ = runner("git", &["branch", "-D", &branch], repo_cwd);
}

/// `POST /events/{run_id}/pause` — 401 without a valid `X-API-Key`; `404`
/// for a `run_id` that is neither live nor suspended; `409` if it is already
/// suspended; otherwise sets the run's [`PauseSignal`] and returns `202
/// {run_id, status: "pausing"}`. Idempotent: a repeat call against an
/// already-pausing (but not yet suspended) run also 202s.
pub async fn pause_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();

    if suspend::list_suspended()
        .iter()
        .any(|(id, _)| *id == run_id)
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "run_id": run_id,
            "status": "suspended",
            "error": "run is already suspended",
        }));
    }

    match suspend::get_pause_signal(run_id) {
        Some(signal) => {
            signal.pause();
            HttpResponse::Accepted().json(serde_json::json!({
                "run_id": run_id,
                "status": "pausing",
            }))
        }
        None => {
            HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or finished run" }))
        }
    }
}

/// `GET /events/suspended` — `200 [{run_id, workflow_type, created_at,
/// suspended_at, resume_at, reason}]`, newest first. `X-API-Key` gated like
/// every other run route.
pub async fn list_suspended(req: HttpRequest, state: web::Data<AppState>) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let body: Vec<serde_json::Value> = suspend::list_suspended()
        .into_iter()
        .map(|(run_id, entry)| {
            serde_json::json!({
                "run_id": run_id,
                "workflow_type": entry.workflow_type,
                "created_at": entry.created_at,
                "suspended_at": entry.suspended_at,
                "resume_at": entry.resume_at,
                "reason": entry.reason,
            })
        })
        .collect();

    HttpResponse::Ok().json(body)
}

/// Falls back to Postgres for a suspended run this process never held
/// in-memory (e.g. a restart) — only reachable when `state.durable` was
/// constructed with a pool. Returns `None` on any failure (no pool, no such
/// row, or the row is not currently suspended) so the caller can 404
/// uniformly.
async fn rehydrate_from_store(state: &AppState, run_id: Uuid) -> Option<SuspendedEntry> {
    let pool = state.durable.pool()?;
    let row = engine_store::get_event(pool, run_id).await.ok()?;

    if !engine_core::suspend::is_suspended(&row.task_context.metadata) {
        return None;
    }

    let suspension = engine_core::suspend::read_suspension(&row.task_context.metadata);
    let resume_at = suspension
        .as_ref()
        .and_then(|s| s.resume_at.clone())
        .unwrap_or_default();
    let reason = suspension
        .as_ref()
        .and_then(|s| s.reason)
        .and_then(|r| serde_json::to_value(r).ok())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();

    Some(SuspendedEntry {
        workflow_type: row.workflow_type,
        data: row.data,
        snapshot: row.task_context,
        created_at: row.created_at,
        suspended_at: row.updated_at,
        resume_at,
        reason,
        // Never inserted into the in-memory index, so `remove_suspended`/
        // `clear_resuming` against this run_id are harmless no-ops on every
        // exit path below -- this flag exists only so the struct shape
        // matches the in-memory case.
        resuming: true,
    })
}

/// `POST /events/{event_id}/resume` — rehydrates a suspended run's
/// `Workflow`/`TaskContext`/budget ledger and continues the walk from the
/// stored `resume_at` pointer. No request body: an operator `{"at": ..}`
/// override is a deliberate non-goal (see task 11's spec).
///
/// | condition | response |
/// |---|---|
/// | unknown, or not suspended | `404` |
/// | already resuming (concurrent) | `409` |
/// | factory policy resolution fails | `422` |
/// | `resume_at` not in the rebuilt workflow | `422` |
/// | ok | `202 {run_id, event_id, status: "resuming", resume_at}` |
///
/// Every failure path from step 4 onward calls [`suspend::clear_resuming`]
/// so a transient failure (a bad policy resolution, a stale `resume_at`)
/// leaves the run retryable rather than permanently bricked.
pub async fn resume_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();

    let entry = match suspend::take_for_resume(run_id) {
        TakeForResume::Ready(entry) => *entry,
        TakeForResume::AlreadyResuming => {
            return HttpResponse::Conflict()
                .json(serde_json::json!({ "error": "resume already in flight" }));
        }
        TakeForResume::NotFound => match rehydrate_from_store(&state, run_id).await {
            Some(entry) => entry,
            None => {
                return HttpResponse::NotFound()
                    .json(serde_json::json!({ "error": "unknown or non-resumable run" }));
            }
        },
    };

    // Step 4: rebuild the budget ledger from the marker's snapshot,
    // falling back to the lossy `from_context` reconstruction when the
    // marker carries no ledger (an older/foreign snapshot).
    let marker = engine_core::suspend::read_suspension(&entry.snapshot.metadata);
    let ledger = marker
        .as_ref()
        .and_then(|m| m.ledger)
        .map(|snapshot| BudgetLedger::from_parts(snapshot.total_tokens, snapshot.total_cost_usd))
        .unwrap_or_else(|| BudgetLedger::from_context(&entry.snapshot));

    // Step 5: rebuild the Workflow from the ORIGINAL trigger payload, then
    // drop any seeded_nodes -- the rehydrated ctx already carries the
    // original run's resolved policy, and a factory rebuilt for the resume
    // must never re-seed/overwrite it.
    let workflow = match state
        .dispatcher
        .dispatch_with_event(&entry.workflow_type, &entry.data)
    {
        Ok(workflow) => workflow.without_seeded_nodes(),
        Err(DispatchError::UnknownWorkflowType(workflow_type)) => {
            suspend::clear_resuming(run_id);
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "policy resolution failed",
                "message": format!("unknown workflow_type '{workflow_type}'"),
            }));
        }
        Err(DispatchError::PolicyResolutionFailed(message)) => {
            suspend::clear_resuming(run_id);
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "policy resolution failed",
                "message": message,
            }));
        }
    };

    // Step 6: the resume point must still exist in the rebuilt graph.
    if !workflow.has_node(&entry.resume_at) {
        suspend::clear_resuming(run_id);
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "error": "resume point no longer exists in the workflow graph",
            "resume_at": entry.resume_at,
        }));
    }

    // Step 7: re-register fresh live-run bookkeeping -- a fresh
    // CancellationToken, a fresh PauseSignal, and the ORIGINAL created_at
    // (not `Utc::now()`) back into `live_run_metadata()`.
    let token = CancellationToken::new();
    state.runs.register(run_id, token.clone());

    let pause = PauseSignal::new();
    suspend::register_pause_signal(run_id, pause.clone());

    crate::http::live_run_metadata()
        .write()
        .expect("live run metadata lock poisoned on write")
        .insert(run_id, (entry.workflow_type.clone(), entry.created_at));

    // Step 8: invalidate the cached terminal ("suspended") SSE frame so a
    // subscriber attached after this resume gets live frames instead of the
    // stale suspended one.
    crate::stream::clear_terminal(run_id);

    // Step 9: the run is no longer suspended -- drop it from the index
    // (a no-op if it was never inserted, i.e. the Postgres-fallback path),
    // then spawn the resumed walk.
    suspend::remove_suspended(run_id);

    let resume_at = entry.resume_at.clone();
    let budget = default_budget_from_env();

    suspend::spawn_run(SpawnedRun {
        run_id,
        workflow,
        workflow_type: entry.workflow_type,
        data: entry.data,
        created_at: entry.created_at,
        start: RunStart::Resume(ResumeState {
            ctx: entry.snapshot,
            at_identity: resume_at.clone(),
            ledger,
        }),
        live: state.live.clone(),
        durable: state.durable.clone(),
        runs: state.runs.clone(),
        campaigns: state.campaigns.clone(),
        token,
        pause,
        budget,
    });

    HttpResponse::Accepted().json(serde_json::json!({
        "run_id": run_id,
        "event_id": run_id,
        "status": "resuming",
        "resume_at": resume_at,
    }))
}

#[cfg(test)]
// These tests hold `registry_test_lock()`'s std `MutexGuard` across `.await` points by
// design — it exists solely to serialize tests that share the global suspend registry, not
// to guard data an async task contends over concurrently, so the guard's lifetime spanning
// the whole test body is intentional rather than a correctness hazard.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use chrono::Utc;
    use engine_contract::TaskContext;
    use engine_core::dispatch::Dispatcher;
    use engine_core::{Node, NodeError, NodeRegistry, Workflow, WorkflowSchema};
    use std::collections::HashMap as StdHashMap;
    use std::sync::Arc;

    use crate::abort::RunRegistry;
    use crate::live_state::LiveStateStore;

    struct MarkerNode;

    #[async_trait::async_trait]
    impl Node for MarkerNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "MarkerNode"
        }
    }

    struct OnlyNode;

    #[async_trait::async_trait]
    impl Node for OnlyNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), serde_json::json!({ "ran": true }));
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "OnlyNode"
        }
    }

    /// `SuspendNode -> MarkerNode`, mirroring `http.rs`'s own suspend
    /// fixture: `SuspendNode`'s successor is the resume pointer.
    fn suspend_fixture_schema(workflow_type: &str) -> WorkflowSchema {
        let mut nodes = StdHashMap::new();
        nodes.insert(
            "SuspendNode".to_string(),
            engine_core::NodeConfig::new("SuspendNode", vec!["MarkerNode".to_string()]),
        );
        nodes.insert(
            "MarkerNode".to_string(),
            engine_core::NodeConfig::new("MarkerNode", vec![]),
        );
        WorkflowSchema::new(workflow_type, "SuspendNode", nodes)
    }

    fn test_app_state_with_suspend_fixture() -> AppState {
        const WORKFLOW_TYPE: &str = "suspend-fixture";
        let mut dispatcher = Dispatcher::new();
        dispatcher.register(
            suspend_fixture_schema(WORKFLOW_TYPE),
            Box::new(|_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(
                    engine_core::nodes::SuspendNode::new("SuspendNode").with_enabled(true),
                ));
                registry.register(Box::new(MarkerNode));
                Ok(Workflow::new(
                    registry,
                    suspend_fixture_schema(WORKFLOW_TYPE),
                ))
            }),
        );

        AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            campaigns: crate::abort::CampaignRegistry::new(),
            api_key: "test-key".to_string(),
        }
    }

    /// Triggers the suspend-fixture workflow against `$app` and blocks until
    /// its run lands in the suspended index, yielding the `run_id`. A macro
    /// (rather than a generic async fn) so it works against whatever
    /// unnameable `impl Service<..>` type `test::init_service` produces for
    /// each test's own `App`.
    macro_rules! suspend_a_run {
        ($app:expr) => {{
            let req = test::TestRequest::post()
                .uri("/events/")
                .insert_header(("X-API-Key", "test-key"))
                .set_json(serde_json::json!({ "workflow_type": "suspend-fixture", "data": {} }))
                .to_request();
            let resp = test::call_service(&$app, req).await;
            assert_eq!(resp.status(), 202);
            let body: serde_json::Value = test::read_body_json(resp).await;
            let run_id = body["run_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .expect("run_id should be a parseable UUID");

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if suspend::list_suspended()
                    .into_iter()
                    .any(|(id, _)| id == run_id)
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "run never landed in the suspended index"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            run_id
        }};
    }

    // -- pause -------------------------------------------------------------

    #[actix_web::test]
    async fn pause_without_api_key_is_unauthorized() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/pause", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn pause_unknown_run_is_404() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/pause", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn pause_a_live_run_is_202_and_idempotent() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let run_id = Uuid::new_v4();
        let sig = PauseSignal::new();
        suspend::register_pause_signal(run_id, sig.clone());

        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        for _ in 0..2 {
            let req = test::TestRequest::post()
                .uri(&format!("/events/{run_id}/pause"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let resp = test::call_service(&app, req).await;
            assert_eq!(resp.status(), 202);
        }
        assert!(sig.is_paused());

        suspend::remove_pause_signal(run_id);
    }

    #[actix_web::test]
    async fn pause_an_already_suspended_run_is_409() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/pause"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 409);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "suspended");

        suspend::remove_suspended(run_id);
    }

    // -- suspended list ------------------------------------------------

    #[actix_web::test]
    async fn suspended_route_resolves_as_a_literal_not_an_event_id() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(
            resp.status(),
            200,
            "the literal /events/suspended path must not be swallowed by the {{event_id}} extractor"
        );
        let body: Vec<serde_json::Value> = test::read_body_json(resp).await;
        assert!(body.is_empty());
    }

    #[actix_web::test]
    async fn suspended_list_contains_a_suspended_run_and_omits_it_after_resume() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: Vec<serde_json::Value> = test::read_body_json(resp).await;
        assert!(body
            .iter()
            .any(|entry| entry["run_id"] == run_id.to_string()));

        let resume_req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resume_resp = test::call_service(&app, resume_req).await;
        assert_eq!(resume_resp.status(), 202);

        let after_req = test::TestRequest::get()
            .uri("/events/suspended")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let after_resp = test::call_service(&app, after_req).await;
        let after_body: Vec<serde_json::Value> = test::read_body_json(after_resp).await;
        assert!(!after_body
            .iter()
            .any(|entry| entry["run_id"] == run_id.to_string()));
    }

    // -- resume --------------------------------------------------------

    #[actix_web::test]
    async fn resume_unknown_run_is_404() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/resume", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    #[actix_web::test]
    async fn resume_without_api_key_is_unauthorized() {
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{}/resume", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn resume_succeeds_with_no_database_url_and_completes_the_run() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "resuming");
        assert_eq!(body["resume_at"], "MarkerNode");

        // Poll the readback until the resumed run reaches its terminal
        // state -- MarkerNode is the only remaining node.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let get_req = test::TestRequest::get()
                .uri(&format!("/events/{run_id}"))
                .insert_header(("X-API-Key", "test-key"))
                .to_request();
            let get_resp = test::call_service(&app, get_req).await;
            let get_body: serde_json::Value = test::read_body_json(get_resp).await;
            if get_body["status"] == "succeeded" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "resumed run never reached succeeded"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[actix_web::test]
    async fn a_second_concurrent_resume_is_409_and_the_first_still_succeeds() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = test_app_state_with_suspend_fixture();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let run_id = suspend_a_run!(app);

        // Take the resuming flag directly, simulating a first caller already
        // in flight, then assert the HTTP layer reports 409 for a second.
        match suspend::take_for_resume(run_id) {
            TakeForResume::Ready(_) => {}
            _ => panic!("expected Ready"),
        }

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 409);

        // The first caller's in-flight resume is still retryable/valid --
        // clearing it and resuming for real now succeeds.
        suspend::clear_resuming(run_id);
        let retry_req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let retry_resp = test::call_service(&app, retry_req).await;
        assert_eq!(retry_resp.status(), 202);
    }

    #[actix_web::test]
    async fn resume_with_unresolvable_resume_point_is_422_and_clears_resuming() {
        let _guard = crate::suspend::registry_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A workflow whose factory rebuilds a *different* graph (no
        // "MarkerNode") than the one the run suspended in -- simulating
        // schema drift between suspend and resume.
        const WORKFLOW_TYPE: &str = "drifted-fixture";
        let mut dispatcher = Dispatcher::new();
        let drifted_schema = || {
            let mut nodes = StdHashMap::new();
            nodes.insert(
                "OnlyNode".to_string(),
                engine_core::NodeConfig::new("OnlyNode", vec![]),
            );
            WorkflowSchema::new(WORKFLOW_TYPE, "OnlyNode", nodes)
        };
        dispatcher.register(
            drifted_schema(),
            Box::new(move |_event: &serde_json::Value| {
                let mut registry = NodeRegistry::new();
                registry.register(Box::new(OnlyNode));
                Ok(Workflow::new(registry, drifted_schema()))
            }),
        );

        let state = AppState {
            dispatcher: Arc::new(dispatcher),
            live: LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: RunRegistry::new(),
            campaigns: crate::abort::CampaignRegistry::new(),
            api_key: "test-key".to_string(),
        };

        let run_id = Uuid::new_v4();
        let mut metadata = serde_json::json!({});
        engine_core::suspend::stamp_suspended(
            &mut metadata,
            engine_core::suspend::Suspension {
                resume_at: "MarkerNode",
                reason: engine_core::suspend::SuspendReason::OperatorPause,
                origin_identity: Some("OnlyNode"),
                ledger: &BudgetLedger::new(),
            },
        );
        let snapshot = TaskContext {
            event: serde_json::Value::Null,
            nodes: StdHashMap::new(),
            metadata,
            node_runs: StdHashMap::new(),
        };
        suspend::insert_suspended(
            run_id,
            SuspendedEntry {
                workflow_type: WORKFLOW_TYPE.to_string(),
                data: serde_json::json!({}),
                snapshot,
                created_at: Utc::now(),
                suspended_at: Utc::now(),
                resume_at: "MarkerNode".to_string(),
                reason: "operator_pause".to_string(),
                resuming: false,
            },
        );

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(crate::http::configure),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!("/events/{run_id}/resume"))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["resume_at"], "MarkerNode");

        // Retryable: `resuming` was cleared, so a follow-up resume against
        // the (still-broken) run is not falsely rejected as already-in-flight.
        match suspend::take_for_resume(run_id) {
            TakeForResume::Ready(_) => {}
            _ => panic!("expected the failed resume to have cleared `resuming`, got a state that is not Ready"),
        }

        suspend::remove_suspended(run_id);
    }
}

// ── `EN.11.H` task 4: campaign-level crash recovery — tests ──────────────

#[cfg(test)]
mod campaign_resume_tests {
    use super::*;
    use engine_core::repo_registry::RepoRegistry;
    use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
    use engine_core::workflows::orchestration::gates::AdmissionGate;
    use engine_core::workflows::orchestration::integrate::{integrate_chain, NeverHeld};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
            directives: None,
            ..Default::default()
        }
    }

    fn one_repo_registry() -> (tempfile::TempDir, RepoRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("repo-a")).unwrap();
        std::fs::write(
            dir.path().join("brain.toml"),
            "[[repos]]\nslug = \"repo-a\"\nrepo_path = \"repo-a\"\n",
        )
        .unwrap();
        let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
        (dir, registry)
    }

    fn recording_flow_runner() -> (FlowRunner, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: FlowRunner = Arc::new(move |invocation| {
            recorded.lock().unwrap().push(invocation.block_id.clone());
            Box::pin(async {
                Ok(engine_contract::TaskContext {
                    event: json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        (runner, calls)
    }

    fn write_done_state(repo_path: &std::path::Path, block_id: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            json!({"status": "done"}).to_string(),
        )
        .unwrap();
    }

    // ── `plan_campaign_resume` ─────────────────────────────────────────

    #[test]
    fn no_checkpoint_on_disk_is_a_no_op() {
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();
        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        let outcome = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");

        assert_eq!(outcome, CampaignResumeOutcome::NoCheckpoint);
        assert!(outcome.message(campaign_id).contains("nothing to resume"));
    }

    #[test]
    fn a_checkpoint_covering_the_whole_chain_is_already_complete() {
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();
        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        let checkpoint = engine_core::workflows::orchestration::checkpoint::Checkpoint {
            campaign_id,
            steps: vec![
                engine_core::workflows::orchestration::checkpoint::CheckpointStep {
                    repo: "repo-a".into(),
                    block_id: "A.1".into(),
                    index: 1,
                    integrated: true,
                    branch: Some("sdlc/A.1".into()),
                },
                engine_core::workflows::orchestration::checkpoint::CheckpointStep {
                    repo: "repo-a".into(),
                    block_id: "A.2".into(),
                    index: 2,
                    integrated: true,
                    branch: Some("sdlc/A.2".into()),
                },
            ],
        };
        engine_core::workflows::orchestration::checkpoint::write_checkpoint(
            roadmap_dir.path(),
            &checkpoint,
        )
        .unwrap();

        let outcome = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");

        assert_eq!(outcome, CampaignResumeOutcome::AlreadyComplete);
        assert!(outcome.message(campaign_id).contains("nothing to resume"));
    }

    #[test]
    fn a_partial_checkpoint_resumes_at_the_first_step_not_yet_integrated() {
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();
        let chain = vec![
            step("repo-a", "A.1"),
            step("repo-a", "A.2"),
            step("repo-a", "A.3"),
        ];

        let checkpoint = engine_core::workflows::orchestration::checkpoint::Checkpoint {
            campaign_id,
            steps: vec![
                engine_core::workflows::orchestration::checkpoint::CheckpointStep {
                    repo: "repo-a".into(),
                    block_id: "A.1".into(),
                    index: 1,
                    integrated: true,
                    branch: Some("sdlc/A.1".into()),
                },
            ],
        };
        engine_core::workflows::orchestration::checkpoint::write_checkpoint(
            roadmap_dir.path(),
            &checkpoint,
        )
        .unwrap();

        let outcome = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");

        match outcome {
            CampaignResumeOutcome::Plan {
                resume_at_index,
                remaining,
            } => {
                assert_eq!(resume_at_index, 2);
                assert_eq!(
                    remaining,
                    vec![step("repo-a", "A.2"), step("repo-a", "A.3")]
                );
            }
            other => panic!("expected Plan, got {other:?}"),
        }
    }

    #[test]
    fn a_deliberate_abort_line_for_the_next_block_refuses_to_resume() {
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();
        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        let checkpoint = engine_core::workflows::orchestration::checkpoint::Checkpoint {
            campaign_id,
            steps: vec![
                engine_core::workflows::orchestration::checkpoint::CheckpointStep {
                    repo: "repo-a".into(),
                    block_id: "A.1".into(),
                    index: 1,
                    integrated: true,
                    branch: Some("sdlc/A.1".into()),
                },
            ],
        };
        engine_core::workflows::orchestration::checkpoint::write_checkpoint(
            roadmap_dir.path(),
            &checkpoint,
        )
        .unwrap();

        // Simulate `EN.11.F`'s abort: `integrate_chain`'s cancellation
        // branch appends exactly this shape of line, naming the block
        // that never started.
        let cancelled_line = json!({
            "ts": "2026-08-23T00:00:00+00:00",
            "lane": "repo-a",
            "repo": "repo-a",
            "block": "A.2",
            "status": "cancelled",
            "note": "campaign cancelled at the block boundary before A.2 started",
        });
        std::fs::write(
            roadmap_dir.path().join("lane-log.jsonl"),
            format!("{}\n", cancelled_line),
        )
        .unwrap();

        let outcome = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");

        assert_eq!(
            outcome,
            CampaignResumeOutcome::Aborted {
                block_id: "A.2".to_string()
            }
        );
        assert!(outcome.message(campaign_id).contains("not a crash"));
    }

    // ── `reconcile_stale_branch` ────────────────────────────────────────

    #[test]
    fn reconcile_stale_branch_removes_the_worktree_then_deletes_the_branch() {
        let calls: Arc<Mutex<Vec<(String, Vec<String>)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let runner: CommandRunner = Arc::new(move |program, args, _cwd| {
            recorded.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            Ok(engine_core::workflows::CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        });

        reconcile_stale_branch(std::path::Path::new("."), "A.2", &runner);

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "git");
        assert_eq!(
            recorded[0].1,
            vec!["worktree", "remove", "--force", "trees/sdlc/A.2"]
        );
        assert_eq!(recorded[1].0, "git");
        assert_eq!(recorded[1].1, vec!["branch", "-D", "sdlc/A.2"]);
    }

    #[test]
    fn reconcile_stale_branch_is_non_fatal_when_git_errors() {
        let runner: CommandRunner =
            Arc::new(|_program, _args, _cwd| Err(std::io::Error::other("no such branch")));

        // Must not panic — every call inside is best-effort.
        reconcile_stale_branch(std::path::Path::new("."), "A.2", &runner);
    }

    // ── Crash-then-resume round trip through `integrate_chain` ─────────

    #[tokio::test]
    async fn a_campaign_killed_after_step_one_resumes_at_step_two_without_duplicating_the_lane_log_line(
    ) {
        let (dir, registry) = one_repo_registry();
        let repo_path = dir.path().join("repo-a");
        write_done_state(&repo_path, "A.1");
        write_done_state(&repo_path, "A.2");

        let (runner, calls) = recording_flow_runner();
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let hold = NeverHeld;
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();

        let chain = vec![step("repo-a", "A.1"), step("repo-a", "A.2")];

        // The crashed run: only step 1 ever gets a chance to integrate —
        // `kill -9` between the two blocks means `integrate_chain` was
        // simply never called again with step 2, not that it failed.
        let first_attempt = integrate_chain(
            &chain[..1],
            &resolve_deps,
            &is_met,
            &admission,
            &hold,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_| {},
            false,
            campaign_id,
        )
        .await
        .expect("step one should integrate cleanly");
        assert_eq!(first_attempt.len(), 1);

        let lines_after_crash = std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl"))
            .unwrap()
            .lines()
            .count();
        assert_eq!(lines_after_crash, 1);

        // Resume: the plan must skip step 1 and hand back only step 2.
        let plan = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");
        let remaining = match plan {
            CampaignResumeOutcome::Plan {
                resume_at_index,
                remaining,
            } => {
                assert_eq!(resume_at_index, 2);
                remaining
            }
            other => panic!("expected Plan, got {other:?}"),
        };
        assert_eq!(remaining, vec![step("repo-a", "A.2")]);

        // Running the REMAINING chain (never the full chain again) is
        // what keeps the lane log append-only: step 1 is simply never
        // asked for again.
        let second_attempt = integrate_chain(
            &remaining,
            &resolve_deps,
            &is_met,
            &admission,
            &hold,
            Duration::from_millis(1),
            None,
            None,
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_| {},
            false,
            campaign_id,
        )
        .await
        .expect("step two should integrate cleanly on resume");
        assert_eq!(second_attempt.len(), 1);
        assert_eq!(second_attempt[0].block_id, "A.2");

        // Exactly one dispatch per block — A.1 was never re-run.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.iter().filter(|b| *b == "A.1").count(), 1);
        assert_eq!(recorded.iter().filter(|b| *b == "A.2").count(), 1);

        // No duplicate lane-log line for A.1: exactly 2 lines total, one
        // per block, in order.
        let final_lines: Vec<String> =
            std::fs::read_to_string(roadmap_dir.path().join("lane-log.jsonl"))
                .unwrap()
                .lines()
                .map(str::to_string)
                .collect();
        assert_eq!(final_lines.len(), 2);
        let parsed: Vec<LaneLogEntry> = final_lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(parsed[0].block, "A.1");
        assert_eq!(parsed[1].block, "A.2");

        // The checkpoint now records both steps integrated, in order.
        let checkpoint = read_checkpoint(roadmap_dir.path(), campaign_id)
            .unwrap()
            .into_option()
            .expect("checkpoint should exist");
        assert_eq!(checkpoint.steps.len(), 2);
        assert!(checkpoint.steps.iter().all(|s| s.integrated));

        // Resuming again now reports the campaign as already complete —
        // never a re-run.
        let final_plan = plan_campaign_resume(roadmap_dir.path(), campaign_id, &chain)
            .expect("plan must not error");
        assert_eq!(final_plan, CampaignResumeOutcome::AlreadyComplete);
    }
}
