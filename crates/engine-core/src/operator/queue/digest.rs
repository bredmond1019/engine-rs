//! Storm suppression and the digest tail (`EN.8.B` task 4).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: items arriving within a
//! configurable suppression window collapse into **one** delivery — N
//! simultaneous arrivals produce one message carrying the top item plus a
//! count, never N messages. The digest tail carries the low-priority
//! remainder the same way: counts plus the top item only, never the full
//! list, and it is **pulled on a schedule, never pushed on arrival** — the
//! caller decides when to ask for one; nothing in this module fires on its
//! own.
//!
//! **This is a separate mechanism from `bastion:BA.18.A`'s seed-before-emit,
//! and both are required.** Seed-before-emit (in bastion's own sink) stops
//! the *producer* inventing N edges on boot. Suppression here stops N
//! *legitimate* items — already correctly recorded, already correctly
//! enqueued — from becoming N operator-facing messages. Neither substitutes
//! for the other.
//!
//! Both the storm-collapse and the scheduled digest are the same shape
//! ([`QueueDigest`]) built by the same pure function ([`build_digest`]):
//! given a slice of items and `now`, no I/O, no clock reads beyond the
//! `now` argument. [`storm_digest`] is [`build_digest`] narrowed to the
//! items that arrived inside `suppression_window_secs` of `now` — collapsing
//! a burst into the single message the acceptance criteria require without
//! inventing a second digest type or a second JSON shape.

use chrono::{DateTime, Utc};

use super::item::{compare_items, OperatorQueueItem};

/// One digest delivery: the single highest-priority item plus how many
/// items it stands in for. Never carries the full tail — that is the whole
/// point of a digest (§7.5 Invariant 3: sixty approvals delivered at once is
/// a list nobody opens).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueDigest {
    /// The single highest-priority item among the items this digest
    /// summarizes, by [`compare_items`]'s order.
    pub top: OperatorQueueItem,
    /// Total number of items this digest summarizes, including `top`.
    pub total_count: usize,
    /// When this digest was produced.
    pub generated_at: DateTime<Utc>,
}

/// Build a digest over `items` as of `now` — the top item by
/// [`compare_items`] plus the total count. `None` when `items` is empty (no
/// digest to deliver).
///
/// Pure: takes a slice and a timestamp, reads nothing else, performs no I/O.
#[must_use]
pub fn build_digest(items: &[OperatorQueueItem], now: DateTime<Utc>) -> Option<QueueDigest> {
    if items.is_empty() {
        return None;
    }
    let top = items
        .iter()
        .min_by(|a, b| compare_items(a, b))
        .cloned()
        .expect("items is non-empty");
    Some(QueueDigest {
        top,
        total_count: items.len(),
        generated_at: now,
    })
}

/// Collapse a storm: of `items`, only those whose `enqueued_at` falls within
/// `suppression_window_secs` of `now` are folded into one [`QueueDigest`] —
/// N simultaneous (or near-simultaneous) arrivals produce one delivery
/// carrying the top item plus the count, never N separate messages.
///
/// A non-positive `suppression_window_secs` degrades to "only items enqueued
/// at exactly `now`", never to "everything" — a misconfigured knob must
/// narrow the collapse, not silently widen it into swallowing the whole
/// queue. `None` when nothing falls inside the window.
///
/// Pure: takes a slice, a window width, and a timestamp; no I/O, no clock
/// reads beyond the `now` argument.
#[must_use]
pub fn storm_digest(
    items: &[OperatorQueueItem],
    suppression_window_secs: i64,
    now: DateTime<Utc>,
) -> Option<QueueDigest> {
    let window = chrono::Duration::seconds(suppression_window_secs.max(0));
    let recent: Vec<OperatorQueueItem> = items
        .iter()
        .filter(|item| {
            let age = now.signed_duration_since(item.enqueued_at);
            // Only items enqueued at-or-before `now`, within the window.
            // (chrono::Duration comparisons handle a future `enqueued_at`
            // as a negative age, which is < Duration::zero() and so is
            // excluded — a clock-skewed item never gets swept in.)
            age >= chrono::Duration::zero() && age <= window
        })
        .cloned()
        .collect();
    build_digest(&recent, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::queue::item::ItemSource;
    use chrono::TimeZone;

    fn payload() -> crate::operator::OperatorPayload {
        crate::operator::OperatorPayload::new("gate-1", "summary", vec![])
    }

    fn item(id: &str, priority: i32, secs: i64) -> OperatorQueueItem {
        let ts = Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap();
        OperatorQueueItem::new(id, payload(), priority, ts, ItemSource::BlockedEdge)
    }

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    // ── build_digest ────────────────────────────────────────────────────

    #[test]
    fn build_digest_over_empty_slice_is_none() {
        assert_eq!(build_digest(&[], now()), None);
    }

    #[test]
    fn build_digest_carries_top_item_and_total_count() {
        let items = vec![item("a", 5, 0), item("b", 50, 0), item("c", 1, 0)];
        let digest = build_digest(&items, now()).expect("digest");
        assert_eq!(digest.top.item_id, "b");
        assert_eq!(digest.total_count, 3);
        assert_eq!(digest.generated_at, now());
    }

    #[test]
    fn build_digest_of_single_item_is_that_item_with_count_one() {
        let items = vec![item("solo", 1, 0)];
        let digest = build_digest(&items, now()).expect("digest");
        assert_eq!(digest.top.item_id, "solo");
        assert_eq!(digest.total_count, 1);
    }

    // ── storm_digest: the headline "N arrivals -> one delivery" behavior ─

    #[test]
    fn n_simultaneous_arrivals_collapse_into_one_digest() {
        let items: Vec<OperatorQueueItem> =
            (0..60).map(|i| item(&format!("item-{i}"), i, 0)).collect();
        let digest = storm_digest(&items, 60, now()).expect("one digest, not 60 messages");
        assert_eq!(digest.total_count, 60);
        // Priority 59 is the highest of 0..60.
        assert_eq!(digest.top.item_id, "item-59");
    }

    #[test]
    fn items_outside_the_window_are_excluded_from_the_storm_digest() {
        let items = vec![
            item("in-window", 1, 0),
            // Enqueued 200s before `now`; outside a 60s window.
            item("stale", 100, -200),
        ];
        let digest = storm_digest(&items, 60, now()).expect("digest");
        assert_eq!(digest.total_count, 1);
        assert_eq!(digest.top.item_id, "in-window");
    }

    #[test]
    fn storm_digest_of_empty_slice_is_none() {
        assert_eq!(storm_digest(&[], 60, now()), None);
    }

    #[test]
    fn nonpositive_window_narrows_rather_than_widens() {
        let items = vec![item("exact", 1, 0), item("later", 100, 5)];
        // A negative window clamps to zero: only items enqueued at exactly
        // `now` survive, never "everything".
        let digest = storm_digest(&items, -30, now()).expect("digest");
        assert_eq!(digest.total_count, 1);
        assert_eq!(digest.top.item_id, "exact");
    }

    #[test]
    fn future_enqueued_at_is_excluded_not_swept_in() {
        let items = vec![item("future", 1, 10)];
        assert_eq!(storm_digest(&items, 60, now()), None);
    }

    #[test]
    fn storm_digest_matches_build_digest_ordering() {
        // Both functions share `compare_items`'s order — a storm digest
        // must never pick a different "top" than a full digest over the
        // same filtered set would.
        let items = vec![item("a", 5, 0), item("b", 10, 0), item("c", 1, 0)];
        let storm = storm_digest(&items, 60, now()).expect("storm digest");
        let full = build_digest(&items, now()).expect("full digest");
        assert_eq!(storm.top, full.top);
    }
}
