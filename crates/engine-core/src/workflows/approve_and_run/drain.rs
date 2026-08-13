//! The drain — pending-harvest records into the depth-limited operator
//! queue (`EN.8.D` task 3).
//!
//! [`drain`] takes a batch of [`PendingHarvestRecord`]s and an
//! [`OperatorQueue`], renders each record via [`render_and_validate`] (task
//! 1), and enqueues one [`OperatorQueueItem`] per conforming record —
//! priority from [`ApproveAndRunPolicy::harvest_item_priority`],
//! [`ItemSource::GateApproval`] (a pending-harvest record is a gate emitting
//! directly, not a `bastion:BA.18.A` blocked-edge sink read), `enqueued_at`
//! supplied by the caller so this module never reads a clock.
//!
//! A record that fails validation does **not** enqueue on the notification
//! path. It is reported back as routed to
//! [`OperatorChannel::session`]`(policy.session_fallback_slug)` — never
//! silently dropped, never degraded onto `notification` (the type-level
//! guarantee from `EN.8.A`: only a [`ValidatedOperatorPayload`] may reach
//! `notification`, so there is no way to force a non-conforming record onto
//! it even by accident).
//!
//! The pass is bounded by [`ApproveAndRunPolicy::drain_batch_max`] —
//! [`DrainReport`] reports how many records were considered versus skipped
//! so a truncated pass is visible rather than looking complete.
//!
//! Delivery uses [`OperatorQueue::next_deliverable`] unchanged — this module
//! does not reimplement depth limiting or ordering. The drain's job ends at
//! enqueue plus exactly one `next_deliverable` call, whose result is
//! reported on [`DrainReport::delivered`].

use chrono::{DateTime, Utc};

use crate::operator::queue::{ItemSource, OperatorQueue, OperatorQueueItem};
use crate::operator::{OperatorChannel, OperatorPayloadLimits, OperatorValidationError};

use super::policy::ApproveAndRunPolicy;
use super::render::{gate_id_for, render_and_validate, PendingHarvestRecord};

/// A pending-harvest record that could not be reduced to a conforming
/// [`crate::operator::OperatorPayload`] under the declared limits — routed
/// to `session-<slug>` instead of `notification`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRoutedRecord {
    /// The record's `artifact_id`, so a caller can locate it in whatever
    /// store holds the pending-harvest record.
    pub artifact_id: String,
    /// The channel this record was routed to — always
    /// `OperatorChannel::Session { slug: policy.session_fallback_slug }`
    /// for a value produced by [`drain`].
    pub channel: OperatorChannel,
    /// Why `render_and_validate` rejected this record.
    pub error: OperatorValidationError,
}

/// The outcome of one [`drain`] pass.
#[derive(Debug, Clone, PartialEq)]
pub struct DrainReport {
    /// How many records this pass actually rendered and validated (bounded
    /// by `policy.drain_batch_max`).
    pub considered: usize,
    /// How many records were skipped because the batch exceeded
    /// `policy.drain_batch_max`. `0` when `truncated` is `false`.
    pub skipped: usize,
    /// Whether the input batch was larger than `policy.drain_batch_max` —
    /// `true` iff `skipped > 0`.
    pub truncated: bool,
    /// How many conforming records were enqueued onto `queue`.
    pub enqueued: usize,
    /// Every record that failed validation and was routed to
    /// `session-<slug>` instead — never silently dropped.
    pub routed_to_session: Vec<SessionRoutedRecord>,
    /// The result of the single [`OperatorQueue::next_deliverable`] call
    /// this drain pass makes after enqueuing — `None` if the queue was
    /// already at depth or nothing conforming was enqueued.
    pub delivered: Option<OperatorQueueItem>,
}

/// Drain `records` into `queue`: render + validate each (bounded by
/// `policy.drain_batch_max`), enqueue every conforming one at
/// `policy.harvest_item_priority` under [`ItemSource::GateApproval`], route
/// every non-conforming one to `session-<slug>`, then make exactly one
/// [`OperatorQueue::next_deliverable`] call and report its result.
///
/// Pure with respect to time: `enqueued_at` is supplied by the caller, no
/// clock is read inside this function. `queue` is mutated in place — the
/// records' relative ordering among themselves does not matter, since
/// delivery order is decided entirely by `queue`'s own
/// [`crate::operator::queue::compare_items`] at selection time.
pub fn drain(
    records: &[PendingHarvestRecord],
    queue: &mut OperatorQueue,
    limits: &OperatorPayloadLimits,
    policy: &ApproveAndRunPolicy,
    enqueued_at: DateTime<Utc>,
) -> DrainReport {
    let batch_max = policy.drain_batch_max as usize;
    let batch_len = records.len().min(batch_max);
    let batch = &records[..batch_len];
    let skipped = records.len() - batch_len;

    let mut enqueued = 0usize;
    let mut routed_to_session = Vec::new();

    for record in batch {
        match render_and_validate(record, limits) {
            Ok(validated) => {
                let item = OperatorQueueItem::new(
                    gate_id_for(&record.artifact_id),
                    validated.payload().clone(),
                    policy.harvest_item_priority,
                    enqueued_at,
                    ItemSource::GateApproval,
                );
                queue.enqueue(item);
                enqueued += 1;
            }
            Err(error) => {
                routed_to_session.push(SessionRoutedRecord {
                    artifact_id: record.artifact_id.clone(),
                    channel: OperatorChannel::session(policy.session_fallback_slug.clone()),
                    error,
                });
            }
        }
    }

    let delivered = queue.next_deliverable(enqueued_at);

    DrainReport {
        considered: batch_len,
        skipped,
        truncated: skipped > 0,
        enqueued,
        routed_to_session,
        delivered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::harvest_gate::pending_harvest_record;
    use crate::operator::queue::OperatorQueuePolicy;
    use chrono::TimeZone;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    fn record(artifact_id: &str) -> PendingHarvestRecord {
        let value = pending_harvest_record(
            artifact_id,
            "https://synapse.example/ingest/learning-artifact",
            json!({"title": "some artifact", "body": "content"}),
            vec!["docs/foo.md".to_string()],
        );
        PendingHarvestRecord::from_value(&value).expect("record parses")
    }

    fn queue() -> OperatorQueue {
        // Depth large enough that a single `next_deliverable` call never
        // hits a pre-existing depth limit in these tests.
        OperatorQueue::new(OperatorQueuePolicy::default())
    }

    #[test]
    fn sixty_pending_records_deliver_exactly_one_highest_priority_item() {
        // Invariant 3, asserted on the drain itself: 60 conforming records,
        // uniform priority under the default policy, so ordering among them
        // falls back to `compare_items`'s `enqueued_at`/`item_id` secondary
        // keys — deterministic, and exactly one is delivered regardless.
        let records: Vec<PendingHarvestRecord> = (0..60)
            .map(|i| record(&format!("artifact-{i:02}")))
            .collect();
        let mut q = queue();
        let policy = ApproveAndRunPolicy::default();
        let limits = OperatorPayloadLimits::default();

        let report = drain(&records, &mut q, &limits, &policy, now());

        assert_eq!(report.considered, 60);
        assert_eq!(report.enqueued, 60);
        assert!(report.routed_to_session.is_empty());
        assert!(!report.truncated);
        assert_eq!(report.skipped, 0);

        // Exactly one item was delivered by the single `next_deliverable`
        // call this drain pass makes.
        let delivered = report.delivered.expect("one item delivered");
        // Uniform priority + identical `enqueued_at` -> `item_id` is the
        // deciding tiebreak (lexicographically smallest first), matching
        // `compare_items`'s documented total order.
        assert_eq!(delivered.item_id, gate_id_for("artifact-00"));

        // Depth-1 default policy: nothing further is deliverable right now,
        // and 59 records remain pending on the queue itself.
        assert_eq!(q.pending_count(), 59);
        assert_eq!(q.open_count(), 1);
    }

    #[test]
    fn non_conforming_record_routes_to_session_never_notification() {
        // A tiny label limit makes the fixed "Approve" label unreducible —
        // see render.rs's equivalent test for why this is the right shape
        // of unreducible record.
        let limits = OperatorPayloadLimits {
            max_options: 3,
            min_options: 2,
            max_label_chars: 5,
            max_summary_chars: 1024,
        };
        let records = vec![record("artifact-bad")];
        let mut q = queue();
        let policy = ApproveAndRunPolicy::default();

        let report = drain(&records, &mut q, &limits, &policy, now());

        assert_eq!(report.considered, 1);
        assert_eq!(report.enqueued, 0);
        assert_eq!(report.routed_to_session.len(), 1);
        let routed = &report.routed_to_session[0];
        assert_eq!(routed.artifact_id, "artifact-bad");
        assert_eq!(
            routed.channel,
            OperatorChannel::session(policy.session_fallback_slug.clone())
        );

        // Never appears on the notification path: the queue is empty and
        // nothing was delivered.
        assert_eq!(q.pending_count(), 0);
        assert_eq!(report.delivered, None);
    }

    #[test]
    fn batch_larger_than_drain_batch_max_reports_truncation() {
        let policy = ApproveAndRunPolicy {
            drain_batch_max: 10,
            ..ApproveAndRunPolicy::default()
        };
        let limits = OperatorPayloadLimits::default();
        let records: Vec<PendingHarvestRecord> = (0..25)
            .map(|i| record(&format!("artifact-{i:02}")))
            .collect();
        let mut q = queue();

        let report = drain(&records, &mut q, &limits, &policy, now());

        assert_eq!(report.considered, 10);
        assert_eq!(report.enqueued, 10);
        assert!(report.truncated);
        assert_eq!(report.skipped, 15);
    }

    #[test]
    fn batch_not_exceeding_drain_batch_max_is_not_truncated() {
        let policy = ApproveAndRunPolicy {
            drain_batch_max: 10,
            ..ApproveAndRunPolicy::default()
        };
        let limits = OperatorPayloadLimits::default();
        let records: Vec<PendingHarvestRecord> = (0..10)
            .map(|i| record(&format!("artifact-{i:02}")))
            .collect();
        let mut q = queue();

        let report = drain(&records, &mut q, &limits, &policy, now());

        assert_eq!(report.considered, 10);
        assert!(!report.truncated);
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn empty_batch_enqueues_and_delivers_nothing() {
        let records: Vec<PendingHarvestRecord> = Vec::new();
        let mut q = queue();
        let policy = ApproveAndRunPolicy::default();
        let limits = OperatorPayloadLimits::default();

        let report = drain(&records, &mut q, &limits, &policy, now());

        assert_eq!(report.considered, 0);
        assert_eq!(report.enqueued, 0);
        assert!(!report.truncated);
        assert_eq!(report.delivered, None);
    }

    #[test]
    fn uniform_priority_drain_falls_back_to_item_id_tiebreak() {
        // Every record enqueues at the same `harvest_item_priority` (the
        // policy default, per the module header's assumption note), so
        // `compare_items`'s primary key ties for all three and delivery is
        // decided by the lexicographically-smallest `item_id`:
        // "artifact-high" < "artifact-low" < "artifact-mid".
        let records: Vec<PendingHarvestRecord> = vec![
            record("artifact-low"),
            record("artifact-high"),
            record("artifact-mid"),
        ];
        let mut q = queue();
        let policy = ApproveAndRunPolicy::default();
        let limits = OperatorPayloadLimits::default();

        let report = drain(&records, &mut q, &limits, &policy, now());
        let delivered = report.delivered.expect("one item delivered");
        assert_eq!(delivered.item_id, gate_id_for("artifact-high"));
    }
}
