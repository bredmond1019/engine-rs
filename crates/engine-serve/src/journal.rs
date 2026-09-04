//! The durable run journal read route (EN.12.D task 5): `GET
//! /campaigns/{id}/journal`.
//!
//! Journal rows have no in-memory counterpart the way `AppState::live` is for
//! events — they persist only in Postgres, written append-only by the
//! background durable writer (`crate::durable`) via
//! `engine_store::insert_journal_row`. This route reads them back with
//! `engine_store::list_journal_rows_for_campaign`, addressed purely by
//! `campaign_id` — no repo, no roadmap, no second derivation — so a
//! repo-less run (no roadmap, no repo at all) is just as addressable as a
//! repo-scoped one. That is the whole point of the durable half over the
//! D57 rendered half (`notes.md`/`review.md`, added in task 6), which can
//! only ever describe a repo-scoped run.
//!
//! With no `DATABASE_URL` configured (`state.durable.pool()` is `None`),
//! this route self-skips exactly like the write path
//! (`crate::durable::spawn_durable_writer`'s pool-is-`None` branch): there is
//! nothing to serve, so it answers identically to an unknown campaign — a
//! clean `404`, never a `500`. This mirrors `crate::resume::rehydrate_from_store`,
//! which returns `None` on a missing pool so its caller 404s uniformly.

use std::sync::{Arc, OnceLock, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use async_trait::async_trait;
use engine_contract::{JournalDecisionKind, JournalRow};
use engine_core::workflows::orchestration::debrief::JournalReader;
use engine_core::workflows::orchestration::integrate::JournalSinkFn;
use sqlx::PgPool;
use uuid::Uuid;

use crate::durable::DurableHandle;

use crate::http::{check_api_key, AppState};

/// `GET /campaigns/{id}/journal` — `200 {campaign_id, rows: [JournalRow, ...]}`
/// ordered oldest-decision-first (`list_journal_rows_for_campaign`'s own
/// ordering), or `404` for a malformed/non-UUID path segment, an unknown
/// campaign, or a self-skip (no `DATABASE_URL`). `X-API-Key` gated like
/// every other campaign/run route.
pub async fn get_campaign_journal(
    path: web::Path<String>,
    req: HttpRequest,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let raw_id = path.into_inner();
    let campaign_id = match Uuid::parse_str(&raw_id) {
        Ok(id) => id,
        Err(_) => {
            return HttpResponse::NotFound()
                .json(serde_json::json!({ "error": "unknown or malformed campaign_id" }));
        }
    };

    let Some(pool) = state.durable.pool() else {
        // No DATABASE_URL configured: the journal write path self-skips
        // (durable.rs) and there is nothing durable to read back either.
        // Treat identically to an unknown campaign rather than a 500 — a
        // caller cannot distinguish "never happened" from "not persisted
        // in this deployment" and should not need to.
        return HttpResponse::NotFound()
            .json(serde_json::json!({ "error": "unknown or malformed campaign_id" }));
    };

    let rows =
        match engine_store::list_journal_rows_for_campaign(pool, &campaign_id.to_string()).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(
                    campaign_id = %campaign_id,
                    error = %err,
                    "journal read: list_journal_rows_for_campaign failed"
                );
                Vec::new()
            }
        };

    if rows.is_empty() {
        return HttpResponse::NotFound()
            .json(serde_json::json!({ "error": "unknown or malformed campaign_id" }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "campaign_id": campaign_id,
        "rows": rows,
    }))
}

// ---------------------------------------------------------------------------
// EN.12.D task 6 — the D57 `notes.md`/`review.md` renderer.
//
// `JournalRow` (`engine_contract::journal`) is deliberately repo/roadmap-
// agnostic — it keys on `campaign_id`/`run_id` only, per EN.11.E/EN.11.G,
// so a repo-less run is addressable with no second derivation (the read
// route above). D57's run-record contract, by contrast, is addressed per
// `(repo x roadmap)` (`planning/decisions/D57-orchestration-run-artifact-
// contract.md` section 1) and requires frontmatter journal rows cannot
// supply on their own (`roadmap`, `lane`, `repo`). `RunRecordMeta` carries
// that missing context; the renderer below is therefore only ever called
// for a repo-scoped run, exactly as the block record requires. A repo-less
// campaign stays addressable through the read route alone and never goes
// through this renderer.
// ---------------------------------------------------------------------------

/// The (repo x roadmap) identity a D57 run record is addressed by. Journal
/// rows carry no such context (see the module doc above), so a caller
/// rendering `notes.md`/`review.md` for a repo-scoped run supplies it here.
#[derive(Debug, Clone)]
pub struct RunRecordMeta {
    pub repo: String,
    pub roadmap: String,
    pub lane: String,
    pub run_started: String,
    /// `None` while the lane is still running — `lifecycle: active`, no
    /// `run_ended` frontmatter value (D57 section 2).
    pub run_ended: Option<String>,
}

impl RunRecordMeta {
    /// D57 section 2: `active` while running, `lane-complete` once the lane
    /// has closed. This renderer never emits `consolidated` — that lifecycle
    /// value is stamped only by `/consolidate-run`, outside this repo.
    fn lifecycle(&self) -> &'static str {
        if self.run_ended.is_some() {
            "lane-complete"
        } else {
            "active"
        }
    }
}

/// One journal decision rendered as a D57 ledger/notes item. `label` is the
/// bold status marker `roadmap_status_discovery.py`'s `_OPEN_ROW_RE` /
/// `_HELD_ROW_RE` count (`**OPEN**` / `**HELD**`, case-insensitive) — every
/// other kind renders as plain `DONE`, which that script does not count but
/// D57's own vocabulary (`OPEN` / `DONE` / `HELD` / `WONTFIX`) requires.
fn ledger_label(kind: JournalDecisionKind) -> &'static str {
    match kind {
        JournalDecisionKind::StepBailed
        | JournalDecisionKind::GateRefused
        | JournalDecisionKind::StateWriteVerificationFailed => "OPEN",
        JournalDecisionKind::BudgetHalted => "HELD",
        JournalDecisionKind::StepIntegrated
        | JournalDecisionKind::ResolvedPolicy
        | JournalDecisionKind::RecallConsulted
        | JournalDecisionKind::DebriefRendered
        | JournalDecisionKind::ConductorProposed => "DONE",
    }
}

fn kind_title(kind: JournalDecisionKind) -> &'static str {
    match kind {
        JournalDecisionKind::StepIntegrated => "step integrated",
        JournalDecisionKind::StepBailed => "step bailed",
        JournalDecisionKind::GateRefused => "gate refused",
        JournalDecisionKind::StateWriteVerificationFailed => "state-write verification failed",
        JournalDecisionKind::BudgetHalted => "budget halted",
        JournalDecisionKind::ResolvedPolicy => "resolved policy",
        JournalDecisionKind::RecallConsulted => "recall consulted",
        JournalDecisionKind::DebriefRendered => "debrief rendered",
        JournalDecisionKind::ConductorProposed => "conductor proposed",
    }
}

fn frontmatter(
    campaign_id: &Uuid,
    meta: &RunRecordMeta,
    doc_kind: &str,
    description: &str,
) -> String {
    let related = format!("{}-orchestration-run-{}", meta.repo, meta.roadmap);
    format!(
        "---\n\
type: Reference\n\
title: \"Orchestration {doc_kind} — {repo}, {roadmap} (campaign {campaign_id})\"\n\
description: \"{description}\"\n\
doc_id: {repo}-orchestration-run-{roadmap}-{doc_kind}\n\
layer: [engine]\n\
project: {repo}\n\
status: active\n\
keywords: [orchestration, journal, {doc_kind}, campaign]\n\
roadmap: {roadmap}\n\
lane: {lane}\n\
run_started: {run_started}\n\
run_ended: {run_ended}\n\
lifecycle: {lifecycle}\n\
related: [{related}]\n\
---\n\n",
        doc_kind = doc_kind,
        repo = meta.repo,
        roadmap = meta.roadmap,
        campaign_id = campaign_id,
        description = description,
        lane = meta.lane,
        run_started = meta.run_started,
        run_ended = meta.run_ended.clone().unwrap_or_default(),
        lifecycle = meta.lifecycle(),
        related = related,
    )
}

/// Renders the D57 `notes.md` view for a repo-scoped run: the running tab of
/// items, one bullet per journal row, each carrying the `**OPEN**` /
/// `**HELD**` / `DONE` marker `roadmap_status_discovery.py`'s
/// `discover_run_records` counts (`_OPEN_ROW_RE`/`_HELD_ROW_RE`) and
/// `/consolidate-run`'s Step 4 selection reads via the `roadmap:` frontmatter
/// field above. Carries no token count, cost, or attempt count — the rendered
/// half is read by two out-of-repo parsers and telemetry is out of scope for
/// this block entirely.
pub fn render_notes_md(campaign_id: &Uuid, rows: &[JournalRow], meta: &RunRecordMeta) -> String {
    let mut out = frontmatter(
        campaign_id,
        meta,
        "notes",
        "Running tab of journal decisions for this campaign, rendered from the durable run journal.",
    );
    out.push_str(&format!(
        "# Orchestration run — `{}` / {} lane `{}`\n\n",
        meta.roadmap, meta.repo, meta.lane
    ));
    out.push_str(
        "Running tab so findings do not get buried. Each item carries a status: `OPEN` · `DONE` · `HELD` · `WONTFIX`.\n\n",
    );
    out.push_str("## Journal\n\n");
    if rows.is_empty() {
        out.push_str("No journal rows for this campaign yet.\n");
        return out;
    }
    for row in rows {
        let label = ledger_label(row.kind);
        out.push_str(&format!(
            "- **{label}** — {step}: {title} — {reason}\n",
            label = label,
            step = row.step,
            title = kind_title(row.kind),
            reason = row.reason,
        ));
    }
    out
}

/// Renders the D57 `review.md` view for a repo-scoped run: the block ledger
/// table plus a plain-English summary, from the same rows `notes.md` above
/// renders. `origin_roadmap` on every row defaults to `meta.roadmap` — this
/// renderer never adopts a block from another roadmap, so the column is
/// always the record's own roadmap slug (D57 section 3).
pub fn render_review_md(campaign_id: &Uuid, rows: &[JournalRow], meta: &RunRecordMeta) -> String {
    let mut out = frontmatter(
        campaign_id,
        meta,
        "review",
        "What this campaign's journal recorded and why, rendered from the durable run journal.",
    );
    out.push_str(&format!(
        "# Orchestration review — {}, {}\n\n",
        meta.repo, meta.roadmap
    ));
    out.push_str("## Block ledger\n\n");
    out.push_str("| Step | Origin roadmap | Decision | Outcome |\n|---|---|---|---|\n");
    if rows.is_empty() {
        out.push_str("| — | — | — | no journal rows for this campaign |\n");
    } else {
        for row in rows {
            out.push_str(&format!(
                "| `{step}` | {roadmap} | {kind} | **{label}** — {reason} |\n",
                step = row.step,
                roadmap = meta.roadmap,
                kind = kind_title(row.kind),
                label = ledger_label(row.kind),
                reason = row.reason,
            ));
        }
    }
    out.push_str("\n## What changed, in plain English\n\n");
    let bailed = rows
        .iter()
        .filter(|r| r.kind == JournalDecisionKind::StepBailed)
        .count();
    let halted = rows
        .iter()
        .filter(|r| r.kind == JournalDecisionKind::BudgetHalted)
        .count();
    let integrated = rows
        .iter()
        .filter(|r| r.kind == JournalDecisionKind::StepIntegrated)
        .count();
    out.push_str(&format!(
        "This campaign integrated {integrated} step(s), bailed {bailed} time(s), and was halted by a budget cap {halted} time(s).\n",
    ));
    out
}

// ---------------------------------------------------------------------------
// EN.12.G task 1 — the live `JournalReader` seam.
//
// `engine-core` depends only on `engine-contract` and cannot call
// `engine_store::list_journal_rows_for_campaign` directly, so the debrief
// (`engine_core::workflows::orchestration::debrief`) reads through an
// injectable trait. This is the one production implementation, over the
// same `engine_store` function the read route above already uses.
// ---------------------------------------------------------------------------

/// The live [`JournalReader`]: reads a campaign's rows through
/// `engine_store::list_journal_rows_for_campaign` over an optional pool.
///
/// Self-skips exactly like the read route above and the durable write path
/// (`crate::durable::spawn_durable_writer`'s pool-is-`None` branch) when no
/// `DATABASE_URL` is configured: `rows_for_campaign` returns an empty `Vec`
/// rather than an error, since "no pool configured" is a deployment fact,
/// not a per-campaign read failure — a debrief run against it should render
/// "nothing ran" rather than fail the node outright.
#[derive(Clone)]
pub struct LiveJournalReader {
    pool: Option<PgPool>,
}

impl LiveJournalReader {
    #[must_use]
    pub fn new(pool: Option<PgPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JournalReader for LiveJournalReader {
    async fn rows_for_campaign(&self, campaign_id: &Uuid) -> Result<Vec<JournalRow>, String> {
        let Some(pool) = self.pool.as_ref() else {
            // No DATABASE_URL configured: mirror the durable write path's
            // self-skip discipline rather than erroring.
            return Ok(Vec::new());
        };

        engine_store::list_journal_rows_for_campaign(pool, &campaign_id.to_string())
            .await
            .map_err(|err| format!("journal read failed for campaign {campaign_id}: {err}"))
    }
}

/// Convenience constructor: an `Arc<dyn JournalReader>` wrapping
/// [`LiveJournalReader`], for `engine-serve`'s `register_debrief` (task 4)
/// to wire into `DebriefNode`.
#[must_use]
pub fn journal_reader_live(pool: Option<PgPool>) -> Arc<dyn JournalReader> {
    Arc::new(LiveJournalReader::new(pool))
}

// ---------------------------------------------------------------------------
// EN.14.E task 4 — the process-global journal `DurableHandle` seam.
//
// `register_debrief`'s (`workflows.rs`) `WorkflowFactory` closure has the
// same shape problem `workflows.rs`'s `repo_registry_cell`/`set_repo_registry`
// pattern already solves for `RepoRegistry`: `engine_core::dispatch::Dispatcher`
// registers a bare `Fn(&serde_json::Value) -> Result<Workflow, String>` per
// `workflow_type`, with no way to thread `AppState`/`DurableHandle` through
// the call — the factory is registered once (`register_builtin_workflows`,
// called at process start-up before `AppState`/its Postgres pool exist) but
// invoked fresh on every `dispatch_with_event` call thereafter, by which
// point a real handle (and its pool) may well exist. This cell is the same
// fix, specialized to the one `DurableHandle` `register_debrief` needs: its
// reader (`journal_reader_live`) and its sink (`journal_sink_live`) both
// resolve it at *dispatch* time, not at registration time, exactly mirroring
// `repo_registry()`'s own resolution point.
//
// Nothing in this crate installs the handle yet — the real handle (wrapping
// a live Postgres pool) is built by whichever binary constructs `AppState`
// (`bastion`'s `serve/mod.rs`, out of this repo, the same boundary
// `AppState`'s own doc comment names for `campaigns`/`runs`), via
// `set_journal_durable_handle`. With none installed, both the reader and the
// sink self-skip exactly as they did before this task: an absent pool reads
// back an empty `Vec` ([`LiveJournalReader::rows_for_campaign`]) and a
// dropped journal row is simply never written, matching
// `DurableHandle::send_journal`'s own swallowed-error contract for a `None`
// writer_pool.
fn journal_durable_cell() -> &'static RwLock<Option<DurableHandle>> {
    static JOURNAL_DURABLE: OnceLock<RwLock<Option<DurableHandle>>> = OnceLock::new();
    JOURNAL_DURABLE.get_or_init(|| RwLock::new(None))
}

/// Install the process-global `DurableHandle` [`register_debrief`]'s reader
/// and sink resolve at dispatch time. Overwrites any previously installed
/// handle — a test that installs one for the duration of a case should
/// restore the previous value (typically `None`) on the way out, mirroring
/// `workflows::set_repo_registry`'s own contract.
pub fn set_journal_durable_handle(handle: DurableHandle) {
    if let Ok(mut guard) = journal_durable_cell().write() {
        *guard = Some(handle);
    }
}

/// Install the production journal `DurableHandle` at real server boot.
///
/// This is a thin, documented wrapper over [`set_journal_durable_handle`] —
/// same effect, but named and doc'd for the one call site that matters: it
/// **must be called exactly once**, at real server boot, with a
/// pool-derived `DurableHandle` built by [`spawn_durable_writer`]
/// (`crate::durable::spawn_durable_writer`). Calling it more than once
/// simply overwrites the previously installed handle, matching
/// `set_journal_durable_handle`'s own last-write-wins contract.
///
/// **The actual call site lives outside this repo**, in `core/bastion`'s
/// server-boot path (wherever it constructs `AppState` and its Postgres
/// pool) — the same boundary `AppState`'s own doc comment and this module's
/// header comment already name for `campaigns`/`runs`. Nothing in
/// `engine-serve` calls this function in production; it exists so that
/// boundary has a documented, discoverable entrypoint to call instead of
/// reaching for the lower-level `set_journal_durable_handle` (which stays
/// available for internal/test use and is not being renamed or removed).
///
/// [`spawn_durable_writer`]: crate::durable::spawn_durable_writer
pub fn install_durable_handle(handle: DurableHandle) {
    set_journal_durable_handle(handle);
}

/// Clear the process-global journal `DurableHandle`, restoring the
/// "no handle installed" self-skip default.
pub fn clear_journal_durable_handle() {
    if let Ok(mut guard) = journal_durable_cell().write() {
        *guard = None;
    }
}

/// Read the currently installed process-global journal `DurableHandle`, if
/// any. `DurableHandle` is cheaply `Clone` (an `mpsc::UnboundedSender` plus
/// an `Option<PgPool>`), matching `repo_registry`'s own clone-out-of-the-lock
/// shape.
pub fn journal_durable_handle() -> Option<DurableHandle> {
    journal_durable_cell()
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// The live journal sink `register_debrief` wires into
/// [`engine_core::workflows::orchestration::graph::debrief_registry`]: every
/// [`JournalRow`] `DebriefNode` decides to write is forwarded to
/// [`DurableHandle::send_journal`] on whichever handle is currently
/// installed (see [`journal_durable_handle`]). With none installed the row
/// is simply dropped — the same "no `DATABASE_URL`, no durable write"
/// self-skip every other seam in this module already applies, never an
/// error or a panic. This is `send_journal`'s first production caller
/// (previously `#[cfg(test)]`-only).
#[must_use]
pub fn journal_sink_live() -> Arc<JournalSinkFn> {
    Arc::new(|row: JournalRow| {
        if let Some(handle) = journal_durable_handle() {
            handle.send_journal(row);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{test, web, App};
    use engine_core::dispatch::Dispatcher;
    use uuid::Uuid;

    use crate::http::{configure, AppState};
    use crate::live_state::LiveStateStore;

    fn test_app_state() -> AppState {
        AppState::builder(
            Arc::new(Dispatcher::new()),
            LiveStateStore::new(),
            crate::durable::spawn_durable_writer(None),
            "test-key".to_string(),
        )
        .build()
    }

    /// EN.12.G task 1: with no pool configured, `LiveJournalReader`
    /// self-skips to an empty `Vec` rather than an error — the same
    /// discipline `get_campaign_journal`'s pool-is-`None` branch applies.
    #[tokio::test]
    async fn live_journal_reader_self_skips_to_empty_rows_with_no_pool() {
        let reader = super::journal_reader_live(None);

        let rows = reader
            .rows_for_campaign(&Uuid::new_v4())
            .await
            .expect("self-skip returns Ok, not Err");

        assert!(rows.is_empty());
    }

    /// EN.14.E task 4: with no process-global `DurableHandle` installed
    /// (the default, and the state every other test in this binary leaves
    /// it in — `nextest` forks a process per test, so there is no
    /// cross-test contention on this static), [`super::journal_durable_handle`]
    /// reads back `None`.
    #[::core::prelude::v1::test]
    fn journal_durable_handle_defaults_to_none() {
        assert!(super::journal_durable_handle().is_none());
    }

    /// Round-trips [`super::set_journal_durable_handle`] /
    /// [`super::journal_durable_handle`] / [`super::clear_journal_durable_handle`]
    /// against a channel-backed test handle (`crate::durable::test_handle`,
    /// no live pool) — the same seam `register_debrief`'s dispatch-time
    /// resolution (`workflows.rs`) reads through.
    #[::core::prelude::v1::test]
    fn journal_durable_handle_round_trips_through_set_and_clear() {
        let (handle, _receiver) = crate::durable::test_handle();

        super::set_journal_durable_handle(handle);
        assert!(
            super::journal_durable_handle().is_some(),
            "installed handle should read back Some"
        );

        super::clear_journal_durable_handle();
        assert!(
            super::journal_durable_handle().is_none(),
            "cleared handle should read back None again"
        );
    }

    /// Task 2 (EN.ticket.wire-shipped-but-unreachable-seams): the production
    /// installer entrypoint, [`super::install_durable_handle`], behaves
    /// identically to [`super::set_journal_durable_handle`] — installs a
    /// test handle, confirms [`super::journal_durable_handle`] reads back
    /// `Some`, confirms [`super::journal_sink_live`] forwards a row through
    /// it, then clears it.
    #[tokio::test]
    async fn install_durable_handle_installs_a_working_journal_sink() {
        let (handle, mut receiver) = crate::durable::test_handle();

        super::install_durable_handle(handle);
        assert!(
            super::journal_durable_handle().is_some(),
            "install_durable_handle should install a handle that reads back Some"
        );

        let sink = super::journal_sink_live();
        let row = super::JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-task2-installer".to_string(),
            run_id: Uuid::new_v4(),
            step: "debrief".to_string(),
            kind: engine_contract::JournalDecisionKind::RecallConsulted,
            reason: "task 2 installer entrypoint test".to_string(),
            detail: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        sink(row.clone());

        super::clear_journal_durable_handle();

        let received = receiver
            .try_recv()
            .expect("sink should have sent the row onto the installed handle's channel");
        match received {
            crate::durable::DurableItem::Journal(received_row) => {
                assert_eq!(received_row.id, row.id);
                assert_eq!(received_row.campaign_id, row.campaign_id);
            }
            other => panic!("expected DurableItem::Journal, got {other:?}"),
        }

        assert!(
            super::journal_durable_handle().is_none(),
            "cleared handle should read back None again"
        );
    }

    /// EN.14.E task 4 — `send_journal`'s first production caller
    /// (previously `#[cfg(test)]`-only, per the block's own acceptance
    /// criteria): with a `DurableHandle` installed, [`super::journal_sink_live`]'s
    /// closure forwards a [`JournalRow`] through `DurableHandle::send_journal`
    /// onto that handle's channel — asserted here by reading it straight
    /// back off the test-handle receiver, with no live Postgres involved.
    #[tokio::test]
    async fn journal_sink_live_forwards_a_row_to_the_installed_handle() {
        let (handle, mut receiver) = crate::durable::test_handle();
        super::set_journal_durable_handle(handle);

        let sink = super::journal_sink_live();
        let row = super::JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-task4".to_string(),
            run_id: Uuid::new_v4(),
            step: "debrief".to_string(),
            kind: engine_contract::JournalDecisionKind::RecallConsulted,
            reason: "task 4 sink wiring test".to_string(),
            detail: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        sink(row.clone());

        super::clear_journal_durable_handle();

        let received = receiver
            .try_recv()
            .expect("sink should have sent the row onto the installed handle's channel");
        match received {
            crate::durable::DurableItem::Journal(received_row) => {
                assert_eq!(received_row.id, row.id);
                assert_eq!(received_row.campaign_id, row.campaign_id);
            }
            other => panic!("expected DurableItem::Journal, got {other:?}"),
        }
    }

    /// With no `DurableHandle` installed (the default self-skip), the sink
    /// silently drops the row instead of panicking — mirroring
    /// `DurableHandle::send_journal`'s own dropped-write contract for a
    /// `None` `writer_pool` at the other end of the channel.
    #[::core::prelude::v1::test]
    fn journal_sink_live_drops_the_row_with_no_handle_installed() {
        assert!(super::journal_durable_handle().is_none());

        let sink = super::journal_sink_live();
        let row = super::JournalRow {
            id: Uuid::new_v4(),
            campaign_id: "campaign-task4-no-handle".to_string(),
            run_id: Uuid::new_v4(),
            step: "debrief".to_string(),
            kind: engine_contract::JournalDecisionKind::RecallConsulted,
            reason: "no handle installed".to_string(),
            detail: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };

        // Must not panic.
        sink(row);
    }

    /// `RecallConsulted` renders `DONE` (an observation, not an open item
    /// or a hold) and its title contains the substring `recall` — the exact
    /// string task 6's un-gateable `bastion journal ... | grep -q 'recall'`
    /// DoD line keys on.
    #[::core::prelude::v1::test]
    fn recall_consulted_renders_done_and_title_contains_recall() {
        assert_eq!(
            super::ledger_label(engine_contract::JournalDecisionKind::RecallConsulted),
            "DONE"
        );
        assert!(
            super::kind_title(engine_contract::JournalDecisionKind::RecallConsulted)
                .contains("recall")
        );
    }

    /// `DebriefRendered` renders `DONE` (the brief IS the artifact, not an
    /// open item or a hold) and its title contains the substring `debrief`.
    #[::core::prelude::v1::test]
    fn debrief_rendered_renders_done_and_title_contains_debrief() {
        assert_eq!(
            super::ledger_label(engine_contract::JournalDecisionKind::DebriefRendered),
            "DONE"
        );
        assert!(
            super::kind_title(engine_contract::JournalDecisionKind::DebriefRendered)
                .contains("debrief")
        );
    }

    /// No `X-API-Key` header -> 401, matching every other campaign/run route.
    #[actix_web::test]
    async fn get_campaign_journal_without_api_key_is_rejected() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/campaigns/{}/journal", Uuid::new_v4()))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 401);
    }

    /// A malformed (non-UUID) campaign id segment -> 404, never a 500.
    #[actix_web::test]
    async fn get_campaign_journal_malformed_id_returns_404_not_500() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/campaigns/not-a-uuid/journal")
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }

    /// With no `DATABASE_URL` (`test_app_state()` always builds `durable`
    /// with `spawn_durable_writer(None)`), any well-formed campaign id
    /// self-skips to a clean 404 rather than a 500 — including a
    /// repo-less campaign, since this route never consults repo/roadmap
    /// identity at all, only `campaign_id`.
    #[actix_web::test]
    async fn get_campaign_journal_self_skips_to_404_with_no_pool_configured() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri(&format!("/campaigns/{}/journal", Uuid::new_v4()))
            .insert_header(("X-API-Key", "test-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), 404);
    }
}
