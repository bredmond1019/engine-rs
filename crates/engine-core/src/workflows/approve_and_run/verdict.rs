//! The verdict path — ledger row, digest-mismatch re-queue, execute-or-not
//! (`EN.8.D` task 4).
//!
//! [`decide`] takes a delivered [`OperatorQueueItem`], the digest presented
//! to the operator at decision time, the option key the operator tapped,
//! and a [`ApprovalLedger`] seam, and turns that into exactly one ledger
//! row via [`operator::ledger::record_decision`]. It does **not**
//! re-implement the digest check — `record_decision` already enforces
//! mismatch -> [`LedgerDecision::Requeued`] with `should_execute == false`,
//! and this module only ever *consumes* [`RecordDecisionOutcome`], never
//! branches around it.
//!
//! On a digest mismatch, the delivered item is answered off the queue's
//! open slot and pushed back onto `pending` — re-queued, never dropped, and
//! never authorized to execute. On a matched `Approved` verdict, [`decide`]
//! returns an [`ExecutionAuthorization`] carrying the **stored** payload
//! (`url` + `payload`) from the pending-harvest record — copied verbatim,
//! never re-derived — for task 5 to POST.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::operator::ledger::{
    record_decision, ApprovalLedger, LedgerDecision, RecordDecisionOutcome,
};
use crate::operator::queue::{OperatorQueue, OperatorQueueItem};

use super::render::{PendingHarvestRecord, OPTION_APPROVE, OPTION_OPEN_SESSION, OPTION_SKIP};

/// The option key the operator tapped did not match one of the three
/// stable keys [`super::render`] renders — approve / skip / open a
/// session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownOptionKey(pub String);

impl std::fmt::Display for UnknownOptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown operator response option key: {:?}", self.0)
    }
}

impl std::error::Error for UnknownOptionKey {}

/// What executing task 4's authorization requires: the exact `url` +
/// `payload` from the pending-harvest record, never re-derived from the
/// rendered summary the operator saw.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionAuthorization {
    /// The queue item this authorization resolves.
    pub item_id: String,
    /// The Synapse ingest URL, copied verbatim from the pending-harvest
    /// record.
    pub url: String,
    /// The exact JSON body to POST, copied verbatim from the
    /// pending-harvest record — never re-derived from the rendered
    /// summary.
    pub payload: Value,
}

/// The full result of one [`decide`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictOutcome {
    /// What `record_decision` wrote and authorized.
    pub ledger_outcome: RecordDecisionOutcome,
    /// `true` iff the presented digest did not match the delivered digest
    /// and the item was pushed back onto `queue`'s pending set.
    pub requeued: bool,
    /// `Some` iff the digest matched and the recorded decision is
    /// [`LedgerDecision::Approved`] — the same condition
    /// `ledger_outcome.should_execute` reports.
    pub authorization: Option<ExecutionAuthorization>,
}

/// Map the operator's tapped option key to the [`LedgerDecision`] it
/// requests. Only the three keys [`super::render::response_options`]
/// renders are recognized; anything else is
/// [`UnknownOptionKey`].
fn requested_decision_for(option_key: &str) -> Result<LedgerDecision, UnknownOptionKey> {
    match option_key {
        OPTION_APPROVE => Ok(LedgerDecision::Approved),
        OPTION_SKIP => Ok(LedgerDecision::Skipped),
        OPTION_OPEN_SESSION => Ok(LedgerDecision::RoutedToSession),
        other => Err(UnknownOptionKey(other.to_string())),
    }
}

/// Resolve one operator verdict for `item`, delivered off `queue`.
///
/// - `item` is the item [`OperatorQueue::next_deliverable`] handed out —
///   its `payload.digest` is the digest the item was *delivered* under.
/// - `presented_digest` is the digest shown to the operator at decision
///   time (what the channel echoes back with the verdict).
/// - `option_key` is the response option the operator tapped.
/// - `record` supplies the stored `url`/`payload` an `Approved` outcome
///   authorizes for execution — never re-derived from `item.payload`.
///
/// Always answers `item.item_id` off `queue`'s open slot (the decision has
/// been made, one way or another). When the recorded decision is
/// [`LedgerDecision::Requeued`] (a digest mismatch), the item is also
/// pushed back onto `queue`'s pending set — re-queued, never dropped.
///
/// Returns [`UnknownOptionKey`] when `option_key` is not one of the three
/// stable keys [`super::render`] renders; in that case no ledger row is
/// written and `queue` is left untouched.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    queue: &mut OperatorQueue,
    item: &OperatorQueueItem,
    record: &PendingHarvestRecord,
    presented_digest: &str,
    option_key: &str,
    ledger: &dyn ApprovalLedger,
    who: impl Into<String>,
    delivered_at: DateTime<Utc>,
    decided_at: DateTime<Utc>,
) -> Result<VerdictOutcome, UnknownOptionKey> {
    let requested_decision = requested_decision_for(option_key)?;

    let ledger_outcome = record_decision(
        ledger,
        item.item_id.clone(),
        item.payload.digest.clone(),
        presented_digest,
        item.payload.rendered_summary.clone(),
        requested_decision,
        who,
        delivered_at,
        decided_at,
    );

    queue.answer(&item.item_id);

    let requeued = ledger_outcome.row.decision == LedgerDecision::Requeued;
    if requeued {
        queue.enqueue(item.clone());
    }

    let authorization = if ledger_outcome.should_execute {
        Some(ExecutionAuthorization {
            item_id: item.item_id.clone(),
            url: record.url.clone(),
            payload: record.payload.clone(),
        })
    } else {
        None
    };

    Ok(VerdictOutcome {
        ledger_outcome,
        requeued,
        authorization,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::harvest_gate::pending_harvest_record;
    use crate::operator::ledger::InMemoryApprovalLedger;
    use crate::operator::queue::{ItemSource, OperatorQueuePolicy};
    use crate::operator::OperatorPayloadLimits;
    use chrono::TimeZone;
    use serde_json::json;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
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

    fn delivered_item(record: &PendingHarvestRecord) -> OperatorQueueItem {
        let limits = OperatorPayloadLimits::default();
        let validated = super::super::render::render_and_validate(record, &limits)
            .expect("record renders and validates");
        OperatorQueueItem::new(
            validated.payload().gate_id.clone(),
            validated.payload().clone(),
            0,
            ts(0),
            ItemSource::GateApproval,
        )
    }

    fn queue_with_item(item: &OperatorQueueItem) -> OperatorQueue {
        let mut queue = OperatorQueue::new(OperatorQueuePolicy::default());
        queue.enqueue(item.clone());
        let delivered = queue.next_deliverable(ts(0)).expect("item delivered");
        assert_eq!(&delivered, item);
        queue
    }

    #[test]
    fn matched_approve_writes_one_approved_row_and_authorizes_execution() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        let outcome = decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            OPTION_APPROVE,
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect("approve is a known option");

        assert!(outcome.ledger_outcome.should_execute);
        assert_eq!(
            outcome.ledger_outcome.row.decision,
            LedgerDecision::Approved
        );
        assert!(!outcome.requeued);
        let auth = outcome
            .authorization
            .expect("approved authorizes execution");
        assert_eq!(auth.item_id, item.item_id);
        assert_eq!(auth.url, record.url);
        assert_eq!(auth.payload, record.payload);
        assert_eq!(ledger.read_all().len(), 1);
        // Resolved off the open slot, not left dangling open.
        assert_eq!(queue.open_count(), 0);
        assert_eq!(queue.pending_count(), 0);
    }

    #[test]
    fn mismatched_digest_writes_one_requeued_row_authorizes_nothing_and_requeues() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        let outcome = decide(
            &mut queue,
            &item,
            &record,
            "a-different-digest-than-was-delivered",
            OPTION_APPROVE,
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect("approve is a known option");

        assert!(!outcome.ledger_outcome.should_execute);
        assert_eq!(
            outcome.ledger_outcome.row.decision,
            LedgerDecision::Requeued
        );
        assert!(outcome.requeued);
        assert!(outcome.authorization.is_none());
        assert_eq!(ledger.read_all().len(), 1);
        // Back on the queue, not dropped: open slot cleared, pending has it.
        assert_eq!(queue.open_count(), 0);
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn skip_writes_its_row_and_authorizes_nothing() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        let outcome = decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            OPTION_SKIP,
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect("skip is a known option");

        assert!(!outcome.ledger_outcome.should_execute);
        assert_eq!(outcome.ledger_outcome.row.decision, LedgerDecision::Skipped);
        assert!(!outcome.requeued);
        assert!(outcome.authorization.is_none());
    }

    #[test]
    fn open_session_writes_its_row_and_authorizes_nothing() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        let outcome = decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            OPTION_OPEN_SESSION,
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect("open_session is a known option");

        assert!(!outcome.ledger_outcome.should_execute);
        assert_eq!(
            outcome.ledger_outcome.row.decision,
            LedgerDecision::RoutedToSession
        );
        assert!(!outcome.requeued);
        assert!(outcome.authorization.is_none());
    }

    #[test]
    fn two_decisions_on_one_item_append_two_rows() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            OPTION_SKIP,
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect("skip is a known option");

        // Re-deliver (the caller re-drove it a second time, e.g. a resend)
        // and decide again.
        decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            OPTION_SKIP,
            &ledger,
            "operator-b",
            ts(20),
            ts(30),
        )
        .expect("skip is a known option");

        assert_eq!(ledger.rows_for(&item.item_id).len(), 2);
    }

    #[test]
    fn unknown_option_key_is_rejected_and_writes_nothing() {
        let record = record("artifact-1");
        let item = delivered_item(&record);
        let mut queue = queue_with_item(&item);
        let ledger = InMemoryApprovalLedger::new();

        let err = decide(
            &mut queue,
            &item,
            &record,
            &item.payload.digest,
            "not_a_real_option",
            &ledger,
            "operator-a",
            ts(0),
            ts(10),
        )
        .expect_err("unrecognized option key must be rejected");

        assert_eq!(err, UnknownOptionKey("not_a_real_option".to_string()));
        assert!(ledger.read_all().is_empty());
    }
}
