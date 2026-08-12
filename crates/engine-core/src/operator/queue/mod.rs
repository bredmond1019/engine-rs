//! The operator queue (`EN.8.B`).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: at most one open
//! operator-facing item at a time, ordered by effective priority rather
//! than arrival, with a digest tail carrying the low-priority remainder as
//! counts and mandatory storm suppression so a restart burst produces one
//! message, not N.
//!
//! Task 1 (plus [`item`]) defines the item shape and its ordering only —
//! pure data and a pure comparator, no I/O, no clock reads. Task 2
//! ([`source`]) adds the durable source reader over the `bastion:BA.18.A`
//! blocked-edge sink. Task 3 (this module's [`OperatorQueue`], plus
//! [`policy`]) adds the depth-limited delivery queue itself: at most
//! `policy.operator_queue_depth` open items at once, released on `answer`
//! or on an unanswered timeout (re-queued, never dropped), with stale items
//! (whose level predicate no longer holds) dropped at selection time. Task
//! 4 adds the digest/storm-suppression tail on top.

use chrono::{DateTime, Utc};

pub mod item;
pub mod policy;
pub mod source;

pub use item::{compare_items, ItemSource, OperatorQueueItem};
pub use policy::{OperatorQueuePolicy, PartialOperatorQueuePolicy};
#[cfg(test)]
pub use source::InMemoryQueueSource;
pub use source::{
    default_sink_path, BlockedEdgeRecord, BlockedEdgeSource, BlockedEdgeState, PendingBlockedEdge,
    QueueSource,
};

/// One item currently open — delivered to the operator, awaiting an
/// `answer` or an unanswered-timeout release.
#[derive(Debug, Clone)]
struct OpenItem {
    item: OperatorQueueItem,
    opened_at: DateTime<Utc>,
}

/// The depth-limited operator queue (`EN.8.B` task 3).
///
/// Holds every pending [`OperatorQueueItem`] plus the (at most
/// `policy.operator_queue_depth`, default 1) currently-open items, and
/// answers [`OperatorQueue::next_deliverable`] — `None` while the queue is
/// at depth, otherwise the highest-priority pending item by
/// [`item::compare_items`]'s order.
///
/// The level predicate — whether an item's underlying condition (e.g. a
/// blocked-edge item's session) still holds — is supplied by the caller via
/// [`OperatorQueue::with_level_predicate`] and re-evaluated at *selection*
/// time (inside `next_deliverable`), never at `enqueue` time, per
/// `bastion:BA.18.A`'s notes: an edge record is a trigger, not a truth. The
/// default predicate accepts every item, matching a gate-approval item that
/// carries no external "still true" condition to re-check.
pub struct OperatorQueue {
    pending: Vec<OperatorQueueItem>,
    open: Vec<OpenItem>,
    policy: OperatorQueuePolicy,
    level_predicate: Box<dyn Fn(&OperatorQueueItem) -> bool + Send + Sync>,
}

impl std::fmt::Debug for OperatorQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorQueue")
            .field("pending", &self.pending)
            .field("open", &self.open)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl OperatorQueue {
    /// Construct an empty queue under `policy`, with a level predicate that
    /// accepts every item (override via [`Self::with_level_predicate`]).
    #[must_use]
    pub fn new(policy: OperatorQueuePolicy) -> Self {
        Self {
            pending: Vec::new(),
            open: Vec::new(),
            policy,
            level_predicate: Box::new(|_| true),
        }
    }

    /// Override the level predicate: a function returning `false` for an
    /// item means its underlying condition no longer holds and it should be
    /// dropped rather than delivered. Evaluated at selection time inside
    /// [`Self::next_deliverable`].
    #[must_use]
    pub fn with_level_predicate(
        mut self,
        predicate: impl Fn(&OperatorQueueItem) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.level_predicate = Box::new(predicate);
        self
    }

    /// The resolved policy this queue is running under.
    #[must_use]
    pub fn policy(&self) -> &OperatorQueuePolicy {
        &self.policy
    }

    /// Serializes the resolved policy this queue is running under, so a
    /// caller can stamp it into `ctx.nodes` (`RunTelemetry`/
    /// `PolicyAggregate` per `CLAUDE.md` standing rule 6) and attribute
    /// observed behavior to the setting that caused it. Never includes a
    /// `cost_usd` key — see `workflow.rs:552-554`.
    #[must_use]
    pub fn policy_state(&self) -> serde_json::Value {
        serde_json::to_value(self.policy).unwrap_or_else(|_| serde_json::json!({}))
    }

    /// Number of items currently open (delivered, awaiting answer/timeout).
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Number of items waiting to be delivered (excludes open items).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Enqueue an item — visible on demand via `pending_count`/
    /// `next_deliverable`, never pushed.
    pub fn enqueue(&mut self, item: OperatorQueueItem) {
        self.pending.push(item);
    }

    /// Release every open item whose answer timeout has elapsed as of
    /// `now`, re-queuing it (never dropping it) so the queue cannot wedge
    /// permanently on one unanswered item.
    fn release_timed_out(&mut self, now: DateTime<Utc>) {
        let timeout = chrono::Duration::seconds(self.policy.answer_timeout_secs.max(0));
        let mut still_open = Vec::with_capacity(self.open.len());
        for open in self.open.drain(..) {
            if now.signed_duration_since(open.opened_at) >= timeout {
                self.pending.push(open.item);
            } else {
                still_open.push(open);
            }
        }
        self.open = still_open;
    }

    /// Drop every pending item whose level predicate no longer holds —
    /// evaluated here, at selection time, never at [`Self::enqueue`].
    fn drop_stale(&mut self) {
        let predicate = &self.level_predicate;
        self.pending.retain(|item| predicate(item));
    }

    /// The next item to deliver, or `None` while the queue is at depth.
    ///
    /// In order: releases any open item whose timeout has elapsed
    /// (re-queued), drops pending items whose level predicate no longer
    /// holds, then — if there is room under `policy.operator_queue_depth` —
    /// opens and returns the highest-priority remaining item
    /// ([`item::compare_items`] order).
    pub fn next_deliverable(&mut self, now: DateTime<Utc>) -> Option<OperatorQueueItem> {
        self.release_timed_out(now);
        self.drop_stale();

        let depth = self.policy.operator_queue_depth as usize;
        if self.open.len() >= depth || self.pending.is_empty() {
            return None;
        }

        self.pending.sort_by(compare_items);
        let item = self.pending.remove(0);
        self.open.push(OpenItem {
            item: item.clone(),
            opened_at: now,
        });
        Some(item)
    }

    /// Answer the open item identified by `item_id`, clearing it so the next
    /// [`Self::next_deliverable`] call can deliver another. Returns `true`
    /// if an open item with that id existed.
    pub fn answer(&mut self, item_id: &str) -> bool {
        let before = self.open.len();
        self.open.retain(|open| open.item.item_id != item_id);
        self.open.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn payload() -> crate::operator::OperatorPayload {
        crate::operator::OperatorPayload::new("gate-1", "summary", vec![])
    }

    fn item(id: &str, priority: i32, secs: i64) -> OperatorQueueItem {
        let ts = Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap();
        OperatorQueueItem::new(id, payload(), priority, ts, ItemSource::BlockedEdge)
    }

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_100_000, 0).unwrap()
    }

    #[test]
    fn empty_queue_delivers_nothing() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        assert_eq!(queue.next_deliverable(now()), None);
    }

    #[test]
    fn single_item_is_delivered_and_marked_open() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        queue.enqueue(item("a", 5, 0));
        let delivered = queue.next_deliverable(now());
        assert_eq!(delivered.as_ref().map(|i| i.item_id.as_str()), Some("a"));
        assert_eq!(queue.open_count(), 1);
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn with_one_open_item_next_deliverable_is_none_regardless_of_pending_count() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        for i in 0..60 {
            queue.enqueue(item(&format!("item-{i}"), i, i as i64));
        }
        let first = queue.next_deliverable(now());
        assert!(first.is_some());
        // 59 items still pending, but depth 1 is already occupied.
        assert_eq!(queue.pending_count(), 59);
        assert_eq!(queue.next_deliverable(now()), None);
    }

    #[test]
    fn sixty_pending_items_deliver_the_highest_priority_one() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        for i in 0..60 {
            queue.enqueue(item(&format!("item-{i}"), i, i as i64));
        }
        let delivered = queue.next_deliverable(now()).expect("one item delivered");
        // Priority 59 is the highest of 0..60.
        assert_eq!(delivered.item_id, "item-59");
    }

    #[test]
    fn answering_the_open_item_makes_exactly_one_more_deliverable() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        queue.enqueue(item("a", 10, 0));
        queue.enqueue(item("b", 5, 0));
        let first = queue.next_deliverable(now()).expect("first delivered");
        assert_eq!(first.item_id, "a");
        assert_eq!(queue.next_deliverable(now()), None);

        assert!(queue.answer(&first.item_id));
        let second = queue.next_deliverable(now()).expect("second delivered");
        assert_eq!(second.item_id, "b");
        // Nothing further is deliverable — depth 1 is occupied again.
        assert_eq!(queue.next_deliverable(now()), None);
    }

    #[test]
    fn answer_with_unknown_item_id_returns_false() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        assert!(!queue.answer("nonexistent"));
    }

    #[test]
    fn unanswered_item_past_timeout_releases_the_next_and_is_requeued() {
        let policy = OperatorQueuePolicy {
            operator_queue_depth: 1,
            answer_timeout_secs: 60,
        };
        let mut queue = OperatorQueue::new(policy);
        // "a" is the only item pending, so it is delivered first regardless
        // of priority.
        queue.enqueue(item("a", 1, 0));

        let t0 = now();
        let first = queue.next_deliverable(t0).expect("first delivered");
        assert_eq!(first.item_id, "a");
        // Still inside the timeout: nothing new is deliverable, even though
        // "b" has since arrived and outranks "a" on priority.
        queue.enqueue(item("b", 100, 0));
        assert_eq!(
            queue.next_deliverable(t0 + chrono::Duration::seconds(30)),
            None
        );

        // Past the timeout: "a" is released back to pending (not dropped),
        // and the next delivery is the highest-priority pending item — "b",
        // never "a" again while a higher-priority item is waiting.
        let t1 = t0 + chrono::Duration::seconds(61);
        let second = queue.next_deliverable(t1).expect("second delivered");
        assert_eq!(second.item_id, "b");
        // "a" is still pending, not dropped.
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn item_whose_level_predicate_no_longer_holds_is_never_delivered() {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default())
            .with_level_predicate(|item| item.item_id != "stale");
        queue.enqueue(item("stale", 100, 0));
        queue.enqueue(item("fresh", 1, 0));

        let delivered = queue.next_deliverable(now()).expect("fresh item delivered");
        assert_eq!(delivered.item_id, "fresh");
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn depth_greater_than_one_allows_multiple_open_items() {
        let policy = OperatorQueuePolicy {
            operator_queue_depth: 2,
            answer_timeout_secs: 900,
        };
        let mut queue = OperatorQueue::new(policy);
        queue.enqueue(item("a", 10, 0));
        queue.enqueue(item("b", 5, 0));
        queue.enqueue(item("c", 1, 0));

        assert!(queue.next_deliverable(now()).is_some());
        assert!(queue.next_deliverable(now()).is_some());
        assert_eq!(queue.open_count(), 2);
        // Depth is exhausted.
        assert_eq!(queue.next_deliverable(now()), None);
    }

    #[test]
    fn policy_state_serializes_the_resolved_policy() {
        let queue = OperatorQueue::new(OperatorQueuePolicy::default());
        let state = queue.policy_state();
        assert_eq!(state["operator_queue_depth"], 1);
        assert_eq!(state["answer_timeout_secs"], 900);
        assert!(state.get("cost_usd").is_none());
    }
}
