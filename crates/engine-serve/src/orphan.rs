//! The boot sweep: reconcile crash-stranded runs and fail them loudly
//! (`EN.9.C` task 5).
//!
//! Per `planning/EN.9.C/tasks.md`: launchd's `KeepAlive=true` /
//! `ThrottleInterval=10` restarts a crashed `bastion serve` instance within
//! ~10s and hides the evidence by coming back up healthy. A run stranded
//! mid-walk carries no `metadata.completion` marker (`engine_core::completion`)
//! and, absent a failure marker, `crate::http::derive_terminal_status` would
//! read it as `"succeeded"` — indistinguishable from a clean finish. The
//! sweep closes that gap: on boot, find every non-terminal-marked row past a
//! policy-resolved age, stamp it `metadata.failure` + `metadata.completion`
//! with `status: "failed"`, and persist — **never resume** a mid-walk crash
//! (`crate::resume` returns `None` for any run without a suspension marker;
//! only `finish_suspended` writes one, so an orphan by definition has none).
//!
//! **`OrphanLister` seam.** Mirrors `engine_core::nodes::http_post::HttpPost`
//! and `engine_core::nodes::doc_materializer::DocMaterializer`'s shape: a
//! trait so the reconcile logic is testable with NO database
//! ([`RecordingOrphanLister`]), while production wires the real
//! `engine-store`-backed [`PgOrphanLister`] via [`orphan_lister_live`].
//!
//! **Boot wiring lands in bastion, not here** — see the spec Notes. This
//! module exposes [`reconcile_orphans`] as a callable entry point; actually
//! calling it at server boot is a `bastion` change, exactly like
//! `spawn_schedule_loop`'s `schedule-loop-spawnable-but-unspawned` carryover.
//!
//! **Never silent.** A sweep that reconciles nothing observably would
//! reproduce the exact "comes back up healthy" failure mode this block
//! exists to end, so [`reconcile_orphans`] prints one line per reconciled
//! run plus a summary line, and returns a [`ReconcileSummary`] so a caller
//! can log or assert on it too.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use engine_contract::{EventsRow, TaskContext};
use engine_core::operator::orphan::OrphanPolicy;
use engine_core::operator::queue::{ItemSource, OperatorQueueItem};
use engine_core::operator::{
    validate, OperatorPayload, OperatorPayloadLimits, OperatorResponseOption,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::live_state::{LiveStateStore, RunId};

/// The injectable seam this sweep lists orphan candidates through:
/// `list_orphan_candidates(older_than, limit)` -> the rows whose
/// `task_context.metadata.completion` is absent and whose `updated_at` is
/// older than `older_than`, ordered oldest-first; and
/// `persist_reconciled(row)` -> writes the stamped row back durably. Async
/// trait so both the live `engine-store`-backed implementation and test
/// stubs can be swapped in behind a single `Arc<dyn OrphanLister>`.
#[async_trait]
pub trait OrphanLister: Send + Sync {
    async fn list_orphan_candidates(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<EventsRow>, String>;

    async fn persist_reconciled(&self, row: &EventsRow) -> Result<(), String>;
}

/// The live `engine-store`-backed `OrphanLister`: `list_orphan_candidates`
/// delegates to `engine_store::list_orphan_candidates`; `persist_reconciled`
/// delegates to `engine_store::upsert_event` (the same durable writer the
/// rest of the crate uses, so a reconciled row is shape-identical to a
/// normally-terminated one).
pub struct PgOrphanLister {
    pool: PgPool,
}

impl PgOrphanLister {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OrphanLister for PgOrphanLister {
    async fn list_orphan_candidates(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<EventsRow>, String> {
        engine_store::list_orphan_candidates(&self.pool, older_than, limit)
            .await
            .map_err(|err| format!("orphan candidate query failed: {err}"))
    }

    async fn persist_reconciled(&self, row: &EventsRow) -> Result<(), String> {
        engine_store::upsert_event(&self.pool, row)
            .await
            .map_err(|err| format!("orphan reconcile persist failed: {err}"))
    }
}

/// Convenience constructor: an `Arc<dyn OrphanLister>` wrapping
/// [`PgOrphanLister`]. Production callers (the eventual `bastion` boot
/// wiring) reach for this; tests build a [`RecordingOrphanLister`] instead,
/// so the gated suite never touches a live database.
#[must_use]
pub fn orphan_lister_live(pool: PgPool) -> Arc<dyn OrphanLister> {
    Arc::new(PgOrphanLister::new(pool))
}

/// Test-stub `OrphanLister` backed by an in-memory `Vec<EventsRow>`, so a
/// hermetic test can exercise the full list -> reconcile -> persist ->
/// list-again loop without a database. `list_orphan_candidates` applies the
/// same predicate the live SQL query does (`completion` absent, `updated_at`
/// older than the cutoff, oldest-first, capped at `limit`); `persist_reconciled`
/// upserts by id into the same backing vec, which is what makes a second
/// sweep over the same stub see the first sweep's stamps.
#[derive(Clone)]
pub struct RecordingOrphanLister {
    rows: Arc<Mutex<Vec<EventsRow>>>,
    list_error: Arc<Mutex<Option<String>>>,
}

impl RecordingOrphanLister {
    /// A stub seeded with `rows` and no configured list error.
    #[must_use]
    pub fn new(rows: Vec<EventsRow>) -> Self {
        Self {
            rows: Arc::new(Mutex::new(rows)),
            list_error: Arc::new(Mutex::new(None)),
        }
    }

    /// A stub whose `list_orphan_candidates` always fails with `error` — for
    /// asserting `reconcile_orphans` surfaces a lister error rather than
    /// swallowing it.
    #[must_use]
    pub fn failing_list(error: impl Into<String>) -> Self {
        Self {
            rows: Arc::new(Mutex::new(Vec::new())),
            list_error: Arc::new(Mutex::new(Some(error.into()))),
        }
    }

    /// The stub's current backing rows, post-reconcile — lets a test assert
    /// on the stamped `metadata.failure`/`metadata.completion` shape.
    #[must_use]
    pub fn rows(&self) -> Vec<EventsRow> {
        self.rows.lock().expect("stub lock poisoned").clone()
    }
}

#[async_trait]
impl OrphanLister for RecordingOrphanLister {
    async fn list_orphan_candidates(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<EventsRow>, String> {
        if let Some(err) = self.list_error.lock().expect("stub lock poisoned").clone() {
            return Err(err);
        }

        let rows = self.rows.lock().expect("stub lock poisoned");
        let mut candidates: Vec<EventsRow> = rows
            .iter()
            .filter(|row| {
                !engine_core::is_complete(&row.task_context.metadata) && row.updated_at < older_than
            })
            .cloned()
            .collect();
        candidates.sort_by_key(|row| row.updated_at);
        candidates.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        Ok(candidates)
    }

    async fn persist_reconciled(&self, row: &EventsRow) -> Result<(), String> {
        let mut rows = self.rows.lock().expect("stub lock poisoned");
        match rows.iter_mut().find(|existing| existing.id == row.id) {
            Some(existing) => *existing = row.clone(),
            None => rows.push(row.clone()),
        }
        Ok(())
    }
}

/// What one [`reconcile_orphans`] sweep did: the number of candidates
/// scanned and the ids it reconciled, in the order they were processed
/// (oldest `updated_at` first, per the lister's ordering contract).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub scanned: usize,
    pub reconciled: Vec<Uuid>,
}

/// Find the node a run was mid-execution on, for naming in the failure
/// reason: the `node_runs` entry (if any) whose status is `Running` at the
/// moment the process died. Iterates in a deterministic (sorted-by-name)
/// order so the choice is stable when — in principle — more than one entry
/// is `Running` (should not happen in a correctly-instrumented walk, but
/// this must never panic or be nondeterministic if it does).
fn stuck_node_name(ctx: &engine_contract::TaskContext) -> Option<String> {
    let mut running: Vec<&String> = ctx
        .node_runs
        .iter()
        .filter(|(_, run)| run.status == engine_contract::NodeRunStatus::Running)
        .map(|(name, _)| name)
        .collect();
    running.sort();
    running.into_iter().next().cloned()
}

/// The boot sweep: for every orphan candidate the resolved `policy` allows
/// (`reconcile_on_boot`, `orphan_scan_limit`), stamp `metadata.failure` with
/// a reason naming the crash and the node the run died on (if recoverable),
/// stamp `metadata.completion` with `status: "failed"`, and persist via
/// `lister.persist_reconciled`. Never attempts a resume — a mid-walk crash
/// is unresumable by design (`crate::resume`).
///
/// No-ops (returns an empty, `scanned: 0` summary) when `policy.reconcile_on_boot`
/// is `false`. Surfaces a lister error rather than swallowing it — a failed
/// sweep must be visible, not silently skipped.
///
/// Idempotent: a candidate this call reconciles now carries a `completion`
/// marker, so a second sweep's `list_orphan_candidates` call — live or
/// stubbed — no longer returns it.
///
/// **Seeds `live`'s completed-run ring for every reconciled row** (via
/// [`LiveStateStore::mark_terminal`]) before returning. Without this, a run
/// this sweep reconciles in Postgres is correctly terminal in the database
/// but was never in *this* process's `LiveStateStore` (it crashed under the
/// previous process), so `GET /events/{event_id}` — which serves reads only
/// from `LiveStateStore`, by design, since CI has no `DATABASE_URL` — would
/// 404 it forever: idempotency means a later sweep never re-lists an
/// already-reconciled row, so there is no second chance to seed it. Routing
/// through `mark_terminal` also means a boot-reconciled failure fires the
/// same terminal-run notification hook a live failure would.
pub async fn reconcile_orphans(
    lister: &dyn OrphanLister,
    live: &LiveStateStore,
    policy: &OrphanPolicy,
    now: DateTime<Utc>,
) -> Result<ReconcileSummary, String> {
    if !policy.reconcile_on_boot {
        println!("orphan sweep: reconcile_on_boot is disabled, skipping");
        return Ok(ReconcileSummary::default());
    }

    let candidates = lister
        .list_orphan_candidates(now, policy.orphan_scan_limit)
        .await
        .map_err(|err| format!("orphan sweep: failed to list candidates: {err}"))?;

    let scanned = candidates.len();
    let mut reconciled = Vec::with_capacity(scanned);

    for mut row in candidates {
        let reason = match stuck_node_name(&row.task_context) {
            Some(node) => format!(
                "run {} was found non-terminal with no completion marker at boot; \
                 presumed crashed on node {node}",
                row.id
            ),
            None => format!(
                "run {} was found non-terminal with no completion marker at boot; \
                 presumed crashed (no in-flight node recorded)",
                row.id
            ),
        };

        crate::http::stamp_failure(&mut row.task_context, &reason);
        engine_core::stamp_completion(&mut row.task_context.metadata, "failed");
        row.updated_at = now;

        lister
            .persist_reconciled(&row)
            .await
            .map_err(|err| format!("orphan sweep: failed to persist run {}: {err}", row.id))?;

        live.mark_terminal(
            row.id,
            &row.task_context,
            row.workflow_type.clone(),
            row.created_at,
            row.updated_at,
        );

        println!("orphan sweep: reconciled run {} ({reason})", row.id);
        reconciled.push(row.id);
    }

    println!(
        "orphan sweep: {scanned} candidate(s) scanned, {} reconciled",
        reconciled.len()
    );

    Ok(ReconcileSummary {
        scanned,
        reconciled,
    })
}

// ── Stale-run alarm (`EN.9.C` task 6) ──────────────────────────────────

/// Pure decision function: given `records` — the live map's `(run_id,
/// snapshot, updated_at)` triples, exactly [`LiveStateStore::list_live_records`]'s
/// shape — `now`, and the policy-resolved `stale_run_alarm_secs`, return
/// the run ids whose live status (`crate::http::derive_live_status`) is
/// `"running"` or `"suspended"` AND whose age past `updated_at` is at least
/// the threshold.
///
/// No I/O, no clock reads beyond the `now` argument, no dependency on
/// [`LiveStateStore`] itself — this is what makes it hermetically testable
/// with hand-built fixtures, no database and no real store.
#[must_use]
pub fn stale_run_ids(
    records: &[(RunId, TaskContext, DateTime<Utc>)],
    now: DateTime<Utc>,
    stale_run_alarm_secs: u64,
) -> Vec<RunId> {
    let threshold =
        chrono::Duration::seconds(i64::try_from(stale_run_alarm_secs).unwrap_or(i64::MAX));
    records
        .iter()
        .filter(|(_, snapshot, updated_at)| {
            let live_status = crate::http::derive_live_status(snapshot);
            (live_status == "running" || live_status == "suspended")
                && now.signed_duration_since(*updated_at) >= threshold
        })
        .map(|(run_id, _, _)| *run_id)
        .collect()
}

/// Render a stale-run alarm into a validated [`OperatorPayload`] — same
/// rendering discipline as `failure::render_failure_payload`: a fixed
/// header naming the run and how long it has sat past its last recorded
/// progress, a small fixed acknowledgement-only response set (nothing
/// executes on either option, matching the failure-notification
/// convention), run through `operator::validate` before ever being
/// returned. The one error path is `validate`'s own (a caller-supplied
/// `limits` too small to hold even the fixed header).
fn render_stale_run_alarm_payload(
    run_id: RunId,
    live_status: &str,
    age_secs: i64,
    limits: &OperatorPayloadLimits,
) -> Result<
    engine_core::operator::ValidatedOperatorPayload,
    engine_core::operator::OperatorValidationError,
> {
    let rendered_summary = format!(
        "Run stuck: {live_status}\nRun: {run_id}\nNo progress for {age_secs}s, past the alarm threshold"
    );
    let options = vec![
        OperatorResponseOption::new("acknowledge", "Acknowledge"),
        OperatorResponseOption::new("view_run", "View run"),
    ];
    let gate_id = format!("stale-run:{run_id}");
    let payload = OperatorPayload::new(gate_id, rendered_summary, options);
    validate(payload, limits)
}

/// The stale-run alarm: sweep `live`'s live records for any run past the
/// resolved `policy.stale_run_alarm_secs` threshold (via [`stale_run_ids`])
/// and enqueue exactly one [`OperatorQueueItem`] per such run into
/// `live.operator_queue()`, mirroring
/// `live_state.rs::maybe_enqueue_failure_notification`'s rendering
/// discipline and `orphan_item_priority` for the item's priority.
///
/// De-duplicated via [`LiveStateStore::mark_alarmed`] — the same
/// once-per-run shape `mark_terminal`'s `is_first_terminal_transition`
/// established for terminal notifications: a repeated pass over the same
/// stuck run enqueues nothing further, so one stuck run produces exactly
/// one item, not one per tick.
///
/// Never panics, never blocks: a render/validate failure for one candidate
/// (the one residual error path `render_stale_run_alarm_payload` can hit)
/// is skipped rather than propagated, matching
/// `maybe_enqueue_failure_notification`'s "an unreportable item has no
/// useful fallback, but must never block this path" contract — the sweep
/// still processes every other candidate.
///
/// Returns the number of items actually enqueued this call (0 for a
/// no-stale-runs or an already-alarmed-everything pass).
pub fn alarm_stale_runs(live: &LiveStateStore, policy: &OrphanPolicy, now: DateTime<Utc>) -> usize {
    let records = live.list_live_records();
    let stale = stale_run_ids(&records, now, policy.stale_run_alarm_secs);

    let mut enqueued = 0;
    for run_id in stale {
        if !live.mark_alarmed(run_id) {
            continue;
        }

        let Some((_, snapshot, updated_at)) = records.iter().find(|(id, _, _)| *id == run_id)
        else {
            continue;
        };
        let live_status = crate::http::derive_live_status(snapshot);
        let age_secs = now.signed_duration_since(*updated_at).num_seconds();

        let limits = OperatorPayloadLimits::default();
        let Ok(validated) = render_stale_run_alarm_payload(run_id, live_status, age_secs, &limits)
        else {
            continue;
        };

        let item = OperatorQueueItem::new(
            format!("stale-run:{run_id}"),
            validated.into_payload(),
            policy.orphan_item_priority,
            now,
            ItemSource::GateApproval,
        );

        let mut queue = live
            .operator_queue()
            .write()
            .expect("operator queue lock poisoned on write");
        queue.enqueue(item);
        drop(queue);

        enqueued += 1;
    }

    enqueued
}

// ── Periodic sweep: `alarm_stale_runs`'s first production caller ──────

/// A handle to the background stale-run sweep loop [`spawn_stale_run_sweep`]
/// spawned. Mirrors `crate::schedule::ScheduleLoopHandle`'s "hold or drop"
/// shape exactly — the same boot-wiring convention bastion already knows
/// how to call: the caller may hold this to [`abort`](Self::abort) the loop
/// (e.g. on shutdown) or drop it — dropping does **not** stop the loop (a
/// `tokio::task::JoinHandle` detaches on drop), matching how
/// `spawn_schedule_loop`'s and `spawn_durable_writer`'s handles both behave.
pub struct StaleRunSweepHandle {
    task: tokio::task::JoinHandle<()>,
}

impl StaleRunSweepHandle {
    /// Stop the background sweep loop.
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// One sweep tick: alarm every stale run in `live` against `policy`'s
/// resolved threshold, evaluated at `now`. A thin synchronous wrapper over
/// [`alarm_stale_runs`], split out so [`spawn_stale_run_sweep`]'s loop body
/// stays a one-line call and so tests can drive a tick directly with an
/// INJECTED `now` — no test may sleep on a real interval to observe this.
///
/// Returns the number of `OperatorQueueItem`s enqueued this tick (0 for a
/// no-stale-runs or an already-alarmed-everything tick — the once-per-run
/// dedup in [`LiveStateStore::mark_alarmed`] is what makes a second tick
/// over the same still-stale run enqueue nothing).
#[must_use]
pub fn sweep_stale_runs_once(
    live: &LiveStateStore,
    policy: &OrphanPolicy,
    now: DateTime<Utc>,
) -> usize {
    alarm_stale_runs(live, policy, now)
}

/// The spawnable stale-run sweep bootstrap: `tokio::spawn` a background
/// loop that calls [`sweep_stale_runs_once`] on every tick of a
/// `tokio::time::interval(interval)`, giving [`alarm_stale_runs`] — until
/// now dead code with zero production callers (see the module docs'
/// "Never silent" note and this spec's Notes) — its first one.
///
/// **The tick stays off the async path.** [`alarm_stale_runs`] takes
/// `live`'s operator-queue write lock synchronously
/// (`std::sync::RwLock::write`), so each poll runs inside
/// `tokio::task::spawn_blocking` rather than directly in the loop's async
/// block — matching `spawn_schedule_loop`'s precedent on blocking work: a
/// held lock must not stall every other task on this process's runtime.
///
/// `live` is cheap to clone (an `Arc` around each guarded map —
/// `LiveStateStore`'s own doc comment) and `policy` is `Copy`, so both are
/// captured by value into the spawned task with no extra synchronization.
///
/// The one remaining wiring step — calling this from a live process's
/// startup path — is a different repo's change (`bastion`'s
/// `serve/mod.rs`, alongside its existing `spawn_durable_writer` and
/// `spawn_schedule_loop` calls) and is out of scope here; see this
/// spec's Notes (`BA.21.B`).
pub fn spawn_stale_run_sweep(
    live: LiveStateStore,
    policy: OrphanPolicy,
    interval: std::time::Duration,
) -> StaleRunSweepHandle {
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;

            let live = live.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let now = Utc::now();
                sweep_stale_runs_once(&live, &policy, now)
            })
            .await;
        }
    });

    StaleRunSweepHandle { task }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_contract::{NodeRun, NodeRunStatus, TaskContext};
    use std::collections::HashMap;

    fn task_context_with_running_node(node: &str) -> TaskContext {
        let mut node_runs = HashMap::new();
        node_runs.insert(
            node.to_string(),
            NodeRun {
                status: NodeRunStatus::Running,
                started_at: Some(Utc::now()),
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs,
        }
    }

    fn orphan_row(id: Uuid, updated_at: DateTime<Utc>, ctx: TaskContext) -> EventsRow {
        EventsRow {
            id,
            workflow_type: "CONTENT_PIPELINE".to_string(),
            data: serde_json::json!({}),
            task_context: ctx,
            created_at: updated_at,
            updated_at,
        }
    }

    fn old_cutoff() -> DateTime<Utc> {
        Utc::now()
    }

    #[tokio::test]
    async fn reconciles_every_candidate_and_names_the_stuck_node() {
        let id = Uuid::new_v4();
        let ctx = task_context_with_running_node("SendEmailNode");
        let lister = RecordingOrphanLister::new(vec![orphan_row(
            id,
            Utc::now() - chrono::Duration::hours(2),
            ctx,
        )]);
        let live = LiveStateStore::new();

        let summary = reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("sweep should succeed");

        assert_eq!(summary.scanned, 1);
        assert_eq!(summary.reconciled, vec![id]);

        let row = lister
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .expect("row still present");
        assert_eq!(row.task_context.metadata["failure"]["failed"], true);
        assert!(row.task_context.metadata["failure"]["error"]
            .as_str()
            .unwrap()
            .contains("SendEmailNode"));
        assert_eq!(row.task_context.metadata["completion"]["terminal"], true);
        assert_eq!(row.task_context.metadata["completion"]["status"], "failed");

        // Regression: a run this sweep reconciles must be readable via the
        // completed ring immediately — no second sweep, no 404.
        let record = live
            .get_record(id)
            .expect("reconciled run must be seeded into LiveStateStore");
        assert!(record.terminal);
        assert_eq!(record.snapshot.metadata["completion"]["status"], "failed");
    }

    #[tokio::test]
    async fn reconciles_multiple_candidates() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let old = Utc::now() - chrono::Duration::hours(3);
        let lister = RecordingOrphanLister::new(vec![
            orphan_row(id_a, old, task_context_with_running_node("NodeA")),
            orphan_row(id_b, old, task_context_with_running_node("NodeB")),
        ]);
        let live = LiveStateStore::new();

        let summary = reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("sweep should succeed");

        assert_eq!(summary.scanned, 2);
        assert_eq!(summary.reconciled.len(), 2);
        assert!(summary.reconciled.contains(&id_a));
        assert!(summary.reconciled.contains(&id_b));
    }

    #[tokio::test]
    async fn second_sweep_over_the_same_database_finds_nothing() {
        let id = Uuid::new_v4();
        let old = Utc::now() - chrono::Duration::hours(2);
        let lister = RecordingOrphanLister::new(vec![orphan_row(
            id,
            old,
            task_context_with_running_node("NodeA"),
        )]);
        let live = LiveStateStore::new();

        let first = reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("first sweep should succeed");
        assert_eq!(first.scanned, 1);

        let second = reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("second sweep should succeed");
        assert_eq!(second.scanned, 0);
        assert!(second.reconciled.is_empty());
    }

    #[tokio::test]
    async fn no_ops_when_reconcile_on_boot_is_disabled() {
        let id = Uuid::new_v4();
        let old = Utc::now() - chrono::Duration::hours(2);
        let lister = RecordingOrphanLister::new(vec![orphan_row(
            id,
            old,
            task_context_with_running_node("NodeA"),
        )]);
        let policy = OrphanPolicy {
            reconcile_on_boot: false,
            ..OrphanPolicy::default()
        };
        let live = LiveStateStore::new();

        let summary = reconcile_orphans(&lister, &live, &policy, old_cutoff())
            .await
            .expect("no-op sweep should still return Ok");

        assert_eq!(summary, ReconcileSummary::default());
        // Confirm nothing was actually reconciled: the row is untouched.
        let row = lister.rows().into_iter().find(|r| r.id == id).unwrap();
        assert!(!engine_core::is_complete(&row.task_context.metadata));
    }

    #[tokio::test]
    async fn surfaces_a_lister_error_rather_than_swallowing_it() {
        let lister = RecordingOrphanLister::failing_list("connection refused");
        let live = LiveStateStore::new();

        let err = reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect_err("a lister error must propagate");

        assert!(err.contains("connection refused"));
    }

    #[tokio::test]
    async fn reason_omits_node_name_when_none_was_running() {
        let id = Uuid::new_v4();
        let old = Utc::now() - chrono::Duration::hours(2);
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let lister = RecordingOrphanLister::new(vec![orphan_row(id, old, ctx)]);
        let live = LiveStateStore::new();

        reconcile_orphans(&lister, &live, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("sweep should succeed");

        let row = lister.rows().into_iter().find(|r| r.id == id).unwrap();
        assert!(row.task_context.metadata["failure"]["error"]
            .as_str()
            .unwrap()
            .contains("no in-flight node recorded"));
    }

    #[test]
    fn stuck_node_name_picks_the_running_entry() {
        let ctx = task_context_with_running_node("OnlyRunningNode");
        assert_eq!(stuck_node_name(&ctx), Some("OnlyRunningNode".to_string()));
    }

    #[test]
    fn stuck_node_name_none_when_nothing_running() {
        let mut node_runs = HashMap::new();
        node_runs.insert(
            "DoneNode".to_string(),
            NodeRun {
                status: NodeRunStatus::Success,
                started_at: None,
                completed_at: None,
                error: None,
                input: None,
                usage: None,
            },
        );
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs,
        };
        assert_eq!(stuck_node_name(&ctx), None);
    }

    // ── Stale-run alarm (task 6) ───────────────────────────────────────

    fn running_context() -> TaskContext {
        task_context_with_running_node("SomeNode")
    }

    fn suspended_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({ "suspension": { "suspended": true } }),
            node_runs: HashMap::new(),
        }
    }

    fn terminal_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({ "completion": { "terminal": true, "status": "succeeded" } }),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn fresh_run_does_not_alarm() {
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let records = vec![(run_id, running_context(), now)];

        let stale = stale_run_ids(&records, now, 3600);

        assert!(stale.is_empty());
    }

    #[test]
    fn run_past_the_threshold_alarms() {
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let updated_at = now - chrono::Duration::seconds(3601);
        let records = vec![(run_id, running_context(), updated_at)];

        let stale = stale_run_ids(&records, now, 3600);

        assert_eq!(stale, vec![run_id]);
    }

    #[test]
    fn suspended_run_past_the_threshold_also_alarms() {
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let updated_at = now - chrono::Duration::seconds(7200);
        let records = vec![(run_id, suspended_context(), updated_at)];

        let stale = stale_run_ids(&records, now, 3600);

        assert_eq!(stale, vec![run_id]);
    }

    #[test]
    fn terminal_run_never_alarms_regardless_of_age() {
        // A terminal snapshot's `derive_live_status` still reports
        // "running" (it never re-derives terminality — see the function's
        // own docs) so this exercises the real defense: `mark_terminal`
        // removes a run from the live map entirely, so it can never appear
        // in `list_live_records` in the first place. This test asserts the
        // integration-level guarantee via `LiveStateStore`, not the pure
        // function alone.
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        store.record(run_id, &terminal_context());
        store.mark_terminal(run_id, &terminal_context(), "wf", now, now);

        let far_future = now + chrono::Duration::hours(10);
        let enqueued = alarm_stale_runs(&store, &OrphanPolicy::default(), far_future);

        assert_eq!(enqueued, 0);
    }

    #[test]
    fn threshold_is_policy_resolved_not_hardcoded() {
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let updated_at = now - chrono::Duration::seconds(100);
        let records = vec![(run_id, running_context(), updated_at)];

        // Below a 3600s default threshold: not stale.
        assert!(stale_run_ids(&records, now, 3600).is_empty());
        // But stale against a tighter, policy-resolved 60s threshold.
        assert_eq!(stale_run_ids(&records, now, 60), vec![run_id]);
    }

    #[test]
    fn alarm_stale_runs_enqueues_exactly_one_item_for_a_stale_run() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        store.record(run_id, &running_context());

        let far_future = now + chrono::Duration::hours(2);
        let policy = OrphanPolicy::default();
        let enqueued = alarm_stale_runs(&store, &policy, far_future);

        assert_eq!(enqueued, 1);
        let queue_len = store
            .operator_queue()
            .read()
            .expect("queue lock")
            .pending_count();
        assert_eq!(queue_len, 1);
    }

    #[test]
    fn a_second_pass_over_the_same_stuck_run_enqueues_nothing_further() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        store.record(run_id, &running_context());

        let far_future = now + chrono::Duration::hours(2);
        let policy = OrphanPolicy::default();

        let first = alarm_stale_runs(&store, &policy, far_future);
        assert_eq!(first, 1);

        let second = alarm_stale_runs(&store, &policy, far_future + chrono::Duration::hours(1));
        assert_eq!(second, 0);

        let queue_len = store
            .operator_queue()
            .read()
            .expect("queue lock")
            .pending_count();
        assert_eq!(
            queue_len, 1,
            "one stuck run must produce one item, not one per tick"
        );
    }

    // ── Periodic sweep (task 2) ─────────────────────────────────────────

    #[test]
    fn sweep_stale_runs_once_enqueues_exactly_one_item_for_a_stale_run() {
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        store.record(run_id, &running_context());

        let far_future = now + chrono::Duration::hours(2);
        let policy = OrphanPolicy::default();
        let enqueued = sweep_stale_runs_once(&store, &policy, far_future);

        assert_eq!(enqueued, 1);
        let queue_len = store
            .operator_queue()
            .read()
            .expect("queue lock")
            .pending_count();
        assert_eq!(queue_len, 1);
    }

    #[test]
    fn a_second_sweep_tick_over_the_same_stale_run_enqueues_nothing() {
        // The specific thing a *periodic* caller can break that a single
        // `alarm_stale_runs` call cannot: does `mark_alarmed`'s
        // once-per-run dedup survive being driven by a loop, tick after
        // tick, over the same still-stale run?
        let store = LiveStateStore::new();
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        store.record(run_id, &running_context());

        let far_future = now + chrono::Duration::hours(2);
        let policy = OrphanPolicy::default();

        let first_tick = sweep_stale_runs_once(&store, &policy, far_future);
        assert_eq!(first_tick, 1);

        let second_tick =
            sweep_stale_runs_once(&store, &policy, far_future + chrono::Duration::hours(1));
        assert_eq!(second_tick, 0);

        let queue_len = store
            .operator_queue()
            .read()
            .expect("queue lock")
            .pending_count();
        assert_eq!(
            queue_len, 1,
            "one stale run must produce one item across the whole tick sequence, not one per tick"
        );
    }

    // ── Fixture evidence: both directions of the threshold (task 3) ────
    //
    // These two tests are the declared standing fixture for the block's
    // `gateable: false` acceptance criterion ("the sweep actually fires
    // inside a running `bastion serve` process"). That evidence lives in
    // another repo (bastion's boot wiring, BA.21.B) and in the Mini's
    // INSTALLED binary — it cannot be observed from this repo's `cargo`
    // checks. What stands in for it here is an engine-side test that
    // drives the alarm's pure decision function (`stale_run_ids`, the same
    // function `sweep_stale_runs_once`/`alarm_stale_runs` call) with an
    // injected clock and asserts the enqueue decision at every tick of a
    // simulated tick-set — never a real sleep, never a real interval.

    #[test]
    fn a_long_legitimate_chain_of_per_step_progress_is_never_flagged_stale() {
        // Simulates `integrate_chain`'s per-step progress restamping:
        // `OrchestrationRunNode::with_step_observer` (graph.rs:337) fans
        // out through `engine_serve::suspend::progress_fanout` to
        // `LiveStateStore::record`, which restamps `updated_at` once per
        // COMPLETED STEP, not once per whole chain (see EN.12.A's
        // amendment note in `planning/blocks/EN.12.A.json`). Because of
        // that, the quiet window a legitimately long chain exposes to the
        // alarm is bounded by ONE block's duration, not by chain length.
        //
        // This test is written to FAIL if that per-step restamping were
        // removed: it drives `stale_run_ids` — the exact function the
        // sweep calls — against a synthetic `updated_at` series that only
        // stays "fresh" because it is restamped after every simulated
        // step. Stop restamping (i.e. hold `updated_at` at the chain's
        // start instead of advancing it per step) and the very same
        // `stale_run_ids` call would flag the run once the cumulative gap
        // (STEPS * PER_STEP_MAX_SECS) exceeds the derived threshold — which
        // it does well before 8 steps (8 * 2640s = 21,120s >> 5280s).
        let run_id = Uuid::new_v4();
        let ctx = running_context();
        let policy = OrphanPolicy::default();
        let threshold = policy.stale_run_alarm_secs;

        const STEPS: i64 = 8;
        const PER_STEP_MAX_SECS: i64 =
            engine_core::operator::orphan::OBSERVED_PER_BLOCK_MAX_SECS as i64;

        let mut updated_at = Utc::now();
        let mut clock = updated_at;
        for step in 0..STEPS {
            // Advance the wall clock by the measured per-block MAXIMUM
            // before this step's progress event lands — the worst case
            // quiet window a single in-flight block can produce.
            clock += chrono::Duration::seconds(PER_STEP_MAX_SECS);

            let records = vec![(run_id, ctx.clone(), updated_at)];
            let stale = stale_run_ids(&records, clock, threshold);
            assert!(
                stale.is_empty(),
                "step {step}: a block running at the measured per-block maximum must not trip \
                 the alarm — quiet window {PER_STEP_MAX_SECS}s vs threshold {threshold}s"
            );

            // The step completes: its progress event restamps `updated_at`
            // to now, exactly like `LiveStateStore::record` does in
            // production. Without this line, the loop reproduces the
            // pre-amendment monolithic-node model and the assertion above
            // fails partway through the sequence.
            updated_at = clock;
        }
    }

    #[test]
    fn a_run_idle_past_the_derived_threshold_with_no_step_progress_is_flagged() {
        // The opposite direction, per carryover
        // `gate-scope-must-be-shown-capable-of-failing`: a one-directional
        // test (only "long legitimate chain is never flagged") would pass
        // trivially against a threshold set to infinity. This asserts the
        // alarm still fires when there is genuinely no step progress.
        let run_id = Uuid::new_v4();
        let ctx = running_context();
        let policy = OrphanPolicy::default();
        let threshold = policy.stale_run_alarm_secs;

        let updated_at = Utc::now();
        // No intervening step progress restamps `updated_at` — the run
        // sits idle past the derived threshold with a single second to
        // spare, so this fails a threshold set uselessly high too.
        let now = updated_at + chrono::Duration::seconds(threshold as i64 + 1);
        let records = vec![(run_id, ctx, updated_at)];

        let stale = stale_run_ids(&records, now, threshold);

        assert_eq!(stale, vec![run_id]);
    }
}
