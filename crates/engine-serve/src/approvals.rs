//! Engine-side HTTP read surface over the approval ledger
//! (`EN.ticket.approval-ledger-read-endpoint`).
//!
//! `EN.8.C` shipped the ledger as an append-only JSONL file behind the
//! [`ApprovalLedger`] trait, with no HTTP surface at all. This module adds
//! two authenticated `GET` routes over an **injectable, optional** ledger
//! seam so a cockpit on another host can render decision rows and
//! time-to-approval stats it cannot read off the engine host's filesystem.
//!
//! **The seam is `Option<web::Data<Arc<dyn ApprovalLedger>>>`, deliberately
//! not a required [`crate::http::AppState`] field.** `AppState`'s fields
//! are public and it is struct-literal-constructed in `bastion`
//! (`../bastion/src/serve/mod.rs`) and in five `crates/engine-serve/tests/*.rs`
//! files, so adding a required `ledger` field would be a cross-repo
//! breaking change for a surface bastion is not yet ready to wire. An
//! `Option<web::Data<..>>` extractor is additive: bastion compiles
//! untouched, and these routes exist and answer 503 until one
//! `.app_data(...)` line lands there. See `planning/decisions/D15-additive-seams-over-appstate-fields.md`.
//!
//! Route registration lives in `crate::http::configure` (a later task in
//! this ticket) — this module only defines the seam, the response DTOs,
//! and the two handlers.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_core::operator::ledger::{
    decisions_per_day, time_to_approval_stats, ApprovalLedger, ApprovalLedgerRow,
};
use serde::{Deserialize, Serialize};

use crate::http::{check_api_key, AppState};

/// The ledger seam both handlers extract. `None` when the embedding host
/// has not registered a ledger — see this module's docs for why that is
/// additive rather than a required `AppState` field.
type LedgerData = web::Data<Arc<dyn ApprovalLedger>>;

/// Default `limit` for `GET /approvals/ledger` when the query param is
/// omitted.
const DEFAULT_LIMIT: usize = 100;

/// Maximum `limit` for `GET /approvals/ledger`; a larger request is served
/// this clamp rather than rejected.
const MAX_LIMIT: usize = 1000;

/// Stable JSON error body returned by both routes when no ledger is
/// registered. Kept identical between the two routes.
fn ledger_not_configured() -> HttpResponse {
    HttpResponse::ServiceUnavailable()
        .json(serde_json::json!({ "error": "approval ledger not configured" }))
}

/// Query parameters for `GET /approvals/ledger`.
#[derive(Debug, Deserialize)]
pub struct ListLedgerQuery {
    pub item_id: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Response body for `GET /approvals/ledger`.
#[derive(Debug, Serialize)]
struct ListLedgerResponse {
    rows: Vec<ApprovalLedgerRow>,
    total: usize,
    limit: usize,
    offset: usize,
}

/// `GET /approvals/ledger` — 401 without a valid `X-API-Key`; 503 with a
/// stable JSON body when no ledger is registered; otherwise 200 with rows
/// **newest-first** (the store returns oldest-first; the reversal is this
/// endpoint's).
///
/// `total` counts the rows matching the `item_id` filter *before*
/// `limit`/`offset` are applied. `limit` defaults to [`DEFAULT_LIMIT`] and
/// is clamped to [`MAX_LIMIT`] rather than rejected. An `offset` past the
/// end of the (post-filter) row set yields `200` with an empty `rows`
/// array.
pub async fn list_ledger(
    req: HttpRequest,
    query: web::Query<ListLedgerQuery>,
    state: web::Data<AppState>,
    ledger: Option<LedgerData>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let Some(ledger) = ledger else {
        return ledger_not_configured();
    };

    let item_id = query.item_id.clone();
    let ledger = ledger.get_ref().clone();

    let mut rows = match web::block(move || match &item_id {
        Some(item_id) => ledger.rows_for(item_id),
        None => ledger.read_all(),
    })
    .await
    {
        Ok(rows) => rows,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "failed to read approval ledger" }));
        }
    };

    // Store order is oldest-first; this endpoint's contract is newest-first.
    rows.reverse();

    let total = rows.len();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = query.offset.unwrap_or(0);

    let page: Vec<ApprovalLedgerRow> = rows.into_iter().skip(offset).take(limit).collect();

    HttpResponse::Ok().json(ListLedgerResponse {
        rows: page,
        total,
        limit,
        offset,
    })
}

/// `{count, median_seconds, max_seconds}` — the time-to-approval half of
/// the stats response. `median_seconds`/`max_seconds` are `null` exactly
/// when `count` is 0, matching [`engine_core::operator::ledger::TimeToApprovalStats`]'s
/// `Option<Duration>` fields.
#[derive(Debug, Serialize)]
struct TimeToApprovalStatsResponse {
    count: usize,
    median_seconds: Option<i64>,
    max_seconds: Option<i64>,
}

/// Response body for `GET /approvals/ledger/stats`.
#[derive(Debug, Serialize)]
struct LedgerStatsResponse {
    time_to_approval: TimeToApprovalStatsResponse,
    decisions_per_day: std::collections::BTreeMap<String, usize>,
}

/// `GET /approvals/ledger/stats` — 401 without a valid `X-API-Key`; 503
/// with a stable JSON body when no ledger is registered; otherwise 200
/// with time-to-approval stats and a per-day decision count, delegating to
/// [`time_to_approval_stats`] and [`decisions_per_day`] without
/// re-deriving either.
///
/// Note the asymmetry those two functions already encode and that this
/// handler does not "fix": `time_to_approval_stats` excludes `Requeued`
/// rows, `decisions_per_day` includes them.
pub async fn ledger_stats(
    req: HttpRequest,
    state: web::Data<AppState>,
    ledger: Option<LedgerData>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let Some(ledger) = ledger else {
        return ledger_not_configured();
    };

    let ledger = ledger.get_ref().clone();

    let rows = match web::block(move || ledger.read_all()).await {
        Ok(rows) => rows,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "failed to read approval ledger" }));
        }
    };

    let stats = time_to_approval_stats(&rows);
    let per_day = decisions_per_day(&rows);

    HttpResponse::Ok().json(LedgerStatsResponse {
        time_to_approval: TimeToApprovalStatsResponse {
            count: stats.count,
            median_seconds: stats.median.map(|d| d.num_seconds()),
            max_seconds: stats.max.map(|d| d.num_seconds()),
        },
        decisions_per_day: per_day
            .into_iter()
            .map(|(date, count)| (date.to_string(), count))
            .collect(),
    })
}
