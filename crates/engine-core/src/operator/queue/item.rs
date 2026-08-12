//! The operator queue item and its ordering (`EN.8.B` task 1).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: [`OperatorQueueItem`] wraps
//! the `EN.8.A` [`crate::operator::OperatorPayload`] with the fields the
//! queue itself needs — a stable id, an effective priority, an enqueue
//! timestamp, and the source that produced it.
//!
//! **`effective_priority` is supplied by the enqueuer, never computed
//! here.** Per the design call recorded in `tasks.md`: ordering comes from
//! "mev's reverse-topological effective-priority propagation" per the
//! roadmap, but per THE BOUNDARY TEST this crate does not read mev's state
//! or shell out to `mev`. The queue item *carries* a number; `mev` (or
//! whatever enqueues) is what computes it. Reverse this only with an
//! explicit decision file.
//!
//! This module is pure data plus a pure comparator: no I/O, no clock reads.

use chrono::{DateTime, Utc};
use std::cmp::Ordering;

use crate::operator::OperatorPayload;

/// What produced this queue item — distinguishes a blocked-edge item
/// (`bastion:BA.18.A`'s sink, read by `EN.8.B` task 2) from a gate-approval
/// item (an `EN.8.A` gate emitting directly).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    /// Originated from a `bastion:BA.18.A` blocked-edge sink record.
    BlockedEdge,
    /// Originated from an `EN.8.A` gate approval payload.
    GateApproval,
}

/// One item waiting in the operator queue: the `EN.8.A` payload plus the
/// bookkeeping the queue needs to order and deliver it.
///
/// Equality and hashing are derived from all fields — two items are the
/// same queue entry only if every field matches, including the payload
/// itself (a re-rendered payload with a changed digest is a different
/// item, matching `EN.8.A`'s digest-bound re-queue behavior).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OperatorQueueItem {
    /// Stable identifier for this queue entry — the tiebreak of last
    /// resort in [`compare_items`], and the key `answer()` (`EN.8.B` task
    /// 3) resolves against.
    pub item_id: String,
    /// The validated `EN.8.A` payload this item delivers.
    pub payload: OperatorPayload,
    /// Priority supplied by the enqueuer — higher sorts first. This crate
    /// never computes this value; see the module header.
    pub effective_priority: i32,
    /// When this item was enqueued — the second-order sort key (oldest
    /// first among equal priority), and the age a wedge/timeout check
    /// (`EN.8.B` task 3) reads against.
    pub enqueued_at: DateTime<Utc>,
    /// What produced this item.
    pub source: ItemSource,
}

impl OperatorQueueItem {
    /// Construct a queue item.
    pub fn new(
        item_id: impl Into<String>,
        payload: OperatorPayload,
        effective_priority: i32,
        enqueued_at: DateTime<Utc>,
        source: ItemSource,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            payload,
            effective_priority,
            enqueued_at,
            source,
        }
    }
}

/// Order two items for delivery: highest `effective_priority` first, then
/// oldest `enqueued_at` first, then `item_id` lexicographically ascending
/// as the final deterministic tiebreak.
///
/// This chain must be **total** — for any two *distinct* items (differing
/// in `item_id`, since `item_id` is meant to be a stable unique identifier)
/// this function must never return [`Ordering::Equal`]. `item_id` is the
/// last link in the chain precisely to guarantee that: priority can tie,
/// timestamps can tie (two items enqueued in the same tick), but two
/// distinct ids never compare equal under `Ord` for `String`. Without this
/// property, delivery order for equal-priority items would depend on
/// whatever incidental order they happened to arrive in the underlying
/// collection (e.g. a non-stable sort, or HashMap iteration), which makes
/// "with 60 pending items, exactly one is delivered, and it is the
/// highest-priority one" untestable whenever two of those 60 tie on
/// priority — the acceptance criterion silently degrades from "the highest
/// priority item" to "some highest-priority item, non-reproducibly chosen".
#[must_use]
pub fn compare_items(a: &OperatorQueueItem, b: &OperatorQueueItem) -> Ordering {
    b.effective_priority
        .cmp(&a.effective_priority)
        .then_with(|| a.enqueued_at.cmp(&b.enqueued_at))
        .then_with(|| a.item_id.cmp(&b.item_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::OperatorPayload;
    use chrono::TimeZone;

    fn payload() -> OperatorPayload {
        OperatorPayload::new("gate-1", "summary", vec![])
    }

    fn item(id: &str, priority: i32, secs: i64) -> OperatorQueueItem {
        let ts = Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap();
        OperatorQueueItem::new(id, payload(), priority, ts, ItemSource::BlockedEdge)
    }

    #[test]
    fn higher_priority_sorts_first() {
        let hi = item("a", 10, 0);
        let lo = item("b", 1, 0);
        assert_eq!(compare_items(&hi, &lo), Ordering::Less);
        assert_eq!(compare_items(&lo, &hi), Ordering::Greater);
    }

    #[test]
    fn equal_priority_older_sorts_first() {
        let older = item("a", 5, 0);
        let newer = item("b", 5, 100);
        assert_eq!(compare_items(&older, &newer), Ordering::Less);
    }

    #[test]
    fn equal_priority_equal_time_breaks_on_item_id() {
        let a = item("aaa", 5, 0);
        let b = item("bbb", 5, 0);
        assert_eq!(compare_items(&a, &b), Ordering::Less);
        assert_eq!(compare_items(&b, &a), Ordering::Greater);
    }

    #[test]
    fn ordering_is_total_for_distinct_items() {
        // Same priority, same timestamp, different id: must never tie.
        let a = item("x", 5, 0);
        let b = item("y", 5, 0);
        assert_ne!(compare_items(&a, &b), Ordering::Equal);
    }

    #[test]
    fn identical_item_compares_equal() {
        let a = item("same", 5, 0);
        let b = item("same", 5, 0);
        assert_eq!(compare_items(&a, &b), Ordering::Equal);
    }

    #[test]
    fn full_sort_is_deterministic_across_shuffles() {
        // Several hand-picked permutations of the same five items (no
        // `rand` dependency in this crate) — every permutation must sort
        // to the same final order.
        let base = |order: [&str; 5]| -> Vec<OperatorQueueItem> {
            let all = [
                ("a", 5, 0),
                ("b", 10, 5),
                ("c", 5, 0),
                ("d", 5, 0),
                ("e", 1, 0),
            ];
            order
                .iter()
                .map(|id| {
                    let (_, p, s) = all.iter().find(|(i, _, _)| i == id).unwrap();
                    item(id, *p, *s)
                })
                .collect()
        };
        let permutations: [[&str; 5]; 4] = [
            ["a", "b", "c", "d", "e"],
            ["e", "d", "c", "b", "a"],
            ["c", "a", "e", "b", "d"],
            ["b", "e", "d", "a", "c"],
        ];
        for perm in permutations {
            let mut items = base(perm);
            items.sort_by(compare_items);
            let ids: Vec<&str> = items.iter().map(|i| i.item_id.as_str()).collect();
            assert_eq!(ids, vec!["b", "a", "c", "d", "e"]);
        }
    }
}
