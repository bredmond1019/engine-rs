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

/// `POST /events/{run_id}/abort` — 401 on a missing/bad `X-API-Key`, 404 for
/// an unknown or already-finished `run_id`, otherwise triggers that run's
/// `CancellationToken` and returns 202 Accepted (the run loop observes the
/// cancellation at the next node boundary and stamps the cancelled terminal
/// state — see `crates/engine-core/src/workflow.rs`).
pub async fn abort_run(
    req: HttpRequest,
    path: web::Path<Uuid>,
    state: web::Data<AppState>,
) -> impl Responder {
    if !check_api_key(&req, &state.api_key) {
        return HttpResponse::Unauthorized().finish();
    }

    let run_id = path.into_inner();
    match state.runs.get(run_id) {
        Some(token) => {
            token.cancel();
            HttpResponse::Accepted()
                .json(serde_json::json!({ "run_id": run_id, "status": "aborting" }))
        }
        None => {
            HttpResponse::NotFound().json(serde_json::json!({ "error": "unknown or finished run" }))
        }
    }
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
}
