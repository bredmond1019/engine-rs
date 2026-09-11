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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use async_trait::async_trait;
use claude_code_rs::Config;
use engine_contract::{JournalDecisionKind, JournalRow, TaskContext};
use engine_core::policy::profiles::read_harness_policy_defaults;
use engine_core::policy::resolve::{resolve, Policy};
use engine_core::policy::tier::{model_tier_to_model_string, ModelTier};
use engine_core::repo_registry::RepoRegistry;
use engine_core::workflows::orchestration::chain::ChainStep;
use engine_core::workflows::orchestration::debrief::JournalReader;
use engine_core::workflows::orchestration::execute::{EngineKind, ExecutionOutcome, FlowRunner};
use engine_core::workflows::orchestration::gates::{AdmissionGate, DependencyEdge};
use engine_core::workflows::orchestration::integrate::{
    integrate_chain_with_run_record, CloseBlockFn, ComposeLedgerEntriesFn, HoldSource,
    IntegrateError, JournalSinkFn, RunRecordLifecycle, RunRecordSinkFn, StepObserverFn,
};
use engine_core::workflows::orchestration::ledger::{
    Coverage, CrossRepo, CrossRepoE2e, LedgerStatus, NewLedgerEntry,
};
use engine_core::{AgentCodeStep, Node};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
// `EN.15.G` task 3 — the FIRST production caller of `render_notes_md` /
// `render_review_md`.
//
// Before this, both renderers had seven test call sites
// (`crates/engine-serve/tests/journal_integration.rs`) and zero production
// ones — exactly the `call_site: NONE` shape: tested, green, and
// unreachable from a real run. `drive_chain_with_run_record` below is a
// real (non-test) caller: it drives an actual chain through
// `integrate_chain_with_run_record` and, via the `RunRecordSinkFn` it
// wires, rewrites `notes.md`/`review.md` at the chain's `Started` and
// `Terminal` lifecycle transitions.
// ---------------------------------------------------------------------------

/// In-process accumulator of the [`JournalRow`]s a driven chain emits, shared between the
/// `journal_sink` closure that fills it (called once per decision point,
/// `integrate::emit_journal`) and the `run_record_sink` closure that reads it back to render
/// `notes.md`/`review.md`'s "running tab" — both are plain, synchronous `Send + Sync`
/// closures the `engine-core` loop calls directly, never handed an async `JournalReader` the
/// way the durable Postgres path is (see this module's `LiveJournalReader`); a local `Vec`
/// avoids needing to `.await` inside a sync callback.
type SharedRows = Arc<Mutex<Vec<JournalRow>>>;

fn recording_journal_sink(rows: SharedRows) -> Arc<JournalSinkFn> {
    Arc::new(move |row: JournalRow| {
        if let Ok(mut guard) = rows.lock() {
            guard.push(row);
        }
    })
}

/// The production [`RunRecordSinkFn`]: on every lifecycle transition, re-renders
/// `notes.md`/`review.md` from whatever rows `recording_journal_sink` has accumulated so far
/// and overwrites both files in `roadmap_dir`. `RunRecordLifecycle::Started` writes with
/// `meta.run_ended: None` (`lifecycle: active`); `RunRecordLifecycle::Terminal` writes with
/// `run_ended: Some(<now>)` (`lifecycle: lane-complete`) — see [`RunRecordMeta::lifecycle`].
/// A process killed between the two calls therefore leaves the `Started` write on disk,
/// `lifecycle: active` forever, which is exactly the shape `mev lanes` needs to report a dead
/// run `degraded` rather than `live`. Both writes are best-effort (`tracing::warn!` on a
/// filesystem error) — a rendering failure must never mask the chain's own real outcome, which
/// this sink has no way to influence anyway (it returns nothing `integrate_chain_impl`'s
/// control flow depends on).
fn run_record_sink(
    roadmap_dir: PathBuf,
    campaign_id: Uuid,
    meta: RunRecordMeta,
    rows: SharedRows,
) -> Arc<RunRecordSinkFn> {
    Arc::new(move |lifecycle| {
        let mut meta = meta.clone();
        if lifecycle == RunRecordLifecycle::Terminal {
            meta.run_ended = Some(chrono::Utc::now().to_rfc3339());
        }
        let snapshot: Vec<JournalRow> = rows.lock().map(|guard| guard.clone()).unwrap_or_default();
        let notes = render_notes_md(&campaign_id, &snapshot, &meta);
        let review = render_review_md(&campaign_id, &snapshot, &meta);
        if let Err(err) = std::fs::write(roadmap_dir.join("notes.md"), notes) {
            tracing::warn!(
                error = %err,
                path = %roadmap_dir.join("notes.md").display(),
                "EN.15.G: failed to write notes.md"
            );
        }
        if let Err(err) = std::fs::write(roadmap_dir.join("review.md"), review) {
            tracing::warn!(
                error = %err,
                path = %roadmap_dir.join("review.md").display(),
                "EN.15.G: failed to write review.md"
            );
        }
    })
}

// ---------------------------------------------------------------------------
// `EN.15.L` task 3 — the production `ComposeLedgerEntriesFn` caller.
//
// `compose_ledger_entries_via_agent` (wired into `drive_chain_with_run_record` below,
// alongside `run_record_sink`) is the first PRODUCTION caller of the seam `EN.15.L` task 2
// threaded through `integrate_chain_impl` — before this, `ComposeLedgerEntriesFn` had test
// call sites only. It runs one `AgentCodeStep` per just-integrated block, asking it to
// propose candidate D57 verification-ledger entries from that block's `ExecutionOutcome`;
// every deterministic rule (the `<repo>-` id stamp, forcing `status: untested`, the
// `coverage`/`call_site` validation refusals, the merge-append write) stays in
// `engine_core::workflows::orchestration::ledger` (task 1) and
// `compose_and_append_ledger_entries` (task 2) — this function's only job is turning the
// model's fenced-JSON reply into `Vec<NewLedgerEntry>`, unstamped and unvalidated.
// ---------------------------------------------------------------------------

/// The stable system-prompt prefix for the ledger composer (standing rule 7 / D24: a node's
/// stable prompt is a file, `include_str!`-ed, never an inline literal — kept run-invariant so
/// `apply_prompt_cache`'s breakpoint holds across every block this composer runs against).
const COMPOSE_LEDGER_ENTRIES_PROMPT: &str =
    include_str!("../../engine-core/src/workflows/orchestration/prompts/compose_ledger_entries.md");

/// `Node::name()` identity the composer's `AgentCodeStep` runs under, and the `ctx.nodes` key
/// its reply is read back from.
const LEDGER_COMPOSER_NODE_NAME: &str = "VerificationLedgerComposer";

/// `harness.json`'s existing `orchestration.policy` / `orchestration.profiles` sections (the
/// same ones `OrchestrationPolicy` reads — `crates/engine-core/src/workflows/orchestration/graph.rs`)
/// are the workflow-keyed lookup this reuses, per CLAUDE.md standing rule 6 ("nodes are
/// configurable, not hardcoded"): no new harness.json section for one extra knob. This type
/// declares only the one field it owns (`ledger_composer_model_tier`) — serde ignores every
/// other sibling field already living in that JSON object on the way through, so this can be
/// added without touching `OrchestrationPolicy`'s own struct at all. `EN.15.L` task 4 documents
/// the field in `planning/harness.json` alongside the existing no-op defaults.
const ORCHESTRATION_HARNESS_KEY: &str = "orchestration";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct LedgerComposerPolicy {
    ledger_composer_model_tier: ModelTier,
}

impl Default for LedgerComposerPolicy {
    fn default() -> Self {
        Self {
            ledger_composer_model_tier: ModelTier::Sonnet,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct PartialLedgerComposerPolicy {
    ledger_composer_model_tier: Option<ModelTier>,
}

impl Policy for LedgerComposerPolicy {
    type Partial = PartialLedgerComposerPolicy;

    fn apply(self, over: &Self::Partial) -> Self {
        Self {
            ledger_composer_model_tier: over
                .ledger_composer_model_tier
                .unwrap_or(self.ledger_composer_model_tier),
        }
    }
}

/// Resolve the composer's model tier: `harness.json`'s `orchestration.policy` (read from the
/// just-integrated block's own repo checkout, `repo_path`) over the built-in `Sonnet` default.
/// Only two of the four policy layers apply here — there is no per-run `profile`/`event`
/// override reachable from a bare [`ExecutionOutcome`], so this deliberately resolves the
/// same two layers [`crate::workflows::content_pipeline`] and friends fall back to when no
/// profile was selected, rather than silently inventing a profile identity.
fn resolve_ledger_composer_model_tier(repo_path: &Path) -> ModelTier {
    let harness_defaults = read_harness_policy_defaults::<PartialLedgerComposerPolicy>(
        repo_path,
        ORCHESTRATION_HARNESS_KEY,
    )
    .ok()
    .flatten();
    resolve(
        LedgerComposerPolicy::default(),
        harness_defaults.as_ref(),
        None,
        None,
    )
    .ledger_composer_model_tier
}

fn ledger_status_from_wire(value: &str) -> LedgerStatus {
    match value {
        "tested" => LedgerStatus::Tested,
        "partial" => LedgerStatus::Partial,
        "failed" => LedgerStatus::Failed,
        "blocked" => LedgerStatus::Blocked,
        "not_applicable" => LedgerStatus::NotApplicable,
        _ => LedgerStatus::Untested,
    }
}

fn ledger_coverage_from_wire(value: &str) -> Coverage {
    match value {
        "covered" => Coverage::Covered,
        "partial" => Coverage::Partial,
        _ => Coverage::Uncovered,
    }
}

fn cross_repo_e2e_from_wire(value: &str) -> CrossRepoE2e {
    match value {
        "exists" => CrossRepoE2e::Exists,
        "needed" => CrossRepoE2e::Needed,
        _ => CrossRepoE2e::NotApplicable,
    }
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Map one raw JSON candidate object (whatever shape the model actually returned) into a
/// [`NewLedgerEntry`] — unstamped, unvalidated. A missing field becomes an empty
/// string/default, never an error: [`super::ledger`]'s own validation (task 1) is what refuses
/// an unusable candidate, and it must see the empty value to do so (e.g. a missing `call_site`
/// must arrive as `""`, not be silently defaulted to `"NONE"` here, or a real omission would be
/// indistinguishable from the model's own considered `"NONE"` answer).
fn candidate_from_json(value: &Value) -> Result<NewLedgerEntry, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "composer candidate entry was not a JSON object".to_string())?;
    let get_str = |key: &str| -> String {
        obj.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let cross_repo = obj
        .get("cross_repo")
        .and_then(Value::as_object)
        .map(|cr| CrossRepo {
            dependent: cr
                .get("dependent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            repos: cr.get("repos").map(string_array).unwrap_or_default(),
            e2e: cr
                .get("e2e")
                .and_then(Value::as_str)
                .map(cross_repo_e2e_from_wire)
                .unwrap_or_default(),
            note: cr
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });

    Ok(NewLedgerEntry {
        id: get_str("id"),
        capability: get_str("capability"),
        status: obj
            .get("status")
            .and_then(Value::as_str)
            .map(ledger_status_from_wire)
            .unwrap_or_default(),
        env: get_str("env"),
        how_to_verify: get_str("how_to_verify"),
        call_site: get_str("call_site"),
        evidence: get_str("evidence"),
        coverage: obj
            .get("coverage")
            .and_then(Value::as_str)
            .map(ledger_coverage_from_wire)
            .unwrap_or_default(),
        covered_by: obj.get("covered_by").map(string_array).unwrap_or_default(),
        cross_repo,
        // The composer never gets to attach a remediation directly (task 1's `compose` always
        // stamps `status: untested`, and `remediation` is only valid on `failed`/`blocked`) —
        // see this file's `compose_ledger_entries_via_agent` doc.
        remediation: None,
    })
}

/// Extract a JSON array from the model's raw reply: a fenced ` ```json ... ``` ` block if
/// present, else the first `[` .. last `]` span. Returns `None` when neither shape is found —
/// the caller turns that into `Err`, which `compose_and_append_ledger_entries` (task 2) treats
/// exactly like an empty `Ok(vec![])` plus a logged, recorded gap.
fn extract_json_array(content: &str) -> Option<&str> {
    if let Some(fence_start) = content.find("```") {
        let after = &content[fence_start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(fence_end) = after.find("```") {
            return Some(after[..fence_end].trim());
        }
    }
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end < start {
        return None;
    }
    Some(&content[start..=end])
}

fn parse_candidate_entries(content: &str) -> Result<Vec<NewLedgerEntry>, String> {
    let json_text = extract_json_array(content)
        .ok_or_else(|| "composer output contained no JSON array".to_string())?;
    let value: Value = serde_json::from_str(json_text)
        .map_err(|err| format!("composer output was not valid JSON: {err}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "composer output JSON was not an array".to_string())?;
    items.iter().map(candidate_from_json).collect()
}

/// The dynamic, per-run body appended after [`COMPOSE_LEDGER_ENTRIES_PROMPT`]'s stable prefix —
/// which block just integrated and the full `ctx.nodes` map of its child run, the composer's
/// only source of truth for `capability`/`evidence`/`call_site`.
fn build_compose_prompt(
    repo: &str,
    block_id: &str,
    engine: EngineKind,
    ctx_nodes: &Value,
) -> String {
    format!(
        "{COMPOSE_LEDGER_ENTRIES_PROMPT}\n\n## This block\n\nrepo: {repo}\nblock_id: {block_id}\nengine: {engine}\n\n\
         ## Child run context (`ctx.nodes`, JSON)\n\n```json\n{}\n```\n",
        serde_json::to_string_pretty(ctx_nodes).unwrap_or_else(|_| "{}".to_string()),
    )
}

/// The production [`ComposeLedgerEntriesFn`]: one `AgentCodeStep` call per just-integrated
/// step, asking it to propose D57 verification-ledger candidates from that step's
/// [`ExecutionOutcome`]. Wired into [`drive_chain_with_run_record`] below (this repo's own
/// production caller, per this block's `why` — no test call site substitutes for it). A model
/// or parse failure returns `Err(String)`, which `compose_and_append_ledger_entries` (task 2,
/// `engine_core::workflows::orchestration::integrate`) already treats as a non-fatal gap: it
/// logs via `tracing::warn!` and records it as a journal row, which `render_notes_md`
/// (`EN.15.G` task 3, this same file) renders into the run's `notes.md` as an `**OPEN**`
/// finding — never a failed chain or a blocked `close_block`.
fn compose_ledger_entries_via_agent(
    outcome: &ExecutionOutcome,
) -> BoxFuture<'static, Result<Vec<NewLedgerEntry>, String>> {
    let repo = outcome.repo.clone();
    let block_id = outcome.block_id.clone();
    let repo_path = outcome.repo_path.clone();
    let engine = outcome.engine;
    let ctx_nodes = serde_json::to_value(&outcome.ctx.nodes).unwrap_or(Value::Null);

    Box::pin(async move {
        let model_tier = resolve_ledger_composer_model_tier(&repo_path);
        // `Local` has no meaning for this composer (no OpenAI-compatible transport is wired
        // here) — `model_tier_to_model_string`'s `local_model` fallback is a placeholder that
        // is never actually reached because `harness.json`'s built-in default is `Sonnet` and
        // nothing sets this knob to `local` today; documented rather than silently supported.
        let model = model_tier_to_model_string(model_tier, "unset-local-model");
        let config = Config {
            model: Some(model),
            ..Config::default()
        };
        let prompt = build_compose_prompt(&repo, &block_id, engine, &ctx_nodes);
        let step = AgentCodeStep::new(LEDGER_COMPOSER_NODE_NAME, config, prompt);

        let ctx = TaskContext {
            event: Value::Object(serde_json::Map::new()),
            nodes: HashMap::new(),
            metadata: Value::Object(serde_json::Map::new()),
            node_runs: HashMap::new(),
        };
        let ctx = step
            .process(ctx)
            .await
            .map_err(|err| format!("verification-ledger composer call failed: {err}"))?;

        let content = ctx
            .nodes
            .get(LEDGER_COMPOSER_NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        parse_candidate_entries(content)
    })
}

/// Drive a real chain end to end through
/// [`integrate_chain_with_run_record`](engine_core::workflows::orchestration::integrate::integrate_chain_with_run_record)
/// while maintaining its D57 run record (`notes.md`/`review.md`) in `roadmap_dir` — the first
/// production caller of [`render_notes_md`]/[`render_review_md`] (see this section's module
/// doc). Every argument besides `meta` mirrors `integrate_chain_with_run_record`'s own; `meta`
/// supplies the `(repo, roadmap, lane, run_started)` identity a D57 record is addressed by
/// (`RunRecordMeta`'s own doc) — its `run_ended` field is ignored (overwritten at each
/// lifecycle transition) and may be left `None`.
#[allow(clippy::too_many_arguments)]
pub async fn drive_chain_with_run_record(
    chain: &[ChainStep],
    resolve_depends_on: &dyn Fn(&str, &str) -> Vec<DependencyEdge>,
    is_edge_met: &dyn Fn(&str, &str) -> bool,
    admission: &AdmissionGate,
    hold_source: &dyn HoldSource,
    poll_interval: Duration,
    hold_deadline: Option<Duration>,
    resolve_engine: &dyn Fn(&str, &str) -> EngineKind,
    registry: &RepoRegistry,
    run_flow: &FlowRunner,
    roadmap_dir: &Path,
    lane: Option<&str>,
    step_observer: &StepObserverFn,
    default_use_worktree: bool,
    default_auto_pr: bool,
    campaign_id: Uuid,
    close_block: &CloseBlockFn,
    meta: RunRecordMeta,
) -> Result<Vec<ExecutionOutcome>, IntegrateError> {
    let rows: SharedRows = Arc::new(Mutex::new(Vec::new()));
    let journal_sink = recording_journal_sink(rows.clone());
    let record_sink = run_record_sink(roadmap_dir.to_path_buf(), campaign_id, meta, rows);

    integrate_chain_with_run_record(
        chain,
        resolve_depends_on,
        is_edge_met,
        admission,
        hold_source,
        poll_interval,
        hold_deadline,
        None,
        None,
        resolve_engine,
        registry,
        run_flow,
        roadmap_dir,
        lane,
        step_observer,
        default_use_worktree,
        default_auto_pr,
        campaign_id,
        close_block,
        Some(journal_sink.as_ref()),
        None,
        Some(record_sink.as_ref()),
        Some(&compose_ledger_entries_via_agent as &ComposeLedgerEntriesFn),
    )
    .await
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

/// `EN.15.G` task 3: proves `drive_chain_with_run_record` — the production caller added
/// above — actually calls `render_notes_md`/`render_review_md` by driving a REAL chain
/// through `integrate_chain_with_run_record`, never by calling either renderer directly.
#[cfg(test)]
mod run_record_lifecycle_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use engine_core::repo_registry::RepoRegistry;
    use engine_core::workflows::orchestration::execute::{EngineKind, FlowRunner};
    use engine_core::workflows::orchestration::gates::AdmissionGate;
    use engine_core::workflows::orchestration::integrate::NeverHeld;
    use uuid::Uuid;

    use super::{drive_chain_with_run_record, ChainStep, RunRecordMeta};

    fn step(repo: &str, block_id: &str) -> ChainStep {
        ChainStep {
            repo: repo.to_string(),
            block_id: block_id.to_string(),
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

    fn write_done_state(repo_path: &std::path::Path, block_id: &str) {
        let dir = repo_path.join("planning").join(block_id).join("sdlc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sdlc-flow-state.json"),
            serde_json::json!({"status": "done"}).to_string(),
        )
        .unwrap();
    }

    fn test_meta() -> RunRecordMeta {
        RunRecordMeta {
            repo: "repo-a".to_string(),
            roadmap: "roadmap-a".to_string(),
            lane: "repo-a".to_string(),
            run_started: "2026-09-08T00:00:00+00:00".to_string(),
            run_ended: None,
        }
    }

    /// Acceptance criterion: a chain run to a clean terminal node CLEARS `lifecycle:`,
    /// asserted by re-reading the record after the run — driven for real through
    /// `drive_chain_with_run_record`, never by calling `render_notes_md` directly.
    #[tokio::test]
    async fn clean_terminal_clears_lifecycle_in_the_written_run_record() {
        let (dir, registry) = one_repo_registry();
        write_done_state(&dir.path().join("repo-a"), "A.1");

        let runner: FlowRunner = Arc::new(move |_invocation| {
            Box::pin(async {
                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let hold = NeverHeld;
        let roadmap_dir = tempfile::tempdir().unwrap();
        let campaign_id = Uuid::new_v4();
        let chain = vec![step("repo-a", "A.1")];

        let outcomes = drive_chain_with_run_record(
            &chain,
            &resolve_deps,
            &is_met,
            &admission,
            &hold,
            Duration::from_millis(1),
            None,
            &resolve_engine,
            &registry,
            &runner,
            roadmap_dir.path(),
            None,
            &|_| {},
            false,
            true,
            campaign_id,
            &|_repo: &str, _id: &str| {},
            test_meta(),
        )
        .await
        .expect("clean chain should integrate and return outcomes");
        assert_eq!(outcomes.len(), 1);

        let notes = std::fs::read_to_string(roadmap_dir.path().join("notes.md"))
            .expect("notes.md should have been written");
        assert!(
            notes.contains("lifecycle: lane-complete"),
            "expected a clean terminal run to clear lifecycle, got:\n{notes}"
        );
        let review = std::fs::read_to_string(roadmap_dir.path().join("review.md"))
            .expect("review.md should have been written");
        assert!(
            review.contains("lifecycle: lane-complete"),
            "expected a clean terminal run to clear lifecycle, got:\n{review}"
        );
    }

    /// Acceptance criterion: a chain killed mid-run leaves `lifecycle: active`. Simulated by
    /// aborting the driving task while a step is still in flight — unlike a bail or a
    /// cancellation (both of which still return from `integrate_chain_impl` and fire
    /// `Terminal`), an abort truly stops the future without running any more of this
    /// process's code, the same as a `kill -9` would.
    #[tokio::test]
    async fn killed_mid_run_leaves_lifecycle_active_in_the_written_run_record() {
        let (_dir, registry) = one_repo_registry();

        // Hangs forever — the abort below fires while this step is still "running", well
        // after `RunRecordLifecycle::Started` already wrote `lifecycle: active` to disk.
        let runner: FlowRunner = Arc::new(move |_invocation| {
            Box::pin(async {
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok(engine_contract::TaskContext {
                    event: serde_json::json!({}),
                    nodes: std::collections::HashMap::new(),
                    metadata: serde_json::json!({}),
                    node_runs: std::collections::HashMap::new(),
                })
            })
        });
        let resolve_engine = |_repo: &str, _id: &str| EngineKind::Flow;
        let resolve_deps = |_repo: &str, _id: &str| Vec::new();
        let is_met = |_repo: &str, _id: &str| true;
        let admission = AdmissionGate::with_default_policy();
        let hold = NeverHeld;
        let roadmap_dir = tempfile::tempdir().unwrap();
        let roadmap_path = roadmap_dir.path().to_path_buf();
        let campaign_id = Uuid::new_v4();
        let chain = vec![step("repo-a", "A.1")];
        let meta = test_meta();

        // `execute::FlowFuture` is deliberately not `Send` (see `graph.rs`'s own doc on that
        // exact point), so the driving future can't cross `tokio::spawn`'s `Send` bound —
        // `spawn_local` on a `LocalSet` is the abort-capable equivalent that doesn't need one.
        let local = tokio::task::LocalSet::new();
        let handle = local.spawn_local(async move {
            let _ = drive_chain_with_run_record(
                &chain,
                &resolve_deps,
                &is_met,
                &admission,
                &hold,
                Duration::from_millis(1),
                None,
                &resolve_engine,
                &registry,
                &runner,
                &roadmap_path,
                None,
                &|_| {},
                false,
                true,
                campaign_id,
                &|_repo: &str, _id: &str| {},
                meta,
            )
            .await;
        });

        local
            .run_until(async {
                // Give `Started` a chance to write before the abort — the write itself is
                // synchronous, but the spawned task needs a scheduler tick to reach it.
                tokio::time::sleep(Duration::from_millis(50)).await;
                handle.abort();
                let _ = handle.await;
            })
            .await;

        let notes = std::fs::read_to_string(roadmap_dir.path().join("notes.md"))
            .expect("Started should already have written notes.md");
        assert!(
            notes.contains("lifecycle: active"),
            "expected a killed-mid-run record to stay active, got:\n{notes}"
        );
        assert!(
            !notes.contains("lifecycle: lane-complete"),
            "Terminal must never have fired for an aborted run, got:\n{notes}"
        );
    }
}
