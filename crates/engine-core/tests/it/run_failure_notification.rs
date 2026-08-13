//! Integration tests for terminal run-failure notification
//! (`ticket-run-failure-notification`, task 4).
//!
//! Per `planning/ticket-run-failure-notification/tasks.md`: the burst case
//! is the acceptance criterion that matters — many runs fail in quick
//! succession and the queue produces ONE deliverable plus a digest tail,
//! never N messages — plus an end-to-end single-failure case and the two
//! negative cases (`cancelled`/`succeeded` produce nothing deliverable).
//!
//! This binary (`engine-core`'s integration-test crate) has no dependency
//! on `engine-serve`, so there is no `LiveStateStore::mark_terminal` to
//! call here — that hook is exercised directly in
//! `engine-serve/src/live_state.rs`'s own tests (task 3). What these tests
//! exercise end to end is the task 1 decision
//! (`operator::failure::should_notify`), the task 2 renderer
//! (`operator::failure::render_failure_payload`), and the task 5 `EN.8.B`
//! queue (`OperatorQueue` + `build_digest`/`storm_digest`) wired together
//! exactly the way `mark_terminal` wires them — a fixed, injected clock, an
//! in-memory queue, no network, no database, no shared fixture file.

use chrono::{DateTime, TimeZone, Utc};

use engine_core::operator::failure::{
    render_failure_payload, should_notify, FailedNode, FailureNotifyPolicy,
};
use engine_core::operator::queue::{
    storm_digest, ItemSource, OperatorQueue, OperatorQueueItem, OperatorQueuePolicy,
};
use engine_core::operator::OperatorPayloadLimits;

/// Fixed clock — every test in this module reasons about a deterministic
/// instant, never `Utc::now()`.
fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

/// Mirrors `live_state.rs::maybe_enqueue_failure_notification`'s wiring:
/// given a terminal status and (for `failed`) the failing nodes, decide
/// whether to notify, and if so render + enqueue exactly one item into
/// `queue`. A no-op for a status the policy doesn't notify on, matching the
/// hook's own silent-no-op behavior.
#[allow(clippy::too_many_arguments)]
fn maybe_enqueue(
    queue: &mut OperatorQueue,
    policy: &FailureNotifyPolicy,
    run_id: &str,
    workflow_type: &str,
    terminal_status: &str,
    failed_nodes: &[FailedNode],
    enqueued_at: DateTime<Utc>,
) -> bool {
    if !should_notify(terminal_status, policy) {
        return false;
    }
    let limits = OperatorPayloadLimits::default();
    let gate_id = format!("run-failure:{run_id}");
    let Ok(validated) = render_failure_payload(
        gate_id.clone(),
        run_id,
        workflow_type,
        terminal_status,
        failed_nodes,
        &limits,
    ) else {
        return false;
    };
    let item = OperatorQueueItem::new(
        gate_id,
        validated.into_payload(),
        policy.failure_item_priority,
        enqueued_at,
        ItemSource::GateApproval,
    );
    queue.enqueue(item);
    true
}

// ── end-to-end: a single failure produces one deliverable payload ──────

#[test]
fn a_single_failed_run_produces_one_deliverable_payload_naming_the_three_facts() {
    let policy = FailureNotifyPolicy::default();
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());

    let enqueued = maybe_enqueue(
        &mut queue,
        &policy,
        "run-abc",
        "sdlc_flow",
        "failed",
        &[FailedNode::new("BuildNode", "compile error")],
        ts(0),
    );
    assert!(enqueued, "a failed run must enqueue a notification");
    assert_eq!(queue.pending_count(), 1);

    let delivered = queue
        .next_deliverable(ts(0))
        .expect("exactly one deliverable item");
    let summary = &delivered.payload.rendered_summary;
    assert!(summary.contains("sdlc_flow"), "missing workflow type");
    assert!(summary.contains("run-abc"), "missing run id");
    assert!(summary.contains("BuildNode"), "missing failing node");

    assert_eq!(queue.pending_count(), 0);
    assert!(queue.next_deliverable(ts(0)).is_none());
}

// ── negative cases: cancelled / succeeded produce nothing deliverable ──

#[test]
fn a_cancelled_run_produces_nothing_deliverable() {
    let policy = FailureNotifyPolicy::default();
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());

    let enqueued = maybe_enqueue(
        &mut queue,
        &policy,
        "run-cancel",
        "sdlc_flow",
        "cancelled",
        &[],
        ts(0),
    );

    assert!(!enqueued, "a cancelled run must not enqueue anything");
    assert_eq!(queue.pending_count(), 0);
    assert!(queue.next_deliverable(ts(0)).is_none());
}

#[test]
fn a_succeeded_run_produces_nothing_deliverable() {
    let policy = FailureNotifyPolicy::default();
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());

    let enqueued = maybe_enqueue(
        &mut queue,
        &policy,
        "run-ok",
        "sdlc_flow",
        "succeeded",
        &[],
        ts(0),
    );

    assert!(!enqueued, "a succeeded run must not enqueue anything");
    assert_eq!(queue.pending_count(), 0);
    assert!(queue.next_deliverable(ts(0)).is_none());
}

// ── the burst case — the acceptance criterion that matters ─────────────

#[test]
fn a_burst_of_failed_runs_delivers_exactly_one_item_at_a_time_never_n() {
    // 30 runs fail in quick succession (all within the same second, the
    // way a cascading failure or a restart storm would land). The queue's
    // own depth limit (default 1) is what a caller reusing this queue
    // relies on -- this test asserts the queue never bypasses it, matching
    // `live_state.rs`'s own burst test at the `LiveStateStore` layer.
    let policy = FailureNotifyPolicy::default();
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
    let now = ts(0);

    for i in 0..30 {
        let enqueued = maybe_enqueue(
            &mut queue,
            &policy,
            &format!("run-{i}"),
            "sdlc_flow",
            "failed",
            &[FailedNode::new(format!("Node{i}"), "boom")],
            now,
        );
        assert!(enqueued, "every failed run in the burst must enqueue");
    }

    assert_eq!(queue.pending_count(), 30, "all 30 items are pending");

    // Exactly one deliverable at a time -- depth 1, never N messages.
    let first = queue.next_deliverable(now).expect("one deliverable item");
    assert_eq!(queue.open_count(), 1);
    assert!(
        queue.next_deliverable(now).is_none(),
        "the queue must not deliver a second item while one is open"
    );

    // Answering releases exactly one more, never a batch.
    assert!(queue.answer(&first.item_id));
    let second = queue
        .next_deliverable(now)
        .expect("answering releases exactly one more");
    assert_ne!(first.item_id, second.item_id);
    assert!(queue.next_deliverable(now).is_none());
}

#[test]
fn a_burst_of_failed_runs_collapses_into_one_storm_digest_plus_a_count() {
    // Same burst, viewed through the digest tail: `build_digest`/
    // `storm_digest` (EN.8.B task 4) is the mechanism this ticket reuses
    // rather than writing a parallel suppression path (spec Task 4).
    //
    // `OperatorQueue` exposes no accessor for its full pending list (by
    // design -- callers observe it only through `next_deliverable`), so
    // this test builds the same items `maybe_enqueue` would have enqueued
    // in parallel, exactly the way `restart_storm_of_n_blocked_edges_...`
    // does in `operator_queue.rs` for the blocked-edge case.
    let policy = FailureNotifyPolicy::default();
    let now = ts(0);
    let limits = OperatorPayloadLimits::default();

    let items: Vec<OperatorQueueItem> = (0..25)
        .map(|i| {
            let run_id = format!("run-{i}");
            let gate_id = format!("run-failure:{run_id}");
            let validated = render_failure_payload(
                gate_id.clone(),
                &run_id,
                "sdlc_flow",
                "failed",
                &[FailedNode::new(format!("Node{i}"), "boom")],
                &limits,
            )
            .expect("typical failure should validate");
            OperatorQueueItem::new(
                gate_id,
                validated.into_payload(),
                policy.failure_item_priority,
                now,
                ItemSource::GateApproval,
            )
        })
        .collect();
    assert_eq!(items.len(), 25);

    // All 25 arrived at the same instant `now` -- well within a 60s
    // suppression window -- so the storm digest folds them into ONE
    // summary carrying the top item plus the total count, never 25
    // separate messages.
    let digest = storm_digest(&items, 60, now).expect("one digest, not 25 messages");
    assert_eq!(digest.total_count, 25);
    assert_eq!(digest.generated_at, now);
}

#[test]
fn budget_halted_and_failed_runs_are_distinguishable_end_to_end() {
    let policy = FailureNotifyPolicy::default();
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
    let now = ts(0);

    assert!(maybe_enqueue(
        &mut queue,
        &policy,
        "run-halt",
        "sdlc_flow",
        "budget_halted",
        &[],
        now,
    ));
    assert!(maybe_enqueue(
        &mut queue,
        &policy,
        "run-fail",
        "sdlc_flow",
        "failed",
        &[FailedNode::new("BuildNode", "boom")],
        now,
    ));

    let first = queue.next_deliverable(now).expect("first deliverable");
    assert!(queue.answer(&first.item_id));
    let second = queue.next_deliverable(now).expect("second deliverable");

    let summaries: Vec<&str> = [&first, &second]
        .iter()
        .map(|i| i.payload.rendered_summary.as_str())
        .collect();
    assert_ne!(summaries[0], summaries[1]);
    assert!(
        summaries
            .iter()
            .any(|s| s.to_lowercase().contains("budget")),
        "one of the two summaries must name the budget halt"
    );
}
