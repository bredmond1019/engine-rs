//! Integration coverage for the durable run journal (EN.12.D), in the shape
//! of `dispatch_integration.rs` / `abort_integration.rs`. This file starts
//! with task 6's golden-rendering coverage; task 7 adds the route-level
//! coverage (bail-through-the-route, budget-halt-through-the-route,
//! repo-less addressability, no-`DATABASE_URL` self-skip) alongside it.
//!
//! ## The golden rendering (task 6)
//!
//! `engine_serve::journal::render_notes_md` / `render_review_md` produce the
//! D57 `notes.md`/`review.md` view. Two readers OUTSIDE this repo parse that
//! output and cannot be exercised here (D64 un-gateable criterion):
//!
//! - `base-template/scripts/roadmap_status_discovery.py`'s
//!   `discover_run_records` — reads the YAML frontmatter block via
//!   `_FRONTMATTER_RE` for `lifecycle`, `run_started`, `run_ended`, and
//!   counts `**OPEN**` / `**HELD**` occurrences in the body via
//!   `_OPEN_ROW_RE` / `_HELD_ROW_RE` (both case-insensitive, bold markdown).
//! - `/consolidate-run` (`base-template/.claude/commands/consolidate-run.md`,
//!   Step 4) selects records by their `roadmap:` frontmatter field matching
//!   the roadmap slug being consolidated.
//!
//! This test is the standing fixture for that un-gateable criterion: it
//! asserts, field-by-field, that the rendered output carries exactly the
//! shape those two readers require, so a rendering change that would break
//! either of them fails in THIS repo's own suite rather than silently
//! shipping.

use chrono::Utc;
use engine_contract::{JournalDecisionKind, JournalRow};
use engine_serve::journal::{render_notes_md, render_review_md, RunRecordMeta};
use uuid::Uuid;

fn sample_row(step: &str, kind: JournalDecisionKind, reason: &str) -> JournalRow {
    JournalRow {
        id: Uuid::new_v4(),
        campaign_id: "campaign-golden".to_string(),
        run_id: Uuid::new_v4(),
        step: step.to_string(),
        kind,
        reason: reason.to_string(),
        detail: serde_json::json!({}),
        created_at: Utc::now(),
    }
}

fn sample_meta(run_ended: Option<&str>) -> RunRecordMeta {
    RunRecordMeta {
        repo: "engine-rs".to_string(),
        roadmap: "orchestration-extensions".to_string(),
        lane: "engine".to_string(),
        run_started: "2026-08-24".to_string(),
        run_ended: run_ended.map(|s| s.to_string()),
    }
}

/// Extracts the YAML frontmatter block the same way
/// `roadmap_status_discovery.py`'s `_parse_frontmatter` does: a `key: value`
/// map from the region between the two `---` fences.
fn parse_frontmatter(text: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "rendered output must open with a '---' frontmatter fence, matching _FRONTMATTER_RE"
    );
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            out.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

/// GOLDEN: `roadmap_status_discovery.py`'s `discover_run_records` requires
/// `lifecycle`, `run_started`, `run_ended` in frontmatter, and counts
/// `**OPEN**`/`**HELD**` markers in the body.
#[test]
fn render_notes_md_matches_roadmap_status_discovery_shape() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![
        sample_row("build", JournalDecisionKind::StepIntegrated, "ok"),
        sample_row("deploy", JournalDecisionKind::StepBailed, "network timeout"),
        sample_row(
            "deploy",
            JournalDecisionKind::BudgetHalted,
            "daily cap exceeded",
        ),
    ];
    let meta = sample_meta(None);

    let notes = render_notes_md(&campaign_id, &rows, &meta);

    let fm = parse_frontmatter(&notes);
    // Field-by-field against roadmap_status_discovery.py's actual reads:
    // `fm.get("lifecycle", "<absent>")`, `fm.get("run_started", "<absent>")`,
    // `fm.get("run_ended", "<absent>")`.
    assert_eq!(fm.get("lifecycle").map(String::as_str), Some("active"));
    assert_eq!(
        fm.get("run_started").map(String::as_str),
        Some("2026-08-24")
    );
    // run_ended is None on this meta -> frontmatter value is empty, which
    // the Python parser treats as "" (present key, empty value), distinct
    // from "<absent>" (key missing entirely) — both read as falsy/absent by
    // that script's liveness logic, so an empty value is the correct render.
    assert_eq!(fm.get("run_ended").map(String::as_str), Some(""));
    assert_eq!(
        fm.get("roadmap").map(String::as_str),
        Some("orchestration-extensions"),
        "/consolidate-run Step 4 selects records by this exact field"
    );

    // _OPEN_ROW_RE / _HELD_ROW_RE: case-insensitive `**OPEN**` / `**HELD**`.
    let open_count = notes.matches("**OPEN**").count();
    let held_count = notes.matches("**HELD**").count();
    assert_eq!(open_count, 1, "one StepBailed row must render as **OPEN**");
    assert_eq!(
        held_count, 1,
        "one BudgetHalted row must render as **HELD**"
    );

    // No telemetry: out_of_scope in the block record, checked here because
    // both out-of-repo readers parse this exact text.
    for forbidden in ["token", "cost_usd", "attempt_count"] {
        assert!(
            !notes.to_lowercase().contains(forbidden),
            "rendered notes.md must carry no telemetry field: {forbidden}"
        );
    }
}

/// GOLDEN: a repo-scoped run whose lane has closed renders `lifecycle:
/// lane-complete` with a populated `run_ended`, per D57 section 2.
#[test]
fn render_notes_md_lane_complete_when_run_ended_present() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![sample_row(
        "build",
        JournalDecisionKind::StepIntegrated,
        "ok",
    )];
    let meta = sample_meta(Some("2026-08-25"));

    let notes = render_notes_md(&campaign_id, &rows, &meta);
    let fm = parse_frontmatter(&notes);

    assert_eq!(
        fm.get("lifecycle").map(String::as_str),
        Some("lane-complete")
    );
    assert_eq!(fm.get("run_ended").map(String::as_str), Some("2026-08-25"));
}

/// GOLDEN: `review.md` carries the same frontmatter contract as `notes.md`
/// (both readers glob `*.md` under the roadmap dir and parse either file
/// identically) plus a block ledger table.
#[test]
fn render_review_md_matches_roadmap_status_discovery_shape() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![
        sample_row("build", JournalDecisionKind::StepIntegrated, "ok"),
        sample_row("gate", JournalDecisionKind::GateRefused, "admission denied"),
    ];
    let meta = sample_meta(Some("2026-08-25"));

    let review = render_review_md(&campaign_id, &rows, &meta);
    let fm = parse_frontmatter(&review);

    assert_eq!(
        fm.get("lifecycle").map(String::as_str),
        Some("lane-complete")
    );
    assert_eq!(
        fm.get("run_started").map(String::as_str),
        Some("2026-08-24")
    );
    assert_eq!(
        fm.get("roadmap").map(String::as_str),
        Some("orchestration-extensions")
    );

    assert_eq!(
        review.matches("**OPEN**").count(),
        1,
        "the GateRefused row must render as **OPEN**"
    );
    assert!(
        review.contains("| Step | Origin roadmap | Decision | Outcome |"),
        "review.md must carry the D57 block ledger table"
    );

    for forbidden in ["token", "cost_usd", "attempt_count"] {
        assert!(
            !review.to_lowercase().contains(forbidden),
            "rendered review.md must carry no telemetry field: {forbidden}"
        );
    }
}

/// A repo-less run has no `RunRecordMeta` at all — it is addressable purely
/// through the read route (task 5) and never goes through this renderer.
/// This is a documentation-only assertion that the renderer's inputs
/// (`RunRecordMeta`) are never optional/derived from `JournalRow` alone,
/// so a caller cannot accidentally fabricate repo/roadmap identity for a
/// repo-less campaign.
#[test]
fn renderer_requires_explicit_run_record_meta_not_derived_from_rows() {
    let campaign_id = Uuid::new_v4();
    let rows = vec![sample_row(
        "build",
        JournalDecisionKind::StepIntegrated,
        "ok",
    )];
    // JournalRow itself carries no repo/roadmap field to derive from.
    assert!(serde_json::to_value(&rows[0])
        .unwrap()
        .get("repo")
        .is_none());
    assert!(serde_json::to_value(&rows[0])
        .unwrap()
        .get("roadmap")
        .is_none());

    // The renderer is only reachable by supplying RunRecordMeta explicitly.
    let meta = sample_meta(None);
    let notes = render_notes_md(&campaign_id, &rows, &meta);
    assert!(notes.contains(&meta.repo));
    assert!(notes.contains(&meta.roadmap));
}

// ---------------------------------------------------------------------------
// EN.12.D task 7 — route-level integration coverage, plus the standing
// fixture for the un-gateable `bastion journal $ID | grep -q 'bail'`
// criterion (D64). The CLI verb lives in another repo and its binary is
// installed separately, so it cannot be exercised here; this file's
// `bail_reason_is_returned_through_the_read_route` test is the standing
// fixture named by the block record for that criterion.
//
// The three campaign-content tests below require a live Postgres (the read
// route only ever serves rows through `state.durable.pool()`, and
// `insert_journal_row` itself needs a live database) — they are
// `#[ignore]`d exactly like `engine-store`'s `postgres_round_trip.rs` tests,
// opted into with:
//
// ```sh
// DATABASE_URL=postgres://... cargo nextest run -p engine-serve --test journal_integration --run-ignored ignored-only
// ```
//
// The no-`DATABASE_URL` self-skip path (pool absent -> clean 404) is proven
// unconditionally (no live database, no `#[ignore]`) both here
// (`read_route_self_skips_to_404_with_no_pool_configured`) and in
// `crate::journal`'s own unit tests
// (`get_campaign_journal_self_skips_to_404_with_no_pool_configured`).

use std::sync::Arc;

use actix_web::test as actix_test;
use actix_web::{web, App};
use engine_core::dispatch::Dispatcher;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;
use engine_store::{connect, insert_journal_row};

const TEST_API_KEY: &str = "journal-integration-test-key";

fn route_test_app_state(pool: Option<sqlx::PgPool>) -> AppState {
    AppState::builder(
        Arc::new(Dispatcher::new()),
        LiveStateStore::new(),
        spawn_durable_writer(pool),
        TEST_API_KEY.to_string(),
    )
    .build()
}

async fn live_pool() -> sqlx::PgPool {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");
    connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL")
}

/// STANDING FIXTURE for the un-gateable criterion `bastion journal $ID |
/// grep -q 'bail'` (D64): drives a campaign with a bailed step through the
/// read route (task 5) and asserts the bail reason appears in the response
/// body, which is exactly what the out-of-repo `bastion journal` CLI verb
/// would print.
#[actix_web::test]
#[ignore = "requires a live Postgres; run with DATABASE_URL set and --run-ignored ignored-only (see file header)"]
async fn bail_reason_is_returned_through_the_read_route() {
    let pool = live_pool().await;
    let campaign_id = Uuid::new_v4();

    let mut row = sample_row("deploy", JournalDecisionKind::StepBailed, "network timeout");
    row.campaign_id = campaign_id.to_string();
    insert_journal_row(&pool, &row)
        .await
        .expect("insert_journal_row should succeed against a live Postgres");

    let state = route_test_app_state(Some(pool));
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = actix_test::TestRequest::get()
        .uri(&format!("/campaigns/{campaign_id}/journal"))
        .insert_header(("X-API-Key", TEST_API_KEY))
        .to_request();
    let resp = actix_test::call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["campaign_id"], campaign_id.to_string());
    let rows = body["rows"].as_array().expect("rows array");
    assert!(
        rows.iter()
            .any(|r| r["kind"] == "step_bailed" && r["reason"] == "network timeout"),
        "expected a step_bailed row naming the bail reason, got: {body}"
    );
}

/// A campaign halted by a budget cap returns its `BudgetHalted` row through
/// the same read route, carrying the cap name / spend / limit in `detail`.
#[actix_web::test]
#[ignore = "requires a live Postgres; run with DATABASE_URL set and --run-ignored ignored-only (see file header)"]
async fn budget_halted_row_is_returned_through_the_read_route() {
    let pool = live_pool().await;
    let campaign_id = Uuid::new_v4();

    let mut row = sample_row(
        "chain-block-3",
        JournalDecisionKind::BudgetHalted,
        "daily cap exceeded",
    );
    row.campaign_id = campaign_id.to_string();
    row.detail = serde_json::json!({ "cap_name": "daily", "spent": 42.5, "limit": 40.0 });
    insert_journal_row(&pool, &row)
        .await
        .expect("insert_journal_row should succeed against a live Postgres");

    let state = route_test_app_state(Some(pool));
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = actix_test::TestRequest::get()
        .uri(&format!("/campaigns/{campaign_id}/journal"))
        .insert_header(("X-API-Key", TEST_API_KEY))
        .to_request();
    let resp = actix_test::call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_test::read_body_json(resp).await;
    let rows = body["rows"].as_array().expect("rows array");
    let halt = rows
        .iter()
        .find(|r| r["kind"] == "budget_halted")
        .expect("expected a budget_halted row");
    assert_eq!(halt["detail"]["cap_name"], "daily");
    assert_eq!(halt["detail"]["spent"], 42.5);
    assert_eq!(halt["detail"]["limit"], 40.0);
}

/// A repo-less campaign (no roadmap, no repo — `JournalRow` carries no such
/// field, per the renderer's documentation-only assertion above) is still
/// addressable purely by `campaign_id` through the read route.
#[actix_web::test]
#[ignore = "requires a live Postgres; run with DATABASE_URL set and --run-ignored ignored-only (see file header)"]
async fn repo_less_campaign_is_addressable_through_the_read_route() {
    let pool = live_pool().await;
    let campaign_id = Uuid::new_v4();

    let mut row = sample_row("only-step", JournalDecisionKind::StepIntegrated, "ok");
    row.campaign_id = campaign_id.to_string();
    insert_journal_row(&pool, &row)
        .await
        .expect("insert_journal_row should succeed against a live Postgres");

    let state = route_test_app_state(Some(pool));
    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = actix_test::TestRequest::get()
        .uri(&format!("/campaigns/{campaign_id}/journal"))
        .insert_header(("X-API-Key", TEST_API_KEY))
        .to_request();
    let resp = actix_test::call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["campaign_id"], campaign_id.to_string());
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
}

/// With no `DATABASE_URL` (pool absent), the journal write path self-skips
/// rather than failing (`durable.rs`'s existing contract, widened to
/// journal rows), and the read route answers a clean 404 rather than
/// panicking or erroring — proven unconditionally, no live database
/// required, so this runs in the gated suite.
#[actix_web::test]
async fn read_route_self_skips_to_404_with_no_pool_configured() {
    let campaign_id = Uuid::new_v4();
    let state = route_test_app_state(None);

    let app = actix_test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let req = actix_test::TestRequest::get()
        .uri(&format!("/campaigns/{campaign_id}/journal"))
        .insert_header(("X-API-Key", TEST_API_KEY))
        .to_request();
    let resp = actix_test::call_service(&app, req).await;

    assert_eq!(resp.status(), 404);
}
