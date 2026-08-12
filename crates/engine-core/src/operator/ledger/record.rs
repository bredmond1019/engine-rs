//! The approval ledger row type — `EN.8.C` task 1.
//!
//! `ApprovalLedgerRow` is the plain data carrier this whole block persists,
//! reads, and derives time-to-approval from. It carries the five contract
//! fields (`digest`, `decision`, `who`, a decision timestamp, `rendered_diff`)
//! plus the two extra fields the acceptance criteria need: `delivered_at`
//! (when the payload reached the operator, the other end of the
//! time-to-approval interval) and `item_id` (which queued item the row is
//! about, so two decisions on the same item are two distinguishable rows).
//!
//! This type performs no I/O and reads no clock, mirroring `bastion`'s
//! `BlockedEdgeRecord::new` convention: both timestamps are constructor
//! inputs supplied by the caller, never sourced from `Utc::now()` here.
//! That is what keeps every test in this block hermetic and clock-free.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The outcome recorded for one operator gate decision.
///
/// `Requeued` is a decision variant, not an error, deliberately: a digest
/// mismatch between the payload delivered and the payload presented at
/// decision time is a recorded outcome the ledger must capture (per the
/// block's acceptance criteria — it must show up as a re-queue row, never
/// as a silent failure to record and never as an `Approved` row). Modeling
/// it as a variant of the same enum as `Approved`/`Skipped`/
/// `RoutedToSession` keeps it in the same append path and the same query
/// surface as every other decision, instead of living in a side channel
/// that time-to-approval and `decisions_per_day` would have to special-case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerDecision {
    Approved,
    Skipped,
    RoutedToSession,
    Requeued,
}

/// One row of the append-only approval ledger.
///
/// Every operator gate decision produces exactly one of these. All fields
/// are plain, owned data — constructing a row never touches the filesystem
/// or the clock; the caller (the `record_decision` seam added in task 3)
/// supplies `delivered_at` and `decided_at` explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalLedgerRow {
    /// Which queued item this row is about.
    pub item_id: String,
    /// The digest the payload was delivered under (computed over the
    /// rendered payload by `EN.8.A`'s `validate` module).
    pub digest: String,
    /// The outcome of this decision.
    pub decision: LedgerDecision,
    /// The identity the decision arrived with (the channel's declared
    /// operator identity) — an opaque string, not a user database.
    pub who: String,
    /// The moment the payload was delivered to the operator — the start of
    /// the time-to-approval interval.
    pub delivered_at: DateTime<Utc>,
    /// The moment this decision was taken — the end of the time-to-approval
    /// interval.
    pub decided_at: DateTime<Utc>,
    /// The rendered summary as delivered, copied verbatim from the
    /// payload — never re-derived at decision time, so it stays
    /// byte-identical to what the operator actually saw.
    pub rendered_diff: String,
}
