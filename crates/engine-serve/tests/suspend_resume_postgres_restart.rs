//! Live-Postgres tests for EN.6.F's `rehydrate_from_store` fallback
//! (`crates/engine-serve/src/resume.rs`) — the resume path a fresh process
//! (one that never held a run in its in-memory suspended index) falls back
//! to.
//!
//! `suspend_resume_http.rs` proves the in-memory resume path end-to-end but
//! deliberately runs with no `DATABASE_URL`, so it can never exercise
//! `rehydrate_from_store` — the branch `resume_run` takes only when
//! `take_for_resume` returns `TakeForResume::NotFound`. `postgres_round_trip.rs`
//! tests `upsert_event`/`get_task_context` directly but never drives them
//! through the HTTP resume route. Two scenarios live here:
//!
//! 1. `resume_after_simulated_restart_rehydrates_from_postgres` — the happy
//!    path: a run suspends, its durable row lands in Postgres, the in-memory
//!    index is evicted to simulate a restart, and `/resume` completes purely
//!    off the Postgres row.
//! 2. `resume_against_a_row_with_a_malformed_suspension_marker_is_404_not_a_panic`
//!    — the corrupt-state path: a row is planted directly with a
//!    malformed `metadata.suspension` shape (`metadata` is an untyped
//!    `serde_json::Value` in the contract, so Postgres never rejects this),
//!    and `/resume` must 404 cleanly rather than panic or 500.
//!
//! `#[ignore]`d like every other live-Postgres test in this workspace (see
//! `postgres_round_trip.rs`'s header for the rationale): CI has no live
//! Postgres, so `cargo nextest run --workspace` reports these as skipped, not
//! passed — an honest signal. Run explicitly with:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p engine-serve --test suspend_resume_postgres_restart -- --ignored
//! ```

use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{test, web, App};
use engine_core::dispatch::Dispatcher;
use engine_core::{Node, NodeConfig, NodeError, NodeRegistry, Workflow, WorkflowSchema};
use engine_contract::TaskContext;
use engine_serve::abort::RunRegistry;
use engine_serve::durable::spawn_durable_writer;
use engine_serve::http::{configure, AppState};
use engine_serve::live_state::LiveStateStore;
use engine_serve::suspend;
use uuid::Uuid;

const API_KEY: &str = "suspend-resume-postgres-restart-test-key";

/// Completes immediately, stamping `{ "ran": true }` under its own identity.
struct SuccessNode {
    identity: &'static str,
}

#[async_trait::async_trait]
impl Node for SuccessNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        ctx.nodes.insert(
            self.identity.to_string(),
            serde_json::json!({ "ran": true }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}

fn suspend_node_schema(workflow_type: &str) -> WorkflowSchema {
    let mut nodes = StdHashMap::new();
    nodes.insert(
        "SuspendNode".to_string(),
        NodeConfig::new("SuspendNode", vec!["MarkerNode".to_string()]),
    );
    nodes.insert(
        "MarkerNode".to_string(),
        NodeConfig::new("MarkerNode", vec![]),
    );
    WorkflowSchema::new(workflow_type, "SuspendNode", nodes)
}

/// A dispatcher with a single `SuspendNode(enabled: true) -> MarkerNode`
/// workflow — suspends almost immediately after trigger, matching
/// `suspend_resume_http.rs`'s fixture of choice.
fn dispatcher_with_suspend_fixture(workflow_type: &str) -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    let wt = workflow_type.to_string();
    dispatcher.register(
        suspend_node_schema(workflow_type),
        Box::new(move |_event: &serde_json::Value| {
            let mut registry = NodeRegistry::new();
            registry.register(Box::new(
                engine_core::nodes::SuspendNode::new("SuspendNode").with_enabled(true),
            ));
            registry.register(Box::new(SuccessNode {
                identity: "MarkerNode",
            }));
            Ok(Workflow::new(registry, suspend_node_schema(&wt)))
        }),
    );
    dispatcher
}

/// Triggers `workflow_type` against `$app` and blocks until the row is
/// durably readable from Postgres AND marked suspended there — i.e. until
/// the async durable writer has actually landed the suspension marker, not
/// just until the in-memory index sees it (the in-memory index lands first;
/// a restart test must wait for the slower of the two so evicting the
/// in-memory entry leaves a genuinely resumable Postgres row behind). A
/// macro (rather than a generic fn) so it works against whatever unnameable
/// `impl Service<..>` type `test::init_service` produces, matching
/// `suspend_resume_http.rs`'s `trigger_and_wait_suspended!` precedent.
macro_rules! trigger_and_wait_durably_suspended {
    ($app:expr, $pool:expr, $workflow_type:expr) => {{
        let req = test::TestRequest::post()
            .uri("/events/")
            .insert_header(("X-API-Key", API_KEY))
            .set_json(serde_json::json!({ "workflow_type": $workflow_type, "data": {} }))
            .to_request();
        let resp = test::call_service(&$app, req).await;
        assert_eq!(resp.status(), 202);
        let body: serde_json::Value = test::read_body_json(resp).await;
        let run_id = body["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id should be a parseable UUID");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut landed = false;
            if let Ok(Some(task_context)) = engine_store::get_task_context($pool, run_id).await {
                if engine_core::suspend::is_suspended(&task_context.metadata) {
                    landed = true;
                }
            }
            if landed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "run's suspension marker never landed in Postgres"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        run_id
    }};
}

/// The restart scenario: a run suspends, its durable row lands in Postgres,
/// then the process "restarts" (the in-memory suspended index is evicted,
/// simulating a fresh `engine-serve` process that never held this run). The
/// only way `/resume` can complete is `resume_run`'s `rehydrate_from_store`
/// fallback reading the row straight back out of Postgres.
#[actix_web::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-serve --test suspend_resume_postgres_restart -- --ignored`"]
async fn resume_after_simulated_restart_rehydrates_from_postgres() {
    const WORKFLOW_TYPE: &str = "postgres-restart-resume";

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");
    let pool = engine_store::connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let dispatcher = dispatcher_with_suspend_fixture(WORKFLOW_TYPE);
    let state = AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(Some(pool.clone())),
        runs: RunRegistry::new(),
        api_key: API_KEY.to_string(),
    };
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let run_id = trigger_and_wait_durably_suspended!(app, &pool, WORKFLOW_TYPE);

    // The in-memory index also saw this run (same process) -- evict it to
    // simulate the fresh-process case `rehydrate_from_store` exists for.
    // Also drop the pause signal / registration the in-memory path would
    // otherwise have left behind, so nothing but the Postgres row is left to
    // resume from.
    let evicted = suspend::remove_suspended(run_id);
    assert!(
        evicted.is_some(),
        "run should have been in the in-memory index before eviction"
    );
    suspend::remove_pause_signal(run_id);

    // Confirm the eviction actually removed it -- otherwise this test would
    // silently exercise the in-memory path instead of the Postgres fallback.
    assert!(
        !suspend::list_suspended()
            .into_iter()
            .any(|(id, _)| id == run_id),
        "run must be absent from the in-memory index to force the Postgres fallback"
    );

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&app, resume_req).await;
    assert_eq!(
        resume_resp.status(),
        202,
        "resume must succeed via rehydrate_from_store even though the in-memory index never held this run"
    );
    let resume_body: serde_json::Value = test::read_body_json(resume_resp).await;
    assert_eq!(resume_body["run_id"], serde_json::json!(run_id));
    assert_eq!(resume_body["resume_at"], "MarkerNode");

    let deadline = Instant::now() + Duration::from_secs(5);
    let final_body = loop {
        let req = test::TestRequest::get()
            .uri(&format!("/events/{run_id}"))
            .insert_header(("X-API-Key", API_KEY))
            .to_request();
        let resp = test::call_service(&app, req).await;
        let body: serde_json::Value = test::read_body_json(resp).await;
        if body["status"] == "succeeded" {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "resumed run never reached succeeded, last body: {body:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(
        final_body["task_context"]["node_runs"]["SuspendNode"]["status"],
        serde_json::json!("success"),
        "SuspendNode must not be re-executed after a Postgres-rehydrated resume"
    );
    assert_eq!(
        final_body["task_context"]["node_runs"]["MarkerNode"]["status"],
        serde_json::json!("success")
    );

    // Clean up so repeated local runs don't accumulate rows.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(run_id)
        .execute(&pool)
        .await
        .expect("cleanup delete should succeed");
}

/// A companion to the restart test above: instead of a genuinely-suspended
/// row, this plants a row whose `task_context.metadata.suspension` is
/// malformed (a crash mid-write, a foreign/older writer, or manual data
/// surgery could all produce this -- `metadata` is an untyped
/// `serde_json::Value` in the contract, so Postgres itself never rejects it).
/// `rehydrate_from_store` must degrade this to "not resumable" and 404, not
/// panic or 500 -- `engine_core::suspend::is_suspended` (unit-tested
/// directly against malformed shapes in `engine-core::suspend::tests`) is
/// what makes that degrade-safe, and this test proves the full HTTP resume
/// route actually goes through it end-to-end against a real Postgres row.
#[actix_web::test]
#[ignore = "requires a live Postgres; run with `cargo test -p engine-serve --test suspend_resume_postgres_restart -- --ignored`"]
async fn resume_against_a_row_with_a_malformed_suspension_marker_is_404_not_a_panic() {
    const WORKFLOW_TYPE: &str = "postgres-malformed-suspension-resume";

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set to run this ignored test (see file header)");
    let pool = engine_store::connect(&database_url)
        .await
        .expect("failed to connect to DATABASE_URL");

    let run_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    // `suspended` is a required `bool` on `SuspensionState` -- a string here
    // fails the typed parse, exactly like the malformed-metadata unit tests
    // in `engine-core::suspend::tests`, but reached this time through a real
    // Postgres row rather than an in-process `serde_json::json!` literal.
    let malformed_metadata = serde_json::json!({
        "suspension": {
            "suspended": "yes",
            "resume_at": "MarkerNode",
            "unexpected_field": { "nested": "garbage" }
        }
    });

    let row = engine_contract::EventsRow {
        id: run_id,
        workflow_type: WORKFLOW_TYPE.to_string(),
        data: serde_json::json!({}),
        task_context: TaskContext {
            event: serde_json::json!({}),
            nodes: StdHashMap::new(),
            metadata: malformed_metadata,
            node_runs: StdHashMap::new(),
        },
        created_at: now,
        updated_at: now,
    };
    engine_store::insert_event(&pool, &row)
        .await
        .expect("insert_event should succeed even with a malformed suspension shape");

    // A fresh AppState with empty in-memory indexes -- exactly the "process
    // never held this run" case; the only path that can answer at all is
    // `rehydrate_from_store`.
    let dispatcher = dispatcher_with_suspend_fixture(WORKFLOW_TYPE);
    let state = AppState {
        dispatcher: Arc::new(dispatcher),
        live: LiveStateStore::new(),
        durable: spawn_durable_writer(Some(pool.clone())),
        runs: RunRegistry::new(),
        api_key: API_KEY.to_string(),
    };
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(configure),
    )
    .await;

    let resume_req = test::TestRequest::post()
        .uri(&format!("/events/{run_id}/resume"))
        .insert_header(("X-API-Key", API_KEY))
        .to_request();
    let resume_resp = test::call_service(&app, resume_req).await;
    assert_eq!(
        resume_resp.status(),
        404,
        "a malformed suspension marker must degrade to 'not resumable', never panic or 500"
    );

    // Clean up so repeated local runs don't accumulate rows.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(run_id)
        .execute(&pool)
        .await
        .expect("cleanup delete should succeed");
}
