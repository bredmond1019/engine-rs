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

// ─── tests ──────────────────────────────────────────────────────────────
//
// Every test drives the app through `App::new().configure(crate::http::configure)`
// and `actix_web::test`, never by calling a handler function directly — a
// handler-level test would pass even if the routes were never registered,
// which is exactly the gate-blindness shape carryover
// `gate-scope-must-be-shown-capable-of-failing` describes. Deliberately NOT
// a new `crates/engine-serve/tests/*.rs` file (CLAUDE.md standing rule 8):
// engine-serve already carries six such binaries and this ticket must not
// add a seventh.
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use actix_web::{test, web, App};
    use chrono::{DateTime, TimeZone, Utc};
    use engine_core::operator::ledger::{
        ApprovalLedger, ApprovalLedgerRow, FileApprovalLedger, LedgerDecision,
    };
    use tempfile::TempDir;

    use crate::http::{configure, AppState};

    const API_KEY: &str = "approvals-test-key";

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn row(
        item_id: &str,
        decision: LedgerDecision,
        delivered_secs: i64,
        decided_secs: i64,
    ) -> ApprovalLedgerRow {
        ApprovalLedgerRow {
            item_id: item_id.to_string(),
            digest: "digest-a".to_string(),
            decision,
            who: "operator-a".to_string(),
            delivered_at: ts(delivered_secs),
            decided_at: ts(decided_secs),
            rendered_diff: "rendered summary".to_string(),
        }
    }

    /// A minimal, hermetic `AppState` — a `Dispatcher` with no registered
    /// workflows, in-memory live state, an unbound durable writer (no
    /// Postgres pool), and an empty run registry. Nothing under test here
    /// touches any of those; only `api_key` and the ledger seam matter.
    fn test_app_state() -> AppState {
        AppState {
            dispatcher: Arc::new(crate::dispatch::Dispatcher::new()),
            live: crate::live_state::LiveStateStore::new(),
            durable: crate::durable::spawn_durable_writer(None),
            runs: crate::abort::RunRegistry::new(),
            api_key: API_KEY.to_string(),
        }
    }

    /// A `FileApprovalLedger` rooted in a fresh tempdir, pre-populated with
    /// `rows` via `append` (never by writing the file directly, so the
    /// real JSONL round-trip is exercised). Returns the `TempDir` too, so
    /// the caller keeps it alive for the duration of the test.
    fn ledger_with(rows: Vec<ApprovalLedgerRow>) -> (TempDir, FileApprovalLedger) {
        let dir = TempDir::new().expect("tempdir");
        let ledger = FileApprovalLedger::new(dir.path().join("ledger.jsonl"));
        for row in rows {
            ledger.append(row);
        }
        (dir, ledger)
    }

    fn ledger_data(ledger: FileApprovalLedger) -> web::Data<Arc<dyn ApprovalLedger>> {
        web::Data::new(Arc::new(ledger) as Arc<dyn ApprovalLedger>)
    }

    // ── GET /approvals/ledger ──────────────────────────────────────────

    #[actix_web::test]
    async fn list_ledger_returns_rows_newest_first() {
        let (_dir, ledger) = ledger_with(vec![
            row("item-a", LedgerDecision::Approved, 0, 10),
            row("item-b", LedgerDecision::Approved, 100, 110),
            row("item-c", LedgerDecision::Approved, 200, 210),
        ]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = test::read_body_json(resp).await;
        let item_ids: Vec<&str> = body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["item_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            item_ids,
            vec!["item-c", "item-b", "item-a"],
            "expected the exact reverse of insertion order"
        );
    }

    #[actix_web::test]
    async fn list_ledger_total_counts_before_paging_and_offset_past_end_is_empty() {
        let (_dir, ledger) = ledger_with(vec![
            row("item-a", LedgerDecision::Approved, 0, 10),
            row("item-b", LedgerDecision::Approved, 100, 110),
            row("item-c", LedgerDecision::Approved, 200, 210),
        ]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger?limit=1")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["total"], 3);
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);

        let req = test::TestRequest::get()
            .uri("/approvals/ledger?offset=100")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["total"], 3);
        assert!(body["rows"].as_array().unwrap().is_empty());
    }

    #[actix_web::test]
    async fn list_ledger_item_id_filter_returns_only_that_items_rows() {
        let (_dir, ledger) = ledger_with(vec![
            row("item-a", LedgerDecision::Approved, 0, 10),
            row("item-a", LedgerDecision::Requeued, 20, 30),
            row("item-b", LedgerDecision::Approved, 100, 110),
        ]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger?item_id=item-a")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["total"], 2);
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r["item_id"] == "item-a"));
    }

    #[actix_web::test]
    async fn list_ledger_limit_defaults_to_100_and_is_clamped_to_1000() {
        let (_dir, ledger) = ledger_with(vec![row("item-a", LedgerDecision::Approved, 0, 10)]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["limit"], 100);

        let req = test::TestRequest::get()
            .uri("/approvals/ledger?limit=5000")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(
            body["limit"], 1000,
            "a request over the clamp is served the clamp, not rejected"
        );
    }

    #[actix_web::test]
    async fn list_ledger_missing_ledger_file_is_empty_not_404() {
        let dir = TempDir::new().expect("tempdir");
        // Never appended to -- the file never gets created.
        let ledger = FileApprovalLedger::new(dir.path().join("does-not-exist.jsonl"));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["rows"].as_array().unwrap().is_empty());
        assert_eq!(body["total"], 0);
    }

    // ── GET /approvals/ledger/stats ────────────────────────────────────

    #[actix_web::test]
    async fn stats_excludes_requeued_from_time_to_approval_but_includes_it_in_decisions_per_day() {
        let (_dir, ledger) = ledger_with(vec![
            row("item-a", LedgerDecision::Approved, 0, 10),
            row("item-b", LedgerDecision::Requeued, 0, 5),
        ]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger/stats")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["time_to_approval"]["count"], 1);
        assert_eq!(body["time_to_approval"]["median_seconds"], 10);
        assert_eq!(body["time_to_approval"]["max_seconds"], 10);

        let per_day = body["decisions_per_day"].as_object().unwrap();
        let total: u64 = per_day.values().map(|v| v.as_u64().unwrap()).sum();
        assert_eq!(total, 2, "decisions_per_day includes the Requeued row");
    }

    #[actix_web::test]
    async fn stats_over_zero_rows_has_null_median_and_max() {
        let (_dir, ledger) = ledger_with(vec![]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger/stats")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["time_to_approval"]["count"], 0);
        assert!(body["time_to_approval"]["median_seconds"].is_null());
        assert!(body["time_to_approval"]["max_seconds"].is_null());
    }

    // ── unwired / unauthenticated ──────────────────────────────────────

    #[actix_web::test]
    async fn both_routes_503_when_no_ledger_registered() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 503);

        let req = test::TestRequest::get()
            .uri("/approvals/ledger/stats")
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 503);
    }

    #[actix_web::test]
    async fn both_routes_401_on_absent_or_wrong_api_key() {
        let (_dir, ledger) = ledger_with(vec![row("item-a", LedgerDecision::Approved, 0, 10)]);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(test_app_state()))
                .app_data(ledger_data(ledger))
                .configure(configure),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "absent key on /approvals/ledger");

        let req = test::TestRequest::get()
            .uri("/approvals/ledger")
            .insert_header(("X-API-Key", "wrong-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "wrong key on /approvals/ledger");

        let req = test::TestRequest::get()
            .uri("/approvals/ledger/stats")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "absent key on /approvals/ledger/stats");

        let req = test::TestRequest::get()
            .uri("/approvals/ledger/stats")
            .insert_header(("X-API-Key", "wrong-key"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 401, "wrong key on /approvals/ledger/stats");
    }
}
