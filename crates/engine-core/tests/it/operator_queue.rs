//! Integration tests for the operator queue (`EN.8.B` task 5).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: the headline 60-item test,
//! the restart-storm test, the wedge (timeout) test, the deterministic
//! ordering test over shuffled input, and the stale-item test. All use fixed
//! timestamps and the in-memory `QueueSource`/level-predicate seams — no
//! wall-clock reads in assertions, no real filesystem.

use chrono::{DateTime, TimeZone, Utc};

use engine_core::operator::queue::{
    storm_digest, BlockedEdgeState, ItemSource, OperatorQueue, OperatorQueueItem,
    OperatorQueuePolicy, PendingBlockedEdge,
};
use engine_core::operator::OperatorPayload;

fn payload() -> OperatorPayload {
    OperatorPayload::new("gate-1", "summary", vec![])
}

fn ts(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

fn item(id: &str, priority: i32, secs: i64) -> OperatorQueueItem {
    OperatorQueueItem::new(id, payload(), priority, ts(secs), ItemSource::BlockedEdge)
}

fn now() -> DateTime<Utc> {
    ts(1_000_000)
}

// ── the headline 60-item test ───────────────────────────────────────────

#[test]
fn sixty_pending_items_deliver_exactly_one_the_highest_priority_then_release_one_more() {
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
    // Mixed priorities, not a monotonic sequence, so a stable-sort accident
    // on insertion order cannot pass this by coincidence.
    let priorities: Vec<i32> = (0..60)
        .map(|i| ((i * 37) % 97) - 40) // deterministic pseudo-shuffle, includes negatives
        .collect();
    let mut highest = i32::MIN;
    let mut highest_id = String::new();
    for (i, &p) in priorities.iter().enumerate() {
        let id = format!("item-{i}");
        if p > highest {
            highest = p;
            highest_id = id.clone();
        }
        queue.enqueue(item(&id, p, i as i64));
    }

    let delivered = queue
        .next_deliverable(now())
        .expect("exactly one delivered");
    assert_eq!(delivered.item_id, highest_id);
    assert_eq!(delivered.effective_priority, highest);
    assert_eq!(queue.open_count(), 1);
    assert_eq!(queue.pending_count(), 59);

    // Depth 1: nothing further is deliverable while it is open.
    assert_eq!(queue.next_deliverable(now()), None);

    // Answering releases exactly one more.
    assert!(queue.answer(&delivered.item_id));
    let second = queue.next_deliverable(now()).expect("second delivered");
    assert_ne!(second.item_id, delivered.item_id);
    assert_eq!(queue.pending_count(), 58);
    assert_eq!(queue.next_deliverable(now()), None);
}

// ── restart-storm: N already-blocked edges -> one message, not N ───────

#[test]
fn restart_storm_of_n_blocked_edges_produces_one_message_not_n() {
    // Simulate a restart burst: N blocked-edge records, all discovered at
    // the same instant (the sink read happens once, right after restart).
    // In place of an `InMemoryQueueSource` (a `#[cfg(test)]`-only stub not
    // visible to this external integration-test binary), build the pending
    // list directly — the shape a `QueueSource::pending()` call would
    // return.
    let pending: Vec<PendingBlockedEdge> = (0..40)
        .map(|i| PendingBlockedEdge {
            session: format!("sess-{i}"),
            host: "mini-console".to_string(),
            to: BlockedEdgeState::Blocked,
            observed_at: ts(0),
        })
        .collect();
    assert_eq!(pending.len(), 40);

    // Translate into queue items, all enqueued at the same restart instant.
    let restart_at = ts(0);
    let items: Vec<OperatorQueueItem> = pending
        .iter()
        .enumerate()
        .map(|(i, p)| {
            OperatorQueueItem::new(
                format!("edge-{}", p.session),
                payload(),
                i as i32,
                restart_at,
                ItemSource::BlockedEdge,
            )
        })
        .collect();

    // Storm suppression collapses all 40 simultaneous arrivals into one
    // digest — one message, never 40.
    let digest = storm_digest(&items, 60, restart_at).expect("one digest for the whole storm");
    assert_eq!(digest.total_count, 40);
    // The top item is the highest-priority one (index 39, priority 39).
    assert_eq!(digest.top.item_id, "edge-sess-39");

    // And feeding the same storm through the depth-limited queue itself
    // still yields exactly one open item, never N.
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
    for it in items {
        queue.enqueue(it);
    }
    assert!(queue.next_deliverable(restart_at).is_some());
    assert_eq!(queue.open_count(), 1);
    assert_eq!(queue.next_deliverable(restart_at), None);
}

// ── wedge test: unanswered past timeout releases the next, re-queued ───

#[test]
fn unanswered_item_past_timeout_releases_the_next_and_the_timed_out_item_stays_pending() {
    let policy = OperatorQueuePolicy {
        operator_queue_depth: 1,
        answer_timeout_secs: 120,
        ..OperatorQueuePolicy::default()
    };
    let mut queue = OperatorQueue::new(policy);
    queue.enqueue(item("first", 1, 0));

    let t0 = now();
    let opened = queue.next_deliverable(t0).expect("first item delivered");
    assert_eq!(opened.item_id, "first");

    // A higher-priority item arrives while "first" is open — must not
    // preempt it before the timeout.
    queue.enqueue(item("second", 100, 1));
    assert_eq!(
        queue.next_deliverable(t0 + chrono::Duration::seconds(60)),
        None
    );

    // Past the 120s timeout: "first" is released back to pending (never
    // dropped) and the highest-priority pending item is delivered next.
    let t1 = t0 + chrono::Duration::seconds(121);
    let next = queue.next_deliverable(t1).expect("next item delivered");
    assert_eq!(next.item_id, "second");

    // The queue does not permanently wedge: it produced a new delivery.
    // The timed-out item is still pending, not dropped.
    assert_eq!(queue.pending_count(), 1);
    assert!(!queue.answer("first"));
}

// ── ordering test: shuffled equal-priority input, deterministic result ──

#[test]
fn shuffled_equal_priority_input_resolves_to_one_deterministic_order() {
    // Five items, three of which tie on priority 5 and on enqueued_at —
    // the id tiebreak must resolve them the same way regardless of input
    // order.
    let make = |order: &[&str]| -> Vec<OperatorQueueItem> {
        let table: std::collections::HashMap<&str, (i32, i64)> = [
            ("m", (5, 0)),
            ("z", (5, 0)),
            ("a", (5, 0)),
            ("high", (10, 0)),
            ("low", (1, 0)),
        ]
        .into_iter()
        .collect();
        order
            .iter()
            .map(|id| {
                let (p, s) = table[id];
                item(id, p, s)
            })
            .collect()
    };

    let shuffles: [[&str; 5]; 4] = [
        ["m", "z", "a", "high", "low"],
        ["low", "high", "a", "z", "m"],
        ["z", "a", "m", "low", "high"],
        ["high", "low", "z", "m", "a"],
    ];

    let mut results: Vec<Vec<String>> = Vec::new();
    for shuffle in shuffles {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        for it in make(&shuffle) {
            queue.enqueue(it);
        }
        let mut delivery_order = Vec::new();
        while let Some(delivered) = queue.next_deliverable(now()) {
            delivery_order.push(delivered.item_id.clone());
            assert!(queue.answer(&delivered.item_id));
        }
        results.push(delivery_order);
    }

    // Every shuffle produced the exact same delivery order: highest
    // priority first ("high"), then the three priority-5 ties broken
    // lexicographically by item_id ("a", "m", "z"), then "low".
    let expected = vec!["high", "a", "m", "z", "low"];
    for result in &results {
        assert_eq!(result, &expected);
    }
    // Cross-check every result is identical to the first (redundant with
    // the above, but asserts the "one deterministic order" property
    // directly rather than only against a hand-computed expectation).
    for pair in results.windows(2) {
        assert_eq!(pair[0], pair[1]);
    }
}

// ── stale-item test: level predicate false -> skipped, next delivered ──

#[test]
fn item_whose_level_predicate_no_longer_holds_is_skipped_and_next_delivered() {
    let mut queue = OperatorQueue::new(OperatorQueuePolicy::default())
        .with_level_predicate(|it| it.item_id != "no-longer-blocked");
    // The stale item outranks the fresh one on priority, so without the
    // predicate it would be delivered first.
    queue.enqueue(item("no-longer-blocked", 999, 0));
    queue.enqueue(item("still-blocked", 1, 1));

    let delivered = queue
        .next_deliverable(now())
        .expect("the fresh item is delivered");
    assert_eq!(delivered.item_id, "still-blocked");
    // The stale item was dropped at selection time, not merely skipped
    // over.
    assert_eq!(queue.pending_count(), 0);
}
