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

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use uuid::Uuid;

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
