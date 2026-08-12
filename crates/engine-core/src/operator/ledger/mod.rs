//! `engine-core::operator::ledger` — the approval ledger (`EN.8.C`).
//!
//! Persists `{digest, decision, who, timestamp, rendered diff}` for every
//! operator gate decision, from day one, with time-to-approval derivable —
//! see `planning/8.C-approval-ledger/tasks.md` for the full block spec.
//!
//! This module starts with [`record`] (task 1): [`ApprovalLedgerRow`], the
//! plain data carrier every later task in this block builds on, and
//! [`LedgerDecision`], the closed set of outcomes a gate decision can
//! record. [`store`] (task 2) adds the injectable [`ApprovalLedger`] seam,
//! its file-backed default impl, and the XDG path resolver. Task 3 adds
//! [`record_decision`] itself — the one function that turns a gate decision
//! into exactly one ledger row, with the digest-mismatch-\>`Requeued`
//! enforcement built in so it cannot be bypassed at a call site. [`query`]
//! (task 4) adds the pure, hermetic derived queries the client-facing
//! time-to-approval claim and the roadmap's Phase 4.5 operate-first gate
//! rest on.

use chrono::{DateTime, Utc};

pub mod query;
pub mod record;
pub mod store;

pub use query::{decisions_per_day, time_to_approval, time_to_approval_stats, TimeToApprovalStats};
pub use record::{ApprovalLedgerRow, LedgerDecision};
#[cfg(test)]
pub use store::InMemoryApprovalLedger;
pub use store::{default_ledger_path, ApprovalLedger, FileApprovalLedger};

/// What [`record_decision`] tells the caller after appending exactly one
/// row.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDecisionOutcome {
    /// The row that was actually appended. Its `decision` is the *enforced*
    /// outcome, which may differ from `requested_decision` — a digest
    /// mismatch always downgrades the written row to
    /// [`LedgerDecision::Requeued`], regardless of what was requested.
    pub row: ApprovalLedgerRow,
    /// Whether the caller may proceed to execute the decision. `false`
    /// whenever the digest presented at decision time does not match the
    /// digest the payload was delivered under (the row is `Requeued` in
    /// that case) or the recorded decision is not [`LedgerDecision::Approved`] —
    /// only a matched digest with an `Approved` outcome authorizes
    /// execution.
    pub should_execute: bool,
}

/// Record one operator gate decision, appending exactly one row through
/// `ledger`.
///
/// `rendered_diff` must be copied from the delivered payload by the
/// caller — this function never re-derives it, holding the same
/// no-re-derivation guarantee `PersistToBrainNode`/`HarvestApproveNode` hold
/// for the harvest payload (`docs/harvest-gate.md`).
///
/// The digest check is enforced here, not left to the caller: when
/// `presented_digest` (the digest shown to the operator at decision time)
/// does not match `delivered_digest` (the digest the payload was delivered
/// under), the row written is always [`LedgerDecision::Requeued`] —
/// `requested_decision` is discarded in that case — and
/// [`RecordDecisionOutcome::should_execute`] is `false`. It is therefore
/// impossible to write an `Approved` row for a mismatched digest through
/// this function, no matter what the caller passes as
/// `requested_decision`.
#[allow(clippy::too_many_arguments)]
pub fn record_decision(
    ledger: &dyn ApprovalLedger,
    item_id: impl Into<String>,
    delivered_digest: impl Into<String>,
    presented_digest: &str,
    rendered_diff: impl Into<String>,
    requested_decision: LedgerDecision,
    who: impl Into<String>,
    delivered_at: DateTime<Utc>,
    decided_at: DateTime<Utc>,
) -> RecordDecisionOutcome {
    let delivered_digest = delivered_digest.into();
    let digest_matches = delivered_digest == presented_digest;

    // Enforced here: a mismatched digest can never produce anything but a
    // `Requeued` row, regardless of what the caller asked for.
    let decision = if digest_matches {
        requested_decision
    } else {
        LedgerDecision::Requeued
    };

    let row = ApprovalLedgerRow {
        item_id: item_id.into(),
        digest: delivered_digest,
        decision,
        who: who.into(),
        delivered_at,
        decided_at,
        rendered_diff: rendered_diff.into(),
    };

    ledger.append(row.clone());

    RecordDecisionOutcome {
        should_execute: digest_matches && decision == LedgerDecision::Approved,
        row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::ledger::store::InMemoryApprovalLedger;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn matched_digest_records_the_requested_decision_and_allows_execution() {
        let ledger = InMemoryApprovalLedger::new();
        let outcome = record_decision(
            &ledger,
            "item-a",
            "digest-a",
            "digest-a",
            "rendered summary",
            LedgerDecision::Approved,
            "operator-a",
            ts(0),
            ts(10),
        );

        assert!(outcome.should_execute);
        assert_eq!(outcome.row.decision, LedgerDecision::Approved);
        assert_eq!(ledger.read_all(), vec![outcome.row]);
    }

    #[test]
    fn mismatched_digest_is_always_recorded_as_requeued_never_approved() {
        let ledger = InMemoryApprovalLedger::new();
        let outcome = record_decision(
            &ledger,
            "item-a",
            "digest-delivered",
            "digest-presented-different",
            "rendered summary",
            LedgerDecision::Approved,
            "operator-a",
            ts(0),
            ts(10),
        );

        assert!(!outcome.should_execute);
        assert_eq!(outcome.row.decision, LedgerDecision::Requeued);
        assert_eq!(outcome.row.digest, "digest-delivered");
        let rows = ledger.read_all();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision, LedgerDecision::Requeued);
    }

    #[test]
    fn rendered_diff_is_stored_byte_identical_to_what_was_passed() {
        let ledger = InMemoryApprovalLedger::new();
        let diff = "diff: 4 identical one-line edits, byte-for-byte";
        let outcome = record_decision(
            &ledger,
            "item-a",
            "digest-a",
            "digest-a",
            diff,
            LedgerDecision::Approved,
            "operator-a",
            ts(0),
            ts(10),
        );
        assert_eq!(outcome.row.rendered_diff, diff);
    }

    #[test]
    fn two_decisions_on_the_same_item_append_two_rows() {
        let ledger = InMemoryApprovalLedger::new();
        record_decision(
            &ledger,
            "item-a",
            "digest-a",
            "digest-a",
            "summary",
            LedgerDecision::Approved,
            "operator-a",
            ts(0),
            ts(10),
        );
        record_decision(
            &ledger,
            "item-a",
            "digest-a",
            "digest-a",
            "summary",
            LedgerDecision::Skipped,
            "operator-b",
            ts(0),
            ts(20),
        );
        assert_eq!(ledger.rows_for("item-a").len(), 2);
    }

    #[test]
    fn non_approved_matched_digest_decision_does_not_authorize_execution() {
        let ledger = InMemoryApprovalLedger::new();
        let outcome = record_decision(
            &ledger,
            "item-a",
            "digest-a",
            "digest-a",
            "summary",
            LedgerDecision::Skipped,
            "operator-a",
            ts(0),
            ts(10),
        );
        assert!(!outcome.should_execute);
        assert_eq!(outcome.row.decision, LedgerDecision::Skipped);
    }
}
