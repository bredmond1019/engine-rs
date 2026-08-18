//! The receiving half of the Blocked-edge bridge (`EN.9.G` task 2).
//!
//! The producing half is `bastion:BA.18.A`'s always-on poller (closed, out
//! of scope here) — it watches tmux sessions and appends a
//! `BlockedEdgeRecord` to its sink whenever a session transitions to (or
//! out of) `Blocked`. This module is the other end: it receives one
//! **trigger** naming a session and decides whether to deliver an item
//! into `EN.8.B`'s [`OperatorQueue`] (closed — this module wires a caller
//! into it, it does not reimplement it).
//!
//! # THE CENTRAL RULE
//!
//! The edge may only ever **trigger**. It never carries a truth this
//! receiver is allowed to trust. [`BlockedEdgeTrigger`] deliberately has
//! no `to`/state field — on receipt, [`BlockedBridgeReceiver::receive`]
//! re-reads the session's **current** state through the injected
//! [`LevelSource`] and re-evaluates the level predicate `current state ==
//! Blocked` at that moment, inside [`OperatorQueue::next_deliverable`]'s
//! own selection-time `drop_stale` (`EN.8.B` task 3's existing mechanism —
//! this module supplies the live predicate, it does not reimplement
//! selection-time staleness).
//!
//! Why this is not pedantry: an edge that fires while nothing is
//! listening, or a prompt that resolves and re-blocks inside one 2s poll
//! tick, is lost forever if the receiver ever trusts the trigger's own
//! payload. Level-on-receive is the entire difference between an
//! edge-triggered surface that is correct and one that is lossy. A
//! trigger whose session has already resolved by the time this receiver
//! looks exits **without notifying** — not warn-and-notify, not
//! notify-with-a-caveat — because the operator surface is what consumes
//! this, and a spurious approval request trains the operator to ignore
//! it.
//!
//! # Exactly-once under a same-tick resolve-and-re-block
//!
//! `EN.8.B`'s [`OperatorQueuePolicy::operator_queue_depth`] defaults to
//! `1` (§7.5 Invariant 3: at most one open operator-facing item at a
//! time). Two triggers for the same session arriving within one tick
//! therefore produce at most one notification for free: the first
//! trigger that finds the session still `Blocked` opens the queue's one
//! delivery slot and notifies; a second trigger for the same session,
//! processed while that slot is still open, finds `next_deliverable`
//! returns `None` (queue at depth) and exits without notifying. Both
//! outcomes are driven by the same live level read, never by trusting
//! either trigger's own claim.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use engine_core::operator::queue::{
    BlockedEdgeState, ItemSource, OperatorQueue, OperatorQueueItem, OperatorQueuePolicy,
};
use engine_core::operator::{OperatorPayload, OperatorResponseOption};

/// Prefix for the stable `item_id` this receiver derives from a trigger's
/// `session` — see [`item_id_for`]/[`session_from_item_id`]. Two triggers
/// for the same session always resolve to the same `item_id`, which is
/// what lets the queue's own answer/depth bookkeeping treat repeated
/// triggers for one session as the same logical item rather than an
/// unbounded pile of distinct ones.
const ITEM_ID_PREFIX: &str = "blocked-edge:";

fn item_id_for(session: &str) -> String {
    format!("{ITEM_ID_PREFIX}{session}")
}

/// Recover the session an `item_id` produced by [`item_id_for`] names.
/// `None` for any id this receiver did not mint (e.g. a `GateApproval`
/// item sharing the same queue).
fn session_from_item_id(item_id: &str) -> Option<&str> {
    item_id.strip_prefix(ITEM_ID_PREFIX)
}

/// The seam this receiver re-reads at **selection time**, never trusted
/// from the trigger itself — "is `session` `Blocked` right now". The real
/// implementation wraps `engine_core::operator::queue::BlockedEdgeSource`
/// (the `bastion:BA.18.A` sink reader `EN.8.B` task 2 already ships),
/// resolving the *latest* record for `session`. Tests substitute an
/// in-memory double so the gated suite never depends on wall-clock sleeps
/// or a real sink file.
pub trait LevelSource: Send + Sync {
    /// The session's current state, or `None` if this source has never
    /// observed the session at all (treated as "not currently Blocked" —
    /// there is nothing to deliver an item about).
    fn current_state(&self, session: &str) -> Option<BlockedEdgeState>;
}

/// Where a delivered item goes once the level predicate confirms it is
/// live. Injectable so a test can capture calls instead of exercising a
/// real notification transport (email/webhook/etc., all out of this
/// block's scope — see the `EN.8.D` boundary).
pub trait Notifier: Send + Sync {
    fn notify(&self, item: &OperatorQueueItem);
}

/// One trigger: the edge only ever triggers, so this carries identity
/// only. **No state field on purpose** — see the module's THE CENTRAL
/// RULE. `effective_priority` is supplied by the caller (mirroring
/// `OperatorQueueItem`'s own "the enqueuer computes it, this module does
/// not" convention — `EN.8.B` task 1's module header).
#[derive(Debug, Clone)]
pub struct BlockedEdgeTrigger {
    pub session: String,
    pub host: String,
    pub effective_priority: i32,
}

impl BlockedEdgeTrigger {
    #[must_use]
    pub fn new(session: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            host: host.into(),
            effective_priority: 0,
        }
    }

    #[must_use]
    pub fn with_priority(mut self, effective_priority: i32) -> Self {
        self.effective_priority = effective_priority;
        self
    }
}

/// What a [`BlockedBridgeReceiver::receive`] call did.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerOutcome {
    /// The level predicate still read `Blocked` at selection time, the
    /// item was delivered into the queue's one open slot, and the
    /// [`Notifier`] was called.
    Notified(OperatorQueueItem),
    /// No notification happened this call — either the trigger was
    /// stale (the level predicate no longer reads `Blocked`) or the
    /// queue's delivery slot was already occupied by another open item.
    /// Both are "exit without notifying"; callers that must distinguish
    /// staleness specifically should query [`LevelSource`] directly.
    NoDelivery,
}

/// The receiving half of the Blocked-edge bridge.
///
/// Wraps one `EN.8.B` [`OperatorQueue`] whose level predicate is wired,
/// once at construction, to re-read the injected [`LevelSource`] — so
/// every [`OperatorQueue::next_deliverable`] call this receiver makes
/// re-evaluates `current state == Blocked` at that exact moment, never at
/// enqueue time.
pub struct BlockedBridgeReceiver {
    queue: Mutex<OperatorQueue>,
    notifier: Arc<dyn Notifier>,
}

impl BlockedBridgeReceiver {
    /// Construct a receiver under `policy` (`operator_queue_depth`
    /// defaults to 1 per `EN.8.B` task 3 — see the module docs on why
    /// that alone gives exactly-once delivery for a same-tick
    /// resolve-and-re-block), reading liveness from `level_source` and
    /// delivering confirmed items to `notifier`.
    #[must_use]
    pub fn new(
        policy: OperatorQueuePolicy,
        level_source: Arc<dyn LevelSource>,
        notifier: Arc<dyn Notifier>,
    ) -> Self {
        let predicate_source = Arc::clone(&level_source);
        let queue = OperatorQueue::new(policy).with_level_predicate(move |item| {
            match session_from_item_id(&item.item_id) {
                Some(session) => {
                    predicate_source.current_state(session) == Some(BlockedEdgeState::Blocked)
                }
                // An item this receiver did not mint (e.g. a
                // `GateApproval` item sharing the queue) carries no
                // blocked-edge session to re-check — never this
                // receiver's call to drop it.
                None => true,
            }
        });
        Self {
            queue: Mutex::new(queue),
            notifier,
        }
    }

    /// Receive one trigger: build the queue item, enqueue it, then
    /// immediately ask the queue for the next deliverable at `now` — which
    /// re-runs the live level predicate this receiver was constructed
    /// with. Delivers and notifies on a live `Blocked` read; exits
    /// without notifying on anything else (stale trigger, or the queue's
    /// one delivery slot already open).
    pub fn receive(&self, trigger: BlockedEdgeTrigger, now: DateTime<Utc>) -> TriggerOutcome {
        let item_id = item_id_for(&trigger.session);
        let payload = OperatorPayload::new(
            item_id.clone(),
            format!(
                "Session `{}` on `{}` is blocked awaiting operator input.",
                trigger.session, trigger.host
            ),
            vec![OperatorResponseOption::new("acknowledge", "Acknowledge")],
        );
        let item = OperatorQueueItem::new(
            item_id.clone(),
            payload,
            trigger.effective_priority,
            now,
            ItemSource::BlockedEdge,
        );

        let delivered = {
            let mut queue = self.queue.lock().expect("blocked-bridge queue poisoned");
            queue.enqueue(item);
            queue.next_deliverable(now)
        };

        match delivered {
            Some(delivered) if delivered.item_id == item_id => {
                self.notifier.notify(&delivered);
                TriggerOutcome::Notified(delivered)
            }
            _ => TriggerOutcome::NoDelivery,
        }
    }

    /// Number of items currently pending delivery (excludes any open
    /// item). Exposed for tests/observability, not otherwise consumed by
    /// this module.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.queue
            .lock()
            .expect("blocked-bridge queue poisoned")
            .pending_count()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// An in-memory [`LevelSource`] double — no real sink file, no real
    /// clock. Tests set/mutate the current state per session directly.
    struct FakeLevelSource {
        states: StdMutex<HashMap<String, BlockedEdgeState>>,
    }

    impl FakeLevelSource {
        fn new() -> Self {
            Self {
                states: StdMutex::new(HashMap::new()),
            }
        }

        fn set(&self, session: &str, state: BlockedEdgeState) {
            self.states
                .lock()
                .unwrap()
                .insert(session.to_string(), state);
        }
    }

    impl LevelSource for FakeLevelSource {
        fn current_state(&self, session: &str) -> Option<BlockedEdgeState> {
            self.states.lock().unwrap().get(session).copied()
        }
    }

    /// A [`Notifier`] double that records every call — no real
    /// notification transport in the gated suite.
    struct RecordingNotifier {
        calls: StdMutex<Vec<OperatorQueueItem>>,
    }

    impl RecordingNotifier {
        fn new() -> Self {
            Self {
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Notifier for RecordingNotifier {
        fn notify(&self, item: &OperatorQueueItem) {
            self.calls.lock().unwrap().push(item.clone());
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    fn receiver(
        level_source: Arc<FakeLevelSource>,
        notifier: Arc<RecordingNotifier>,
    ) -> BlockedBridgeReceiver {
        BlockedBridgeReceiver::new(OperatorQueuePolicy::default(), level_source, notifier)
    }

    #[test]
    fn a_trigger_whose_session_is_still_blocked_notifies() {
        let level_source = Arc::new(FakeLevelSource::new());
        level_source.set("sess-1", BlockedEdgeState::Blocked);
        let notifier = Arc::new(RecordingNotifier::new());
        let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

        let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-1", "host-a"), now());

        assert!(matches!(outcome, TriggerOutcome::Notified(_)));
        assert_eq!(notifier.call_count(), 1);
    }

    #[test]
    fn a_stale_trigger_whose_session_already_resolved_exits_without_notifying() {
        let level_source = Arc::new(FakeLevelSource::new());
        // The session is Idle by the time the receiver looks — the
        // trigger fired, but the block already resolved.
        level_source.set("sess-2", BlockedEdgeState::Idle);
        let notifier = Arc::new(RecordingNotifier::new());
        let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

        let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-2", "host-a"), now());

        assert_eq!(outcome, TriggerOutcome::NoDelivery);
        assert_eq!(
            notifier.call_count(),
            0,
            "a stale trigger must never notify"
        );
    }

    #[test]
    fn a_trigger_for_a_session_the_level_source_has_never_seen_exits_without_notifying() {
        let level_source = Arc::new(FakeLevelSource::new());
        let notifier = Arc::new(RecordingNotifier::new());
        let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

        // "sess-unknown" was never `set` on the level source at all.
        let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-unknown", "host-a"), now());

        assert_eq!(outcome, TriggerOutcome::NoDelivery);
        assert_eq!(notifier.call_count(), 0);
    }

    #[test]
    fn resolve_and_reblock_within_one_tick_notifies_exactly_once() {
        let level_source = Arc::new(FakeLevelSource::new());
        let notifier = Arc::new(RecordingNotifier::new());
        let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

        // Simulate the poller's fire-then-resolve-then-reblock sequence
        // landing inside one 2s tick: by the time either trigger is
        // *processed*, the session's live state is Blocked again (the
        // "reblock" half of the race). No real sleep — the tick is
        // simulated by two `receive` calls at the same `now`.
        level_source.set("sess-3", BlockedEdgeState::Blocked);

        let first = bridge.receive(BlockedEdgeTrigger::new("sess-3", "host-a"), now());
        let second = bridge.receive(BlockedEdgeTrigger::new("sess-3", "host-a"), now());

        assert!(matches!(first, TriggerOutcome::Notified(_)));
        assert_eq!(second, TriggerOutcome::NoDelivery);
        assert_eq!(
            notifier.call_count(),
            1,
            "exactly one notification, driven by the level read — not one per edge"
        );
    }

    #[test]
    fn a_trigger_that_resolves_between_two_receives_does_not_notify_the_second_time() {
        let level_source = Arc::new(FakeLevelSource::new());
        let notifier = Arc::new(RecordingNotifier::new());
        let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

        level_source.set("sess-4", BlockedEdgeState::Blocked);
        let first = bridge.receive(BlockedEdgeTrigger::new("sess-4", "host-a"), now());
        assert!(matches!(first, TriggerOutcome::Notified(_)));

        // The operator answers, freeing the delivery slot; the session
        // then resolves and a second (stale-by-the-time-it's-checked)
        // trigger arrives.
        bridge.queue.lock().unwrap().answer(&item_id_for("sess-4"));
        level_source.set("sess-4", BlockedEdgeState::Idle);

        let second = bridge.receive(BlockedEdgeTrigger::new("sess-4", "host-a"), now());
        assert_eq!(second, TriggerOutcome::NoDelivery);
        assert_eq!(notifier.call_count(), 1);
    }

    #[test]
    fn item_id_round_trips_session_through_the_prefix() {
        assert_eq!(item_id_for("my-session"), "blocked-edge:my-session");
        assert_eq!(
            session_from_item_id("blocked-edge:my-session"),
            Some("my-session")
        );
        assert_eq!(session_from_item_id("gate-approval-1"), None);
    }
}
