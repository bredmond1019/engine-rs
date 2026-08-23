//! Authenticated abort endpoint + per-run cancellation-token registry
//! (EN.2.B task 5).
//!
//! `RunRegistry` maps a live run's `run_id` to the `CancellationToken`
//! threaded into `Workflow::run_with` for that run (task 3) — the same
//! `Arc<RwLock<HashMap<..>>>` shape as `LiveStateStore`. `post_events`
//! (`http.rs`) registers a fresh token alongside the freshly-minted `run_id`
//! before running, and deregisters it once the run ends (success, failure, or
//! already-cancelled) so an abort against a finished run correctly reads as
//! unknown (404) rather than succeeding on a dead token.
//!
//! `POST /events/{run_id}/abort`: 401 without a valid `X-API-Key` (reuses
//! `crate::http::check_api_key`), 404 for an unknown/finished run id, 202
//! Accepted for a live run — after which that run's token is observably
//! triggered.
//!
//! **EN.6.F: a suspended run has no live token to trigger.**
//! `crate::suspend::spawn_run` deregisters a run's `CancellationToken`
//! unconditionally on exit — including the suspended branch, since nobody
//! is polling the token while a run sits parked in the suspended index —
//! so a suspended run is invisible to [`RunRegistry`]. It must still be
//! killable (the consumer-facing "Stop" affordance applies at any point,
//! suspended included), so [`abort_run`] falls back to the suspended index:
//! it pulls the entry out with [`crate::suspend::remove_suspended`], stamps
//! cancellation into its retained snapshot the same way
//! `suspend::spawn_run`'s bounded-ring eviction backstop does, and marks it
//! terminal.
//!
//! **`EN.11.F` task 2: campaign-scoped abort.** A campaign (`EN.11.E`) is N
//! runs, so [`RunRegistry`]'s per-run token cannot stop a chain — cancelling
//! one run's token does not reach the orchestration chain's own block-
//! boundary check (`EN.11.F` task 4). [`CampaignRegistry`] mirrors
//! `RunRegistry`'s shape exactly (an `Arc<RwLock<HashMap<..>>>`, cheap to
//! clone, `register`/`deregister`/`get`) but keyed by campaign id instead of
//! run id. The chain's block-boundary check reads this token; the route
//! below (`POST /campaigns/{id}/abort`) is what sets it.
//!
//! `POST /campaigns/{id}/abort`: 401 without a valid `X-API-Key` (reuses
//! `crate::http::check_api_key`, same as [`abort_run`]), 404 for an unknown
//! or already-finished campaign id **and** for a malformed/non-UUID path
//! segment (mirroring `crate::http::get_campaign`'s convention rather than
//! inventing a `400`), 202 Accepted for a live campaign — after which that
//! campaign's token is observably triggered. The handler never blocks on
//! anything beyond the registry's own lock, so it returns promptly in every
//! branch — there is no path that can hang.
//!
//! The bastion-side `bastion abort <campaign>` CLI verb is explicitly out of
//! scope for this block (`EN.11.F`'s `out_of_scope`) — engine-rs ships the
//! route only.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use engine_core::CancellationToken;
use uuid::Uuid;

use crate::http::{check_api_key, AppState};
use crate::live_state::RunId;

/// In-memory registry of the `CancellationToken` for every currently-live
/// run, keyed by `run_id`. Cheap to clone (an `Arc` around the guarded map),
/// mirroring `LiveStateStore`'s shape so it can be shared between the HTTP
/// handlers without extra synchronization.
#[derive(Clone, Default)]
pub struct RunRegistry {
    inner: Arc<RwLock<HashMap<RunId, CancellationToken>>>,
}

impl RunRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `token` under `run_id`, overwriting whatever was there
    /// before. Intended to be called right after a run's `run_id` is minted,
    /// before the run starts.
    pub fn register(&self, run_id: RunId, token: CancellationToken) {
        let mut guard = self
            .inner
            .write()
            .expect("run registry lock poisoned on write");
        guard.insert(run_id, token);
    }

    /// Remove and return `run_id`'s token, if any. Intended to be called once
    /// a run ends (success, failure, or cancellation), so a subsequent abort
    /// request against that `run_id` reads as unknown rather than triggering
    /// a token nobody is checking anymore.
    pub fn deregister(&self, run_id: RunId) -> Option<CancellationToken> {
        let mut guard = self
            .inner
            .write()
            .expect("run registry lock poisoned on write");
        guard.remove(&run_id)
    }

    /// Look up `run_id`'s token, if the run is still live.
    pub fn get(&self, run_id: RunId) -> Option<CancellationToken> {
        let guard = self
            .inner
            .read()
            .expect("run registry lock poisoned on read");
        guard.get(&run_id).cloned()
    }
}

/// In-memory registry of the `CancellationToken` for every currently-live
/// campaign, keyed by campaign id. Mirrors [`RunRegistry`]'s shape exactly
/// — an `Arc` around a guarded map, cheap to clone — so a campaign-level
/// abort request can trigger the token the orchestration chain's
/// block-boundary check (`EN.11.F` task 4) reads.
#[derive(Clone, Default)]
pub struct CampaignRegistry {
    inner: Arc<RwLock<HashMap<Uuid, CancellationToken>>>,
}

impl CampaignRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `token` under `campaign_id`, overwriting whatever was there
    /// before. Intended to be called once, when a campaign's id is minted,
    /// before its first block starts.
    pub fn register(&self, campaign_id: Uuid, token: CancellationToken) {
        let mut guard = self
            .inner
            .write()
            .expect("campaign registry lock poisoned on write");
        guard.insert(campaign_id, token);
    }

    /// Remove and return `campaign_id`'s token, if any. Intended to be
    /// called once the campaign finishes (all blocks complete, aborted, or
    /// budget-halted), so a subsequent abort request against that campaign
    /// reads as unknown rather than triggering a token nobody is checking
    /// anymore.
    pub fn deregister(&self, campaign_id: Uuid) -> Option<CancellationToken> {
        let mut guard = self
            .inner
            .write()
            .expect("campaign registry lock poisoned on write");
        guard.remove(&campaign_id)
    }

    /// Look up `campaign_id`'s token, if the campaign is still live.
    pub fn get(&self, campaign_id: Uuid) -> Option<CancellationToken> {
        let guard = self
            .inner
            .read()
            .expect("campaign registry lock poisoned on read");
        guard.get(&campaign_id).cloned()
    }
}

/// `POST /campaigns/{id}/abort` (`EN.11.F` task 2) — 401 on a missing/bad
/// `X-API-Key`, 404 for an unknown or already-finished campaign id **and**
/// for a malformed/non-UUID path segment, otherwise 202 Accepted.
///
/// Triggering the token here does not itself stop anything — the
/// orchestration chain's block-boundary check (`EN.11.F` task 4) is what
/// observes `is_cancelled()` and halts the chain before starting the next
/// block. The in-flight block still finishes and commits (Fork 1's decided
/// semantics); this route only flips the flag the chain polls.
pub async fn abort_campaign(
    req: HttpRequest,
    path: web::Path<String>,
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

    if let Some(token) = state.campaigns.get(campaign_id) {
        token.cancel();
        return HttpResponse::Accepted()
            .json(serde_json::json!({ "campaign_id": campaign_id, "status": "aborting" }));
    }

    HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or finished campaign" }))
}

/// `POST /events/{run_id}/abort` — 401 on a missing/bad `X-API-Key`, 404 for
/// an unknown or already-finished `run_id`, otherwise 202 Accepted.
///
/// A live run is aborted by triggering its `CancellationToken` (the run loop
/// observes the cancellation at the next node boundary and stamps the
/// cancelled terminal state — see `crates/engine-core/src/workflow.rs`). A
/// **suspended** run (EN.6.F) has no live token to trigger, so it falls back
/// to pulling the entry directly out of `crate::suspend`'s suspended index,
/// stamping cancellation into its retained snapshot, and marking it terminal
/// — see this module's docs.
pub async fn abort_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();

    if let Some(token) = state.runs.get(run_id) {
        token.cancel();
        return HttpResponse::Accepted()
            .json(serde_json::json!({ "run_id": run_id, "status": "aborting" }));
    }

    if let Some(mut entry) = crate::suspend::remove_suspended(run_id) {
        engine_core::stamp_cancelled(&mut entry.snapshot.metadata);
        crate::stream::publish_terminal(run_id, &entry.snapshot);
        state.live.mark_terminal(
            run_id,
            &entry.snapshot,
            entry.workflow_type,
            entry.created_at,
            chrono::Utc::now(),
        );
        crate::http::live_run_metadata()
            .write()
            .expect("live run metadata lock poisoned on write")
            .remove(&run_id);
        return HttpResponse::Accepted()
            .json(serde_json::json!({ "run_id": run_id, "status": "aborting" }));
    }

    HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or finished run" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_get_returns_the_same_token() {
        let registry = RunRegistry::new();
        let run_id = Uuid::new_v4();
        let token = CancellationToken::new();

        registry.register(run_id, token.clone());

        let fetched = registry.get(run_id).expect("token should be present");
        assert!(!fetched.is_cancelled());
        token.cancel();
        assert!(
            fetched.is_cancelled(),
            "fetched token shares state with the original clone"
        );
    }

    #[test]
    fn get_on_unknown_run_returns_none() {
        let registry = RunRegistry::new();
        assert!(registry.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn deregister_removes_and_returns_the_token() {
        let registry = RunRegistry::new();
        let run_id = Uuid::new_v4();
        let token = CancellationToken::new();
        registry.register(run_id, token);

        let removed = registry.deregister(run_id);

        assert!(removed.is_some());
        assert!(registry.get(run_id).is_none());
    }

    #[test]
    fn deregister_on_unknown_run_returns_none() {
        let registry = RunRegistry::new();
        assert!(registry.deregister(Uuid::new_v4()).is_none());
    }

    // ── CampaignRegistry ──────────────────────────────────────────────

    #[test]
    fn campaign_register_then_get_returns_the_same_token() {
        let registry = CampaignRegistry::new();
        let campaign_id = Uuid::new_v4();
        let token = CancellationToken::new();

        registry.register(campaign_id, token.clone());

        let fetched = registry.get(campaign_id).expect("token should be present");
        assert!(!fetched.is_cancelled());
        token.cancel();
        assert!(
            fetched.is_cancelled(),
            "fetched token shares state with the original clone"
        );
    }

    #[test]
    fn campaign_get_on_unknown_campaign_returns_none() {
        let registry = CampaignRegistry::new();
        assert!(registry.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn campaign_deregister_removes_and_returns_the_token() {
        let registry = CampaignRegistry::new();
        let campaign_id = Uuid::new_v4();
        let token = CancellationToken::new();
        registry.register(campaign_id, token);

        let removed = registry.deregister(campaign_id);

        assert!(removed.is_some());
        assert!(registry.get(campaign_id).is_none());
    }

    #[test]
    fn campaign_deregister_on_unknown_campaign_returns_none() {
        let registry = CampaignRegistry::new();
        assert!(registry.deregister(Uuid::new_v4()).is_none());
    }
}
