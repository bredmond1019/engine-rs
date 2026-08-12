//! Derived queries over the approval ledger — `EN.8.C` task 4.
//!
//! These are the queries the client-facing time-to-approval claim rests on,
//! and the query the roadmap's Phase 4.5 operate-first gate is defined
//! against:
//!
//! - [`time_to_approval`] / [`time_to_approval_stats`] answer "how long does
//!   an operator actually take to decide?" — the evidence behind the
//!   approval gate being a claim the business can make with a number, not
//!   just a mechanism nobody can point at.
//! - [`decisions_per_day`] answers the roadmap's Phase 4.5 operate-first
//!   gate, defined as "decision rows on >=10 of any rolling 14 days" — that
//!   bar needs a query over real rows, not a manual read of a log file.
//!
//! Every function here is pure: no filesystem access, no clock reads. Each
//! takes rows (or a row) already read from an [`ApprovalLedger`](super::ApprovalLedger)
//! and returns a plain value.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};

use super::{ApprovalLedgerRow, LedgerDecision};

/// The time between a payload being delivered to the operator and the
/// decision being taken, for one row.
///
/// `decided_at - delivered_at`. Callers should not pass a `Requeued` row
/// here if they intend the result to mean "time to approve" — use
/// [`time_to_approval_stats`] over a row set, which excludes `Requeued`
/// rows for exactly that reason.
pub fn time_to_approval(row: &ApprovalLedgerRow) -> Duration {
    row.decided_at - row.delivered_at
}

/// Aggregate time-to-approval statistics over a set of rows.
///
/// `Requeued` rows are ignored — a re-queue is not an approval, and
/// including it would flatter (or otherwise distort) the number the
/// business claim rests on. `count` is the number of non-`Requeued` rows
/// the median/max were computed over; `median` and `max` are `None` when
/// that count is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeToApprovalStats {
    pub count: usize,
    pub median: Option<Duration>,
    pub max: Option<Duration>,
}

/// Compute [`TimeToApprovalStats`] over `rows`, ignoring every
/// [`LedgerDecision::Requeued`] row.
pub fn time_to_approval_stats(rows: &[ApprovalLedgerRow]) -> TimeToApprovalStats {
    let mut durations: Vec<Duration> = rows
        .iter()
        .filter(|row| row.decision != LedgerDecision::Requeued)
        .map(time_to_approval)
        .collect();

    if durations.is_empty() {
        return TimeToApprovalStats::default();
    }

    durations.sort();

    let count = durations.len();
    let max = durations.last().copied();
    let median = Some(durations[count / 2]);

    TimeToApprovalStats { count, median, max }
}

/// Bucket `rows` by the UTC calendar date of `decided_at`, counting rows
/// per day.
///
/// This is the query the roadmap's Phase 4.5 operate-first gate
/// ("decision rows on >=10 of any rolling 14 days") is evaluated against:
/// a caller can slide a 14-day window over the returned map's keys and
/// count how many of those days have a nonzero entry.
///
/// Every decision counts, including `Requeued` — the gate is about operator
/// activity on the surface, not approval throughput specifically.
pub fn decisions_per_day(rows: &[ApprovalLedgerRow]) -> BTreeMap<NaiveDate, usize> {
    let mut counts: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    for row in rows {
        let date = row.decided_at.date_naive();
        *counts.entry(date).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::ledger::record::{ApprovalLedgerRow, LedgerDecision};
    use chrono::{DateTime, TimeZone, Utc};

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn row(
        item_id: &str,
        decision: LedgerDecision,
        delivered_at: DateTime<Utc>,
        decided_at: DateTime<Utc>,
    ) -> ApprovalLedgerRow {
        ApprovalLedgerRow {
            item_id: item_id.to_string(),
            digest: "digest".to_string(),
            decision,
            who: "operator-a".to_string(),
            delivered_at,
            decided_at,
            rendered_diff: "diff".to_string(),
        }
    }

    #[test]
    fn time_to_approval_is_decided_minus_delivered() {
        let r = row("item-a", LedgerDecision::Approved, ts(0), ts(30));
        assert_eq!(time_to_approval(&r), Duration::seconds(30));
    }

    #[test]
    fn stats_ignore_requeued_rows() {
        let rows = vec![
            row("item-a", LedgerDecision::Approved, ts(0), ts(10)),
            row("item-b", LedgerDecision::Requeued, ts(0), ts(1_000_000)),
            row("item-c", LedgerDecision::Approved, ts(0), ts(30)),
        ];

        let stats = time_to_approval_stats(&rows);
        assert_eq!(stats.count, 2);
        assert_eq!(stats.median, Some(Duration::seconds(30)));
        assert_eq!(stats.max, Some(Duration::seconds(30)));
    }

    #[test]
    fn stats_are_default_when_only_requeued_rows_present() {
        let rows = vec![row("item-a", LedgerDecision::Requeued, ts(0), ts(10))];
        let stats = time_to_approval_stats(&rows);
        assert_eq!(stats, TimeToApprovalStats::default());
        assert_eq!(stats.count, 0);
    }

    #[test]
    fn stats_median_and_max_over_several_approved_rows() {
        let rows = vec![
            row("item-a", LedgerDecision::Approved, ts(0), ts(10)),
            row("item-b", LedgerDecision::Approved, ts(0), ts(50)),
            row("item-c", LedgerDecision::Approved, ts(0), ts(30)),
        ];
        let stats = time_to_approval_stats(&rows);
        assert_eq!(stats.count, 3);
        assert_eq!(stats.median, Some(Duration::seconds(30)));
        assert_eq!(stats.max, Some(Duration::seconds(50)));
    }

    #[test]
    fn decisions_per_day_buckets_by_utc_calendar_date() {
        let day_one = Utc.with_ymd_and_hms(2026, 8, 1, 3, 0, 0).unwrap();
        let day_one_later = Utc.with_ymd_and_hms(2026, 8, 1, 20, 0, 0).unwrap();
        let day_two = Utc.with_ymd_and_hms(2026, 8, 2, 3, 0, 0).unwrap();

        let rows = vec![
            row("item-a", LedgerDecision::Approved, day_one, day_one),
            row("item-b", LedgerDecision::Skipped, day_one, day_one_later),
            row("item-c", LedgerDecision::Requeued, day_two, day_two),
        ];

        let counts = decisions_per_day(&rows);
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[&day_one.date_naive()], 2);
        assert_eq!(counts[&day_two.date_naive()], 1);
    }

    #[test]
    fn decisions_per_day_counts_every_decision_including_requeued() {
        let day = Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
        let rows = vec![row("item-a", LedgerDecision::Requeued, day, day)];
        let counts = decisions_per_day(&rows);
        assert_eq!(counts[&day.date_naive()], 1);
    }
}
