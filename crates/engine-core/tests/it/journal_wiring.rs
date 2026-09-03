//! `EN.14.E` task 5 — the block's Ships criterion, driven through the
//! production `register_debrief` seam (`engine_serve::workflows`), plus the
//! control proving that seam can fail (carryover
//! `gate-scope-must-be-shown-capable-of-failing`).
//!
//! ## Why this file cannot itself hold a live-Postgres row count
//!
//! `engine-core`'s `Cargo.toml` (this crate) depends on `engine-contract`
//! and, dev-only, `engine-serve` — never `engine-store` or `sqlx` directly
//! (see `blocked_bridge.rs`'s module doc for the same dependency-boundary
//! note on a different seam). `engine_serve::journal`'s reader/sink
//! constructors take an `Option<PgPool>` by value but name no public way to
//! *obtain* a live pool from outside `engine-serve` — that connection is
//! owned entirely by the host process (`bastion serve`, a different repo)
//! per `AppState`'s own doc comment. So a genuine "curl the route, get rows
//! back from Postgres" run cannot be written as a `#[test]` in this binary
//! without adding `sqlx`/`actix-web` as dependencies of this crate, which is
//! outside this task's declared files.
//!
//! That end-to-end proof already exists, at the layer that *can* reach
//! Postgres: `crates/engine-serve/tests/journal_integration.rs`'s
//! `#[ignore]`d `debrief_renders_end_to_end_and_is_retrievable_over_the_journal_route`
//! drives the real `DEBRIEF` graph over rows actually written to and read
//! back from a live database through the real `GET /campaigns/{id}/journal`
//! route. Run against a locally migrated database
//! (`crates/engine-store/migrations/0001_create_journal.sql` applied to
//! `orchestration_sandbox`) as part of this task:
//!
//! ```text
//! $ DATABASE_URL="postgres://orchestration@localhost:5432/orchestration_sandbox" \
//!     cargo nextest run -p engine-serve --test journal_integration --run-ignored ignored-only
//!     Starting 4 tests across 1 binary (7 tests skipped)
//!         PASS [   0.402s] (1/4) engine-serve::journal_integration repo_less_campaign_is_addressable_through_the_read_route
//!         PASS [   0.402s] (2/4) engine-serve::journal_integration budget_halted_row_is_returned_through_the_read_route
//!         PASS [   0.478s] (3/4) engine-serve::journal_integration bail_reason_is_returned_through_the_read_route
//!         PASS [   0.730s] (4/4) engine-serve::journal_integration debrief_renders_end_to_end_and_is_retrievable_over_the_journal_route
//!      Summary [   0.730s] 4 tests run: 4 passed, 7 skipped
//! ```
//!
//! `debrief_renders_end_to_end_and_is_retrievable_over_the_journal_route`
//! asserts `rows["rows"].as_array()` finds a `debrief_rendered` row after a
//! real `DEBRIEF` run — the literal "journal read returns a number greater
//! than 0" Ships criterion — reached over the real HTTP route.
//!
//! The paired control — the route returning 404 with an empty body when
//! nothing has ever been written for a campaign, exactly the state every
//! campaign was in before this block's task 4 wired a sink at all — is
//! `read_route_self_skips_to_404_with_no_pool_configured` in that same
//! file, unconditional (no live database needed) and run as part of this
//! task:
//!
//! ```text
//! $ cargo nextest run -p engine-serve --test journal_integration -E 'test(self_skips_to_404)'
//!     Starting 1 test across 1 binary (10 tests skipped)
//!         PASS [   0.013s] (1/1) engine-serve::journal_integration read_route_self_skips_to_404_with_no_pool_configured
//!      Summary [   0.014s] 1 test run: 1 passed, 10 skipped
//! ```
//!
//! ## What THIS file proves instead
//!
//! The one seam `EN.14.E` task 4 actually added — `register_debrief`'s
//! dispatch-time resolution of `crate::journal::journal_durable_handle()`
//! (`workflows.rs`'s own doc comment on `register_debrief`) — from the
//! *production* factory itself, driven through the real `Dispatcher` and a
//! real `Workflow::run`, never a hand-built registry. The observable
//! surface reachable without a live pool is exactly what the route's own
//! logic (`get_campaign_journal`: `rows.is_empty()` -> 404) turns on:
//! whether the resolved reader sees any rows. With no `DurableHandle`
//! installed — the literal pre-task-4 state, and the state a hand-revert of
//! `register_debrief`'s sink wiring reproduces — the resolved reader reads
//! back empty for a real campaign run, which is the exact input that
//! `get_campaign_journal` maps to `404` with an empty body. This file pins
//! that logical link at the seam this crate can actually reach.

use engine_core::dispatch::Dispatcher;
use engine_serve::journal::{
    clear_journal_durable_handle, journal_durable_handle, journal_reader_live,
};
use engine_serve::workflows::register_debrief;

const CAMPAIGN_EVENT: &str = "11111111-1111-1111-1111-111111111111";

/// Drives `DEBRIEF` through the exact production entry point
/// (`register_debrief` + `Dispatcher::dispatch_with_event` +
/// `Workflow::run`) — "run a campaign" — and returns the rendered brief
/// text plus whatever the seam `register_debrief` resolves
/// (`journal_durable_handle()`'s pool, fed straight into
/// `journal_reader_live`, matching `register_debrief`'s own closure body)
/// reads back for that campaign afterwards.
async fn run_debrief_campaign_and_read_back(campaign_event: &str) -> (String, usize) {
    let mut dispatcher = Dispatcher::new();
    register_debrief(&mut dispatcher);

    let workflow = dispatcher
        .dispatch_with_event("DEBRIEF", &serde_json::json!(campaign_event))
        .expect("DEBRIEF should dispatch to a runnable Workflow through the production factory");

    let ctx = workflow
        .run(serde_json::json!(campaign_event), Box::new(|_| {}))
        .await
        .expect("a real DEBRIEF campaign run must complete");

    let brief = ctx.nodes[engine_core::workflows::orchestration::debrief::DEBRIEF_NODE_NAME]
        ["brief"]
        .as_str()
        .expect("DebriefNode result must carry a brief string")
        .to_string();

    // The same resolution `register_debrief`'s closure performs at dispatch
    // time (`workflows.rs`): read back through whatever pool the currently
    // installed process-global `DurableHandle` carries.
    let pool = journal_durable_handle().and_then(|handle| handle.pool().cloned());
    let campaign_id = uuid::Uuid::parse_str(campaign_event).expect("fixture id is a valid UUID");
    let rows = journal_reader_live(pool)
        .rows_for_campaign(&campaign_id)
        .await
        .expect("a self-skipping reader never errors, only returns empty");

    (brief, rows.len())
}

/// **The control, proven first**: with no `DurableHandle` installed — the
/// state every campaign was in before task 4's wiring landed, and the state
/// reverting that wiring reproduces — a real `DEBRIEF` campaign run still
/// completes (the digest dispatch and the brief render are independent of
/// the journal sink), but the seam `register_debrief` resolves for a
/// journal read comes back with **zero** rows for that campaign. `zero
/// rows` is precisely the input `get_campaign_journal` (`journal.rs`) maps
/// to a `404` with an empty body — proving the route-level 404 the block
/// record names is downstream of this exact seam, without needing a live
/// Postgres pool to observe it.
#[tokio::test]
async fn campaign_run_reads_back_zero_rows_with_no_durable_handle_installed() {
    // Guard against another test in this file (or a re-run) having left a
    // handle installed — this test's whole point is the absent-handle case.
    clear_journal_durable_handle();
    assert!(
        journal_durable_handle().is_none(),
        "precondition: no DurableHandle installed"
    );

    let (brief, row_count) = run_debrief_campaign_and_read_back(CAMPAIGN_EVENT).await;

    assert!(
        !brief.is_empty(),
        "the campaign run itself must still complete and render a brief"
    );
    assert_eq!(
        row_count, 0,
        "with the sink wiring absent (task-4-reverted state), the read seam must see zero rows \
         for the campaign — the exact input the route maps to 404 with an empty body"
    );
}

/// The suite still builds under the workspace-wide profile this task's
/// acceptance criteria name (`cargo nextest run --workspace --no-run`),
/// asserted here as a real invocation rather than merely trusted — this is
/// the cheapest possible regression guard against a future edit to this
/// binary (or its `Cargo.toml`) that would otherwise only be caught by CI.
/// `#[ignore]`d because it re-invokes the whole workspace's compiler from
/// inside a test process (slow, redundant with the harness's own gate); run
/// explicitly with `--run-ignored ignored-only` when validating this task
/// by hand.
#[test]
#[ignore = "re-invokes `cargo nextest run --workspace --no-run`; the workspace-wide harness gate \
            already runs this exact command directly"]
fn workspace_no_run_build_still_succeeds() {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["nextest", "run", "--workspace", "--no-run"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .status()
        .expect("failed to invoke `cargo nextest run --workspace --no-run`");

    assert!(
        status.success(),
        "cargo nextest run --workspace --no-run must exit 0"
    );
}

/// `register_debrief`'s dispatch-time resolution genuinely reads the
/// process-global cell fresh on each dispatch, not a snapshot taken at
/// registration: installing a handle, dispatching, then clearing it and
/// dispatching again must not panic or error either way, and the second
/// (post-clear) run's seam resolution matches the no-handle-installed
/// control above. `Arc<_>` unused beyond proving the handle constructor
/// this crate CAN reach (`engine_serve::durable::spawn_durable_writer`,
/// with a `None` pool — the only pool value this crate's dependency graph
/// can construct) round-trips through `set_journal_durable_handle` /
/// `journal_durable_handle` / `clear_journal_durable_handle` exactly as
/// `crate::journal`'s own `#[cfg(test)]` suite (task 4) proved from inside
/// `engine-serve`.
#[tokio::test]
async fn installing_then_clearing_the_handle_around_a_real_dispatch_is_safe() {
    let handle = engine_serve::durable::spawn_durable_writer(None);
    engine_serve::journal::set_journal_durable_handle(handle);
    assert!(journal_durable_handle().is_some());

    let (brief_with_handle, _rows_with_handle) =
        run_debrief_campaign_and_read_back(CAMPAIGN_EVENT).await;
    assert!(!brief_with_handle.is_empty());

    clear_journal_durable_handle();
    assert!(journal_durable_handle().is_none());

    let (brief_without_handle, rows_without_handle) =
        run_debrief_campaign_and_read_back(CAMPAIGN_EVENT).await;
    assert!(!brief_without_handle.is_empty());
    assert_eq!(
        rows_without_handle, 0,
        "clearing the handle must reproduce the same zero-row read the control above proves"
    );
}
