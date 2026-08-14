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
use engine_contract::EventsRow;
use engine_core::operator::orphan::OrphanPolicy;
use sqlx::PgPool;
use uuid::Uuid;

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
pub async fn reconcile_orphans(
    lister: &dyn OrphanLister,
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

        let summary = reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
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

        let summary = reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
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

        let first = reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
            .await
            .expect("first sweep should succeed");
        assert_eq!(first.scanned, 1);

        let second = reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
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

        let summary = reconcile_orphans(&lister, &policy, old_cutoff())
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

        let err = reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
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

        reconcile_orphans(&lister, &OrphanPolicy::default(), old_cutoff())
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
}
