//! `EN.9.G` task 3 — integration coverage of the Blocked-edge bridge
//! receiver's central rule: the edge only ever *triggers*, and the run it
//! starts must re-evaluate a **level** predicate (`current state ==
//! Blocked`) before doing anything, never trust the trigger's own payload.
//!
//! `crates/engine-serve/src/blocked_bridge.rs` already ships a `#[cfg(test)]`
//! unit suite covering the same seams from inside that crate. This module
//! exercises the identical race from the *other* side of the dependency
//! edge — through `engine_serve::blocked_bridge`'s public API exactly as an
//! external caller (e.g. the run engine itself) would reach it — which is
//! possible here because `engine-core`'s `Cargo.toml` already carries
//! `engine-serve` as a dev-dependency (several sibling modules in this same
//! binary, e.g. `content_pipeline_e2e.rs` and `policy_dispatch_e2e.rs`,
//! already call `engine_serve::dispatch`/`engine_serve::workflows` the same
//! way). That means the task spec's "use engine-serve's in-module suite
//! instead" fallback — for when the receiver's seams are *unreachable* from
//! `engine-core`'s integration binary — does not apply here; this is a
//! genuine integration test, and it lives in this single binary per
//! standing rule 8 (no new `tests/*.rs` file, just this module plus one
//! `mod blocked_bridge;` line in `tests/it/main.rs`).
//!
//! Every test below drives a fixed, injected clock — never `std::thread::
//! sleep` — to reproduce the one-tick race deterministically and without
//! flake risk.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use chrono::{DateTime, TimeZone, Utc};
use engine_serve::blocked_bridge::{
    BlockedBridgeReceiver, BlockedEdgeTrigger, LevelSource, Notifier, TriggerOutcome,
};

use engine_core::operator::queue::{BlockedEdgeState, OperatorQueueItem, OperatorQueuePolicy};

/// An in-memory [`LevelSource`] double — no real sink file. Tests set the
/// current state per session directly and the receiver re-reads it at
/// selection time, never trusting the trigger's own payload.
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

/// A [`Notifier`] double that records every call — no real notification
/// transport in the gated suite.
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

/// One fixed instant used across a whole test — the "one 2s tick" the
/// producing poller (`bastion:BA.18.A`) would have fired both triggers
/// within is simulated by reusing this same `now` for every `receive`
/// call in a test, never by sleeping past it.
fn tick() -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).unwrap()
}

fn receiver(
    level_source: Arc<FakeLevelSource>,
    notifier: Arc<RecordingNotifier>,
) -> BlockedBridgeReceiver {
    BlockedBridgeReceiver::new(OperatorQueuePolicy::default(), level_source, notifier)
}

/// The headline race this task exists to cover: a session transitions to
/// `Blocked` (the poller fires an edge), then resolves back to `Idle`
/// *within the same 2s tick* the receiver processes the trigger in. The
/// receiver must re-evaluate the level predicate at receive time — since
/// the session is no longer `Blocked` by then, it must exit without
/// notifying, never act on the trigger's own (now-stale) implication.
#[test]
fn fire_then_resolve_within_one_tick_exits_without_notifying() {
    let level_source = Arc::new(FakeLevelSource::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

    // The poller observed the transition into Blocked and fired the edge
    // (that observation is what minted this trigger) — but by the time
    // this receiver actually looks, within the same tick, the session has
    // already resolved back to Idle.
    level_source.set("sess-race", BlockedEdgeState::Idle);

    let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-race", "host-a"), tick());

    assert_eq!(
        outcome,
        TriggerOutcome::NoDelivery,
        "a trigger whose session has already resolved must exit without notifying"
    );
    assert_eq!(
        notifier.call_count(),
        0,
        "a stale trigger must never reach the notifier"
    );
}

/// The companion happy path: a trigger whose session still reads `Blocked`
/// at receive time is live, not stale, and must be delivered.
#[test]
fn a_live_trigger_still_blocked_at_receive_time_notifies() {
    let level_source = Arc::new(FakeLevelSource::new());
    level_source.set("sess-live", BlockedEdgeState::Blocked);
    let notifier = Arc::new(RecordingNotifier::new());
    let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

    let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-live", "host-a"), tick());

    assert!(matches!(outcome, TriggerOutcome::Notified(_)));
    assert_eq!(notifier.call_count(), 1);
}

/// The full resolve-and-re-block variant: within one tick the session goes
/// Blocked -> Idle -> Blocked again, producing two edges (two triggers).
/// Both triggers are processed at the same simulated `now` (no real sleep
/// between them). The receiver must notify exactly once — driven by the
/// live level read at the moment each trigger is processed, not by
/// counting edges.
#[test]
fn resolve_and_reblock_within_one_tick_notifies_exactly_once() {
    let level_source = Arc::new(FakeLevelSource::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

    // By the time the first trigger for this tick is processed, the
    // session already reads Blocked again (the "re-block" half of the
    // race already happened from the level source's point of view).
    level_source.set("sess-reblock", BlockedEdgeState::Blocked);

    let first = bridge.receive(BlockedEdgeTrigger::new("sess-reblock", "host-a"), tick());
    let second = bridge.receive(BlockedEdgeTrigger::new("sess-reblock", "host-a"), tick());

    assert!(
        matches!(first, TriggerOutcome::Notified(_)),
        "the first trigger to find the session live opens the queue's one slot"
    );
    assert_eq!(
        second,
        TriggerOutcome::NoDelivery,
        "a second trigger for the same still-open item must not double-notify"
    );
    assert_eq!(
        notifier.call_count(),
        1,
        "exactly one notification for the tick, driven by the level read — not one per edge"
    );
}

/// A trigger for a session the level source has never observed at all is
/// treated the same as "not currently Blocked" — nothing to deliver.
#[test]
fn a_trigger_for_a_never_observed_session_exits_without_notifying() {
    let level_source = Arc::new(FakeLevelSource::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let bridge = receiver(Arc::clone(&level_source), Arc::clone(&notifier));

    let outcome = bridge.receive(BlockedEdgeTrigger::new("sess-unknown", "host-a"), tick());

    assert_eq!(outcome, TriggerOutcome::NoDelivery);
    assert_eq!(notifier.call_count(), 0);
}
