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
///
/// ## `EN.19.D` task 1 — extend vs. wrap, decided
///
/// `PLANNING_PIPELINE`'s `ApprovalGateNode` needs to record approve / reject
/// / discuss verdicts. This enum was **extended** with [`Rejected`] and
/// [`RoutedToDiscussion`] rather than wrapped in a second, `planning_pipeline`
/// -scoped decision type. Checked against the real code before deciding,
/// per the block record's ground-truth requirement:
///
/// - **No call site exhaustively `match`es over `LedgerDecision`.** Every
///   consumer (`record_decision`'s digest-mismatch enforcement,
///   `approve_and_run::verdict::decide`, every ledger query in
///   `operator::ledger::query`) only ever compares a single variant with
///   `==` (e.g. `decision == LedgerDecision::Approved`,
///   `decision != LedgerDecision::Requeued`). Adding two variants therefore
///   changes zero existing match arms and cannot silently mis-route an
///   existing call site — the usual hazard a wrapping type exists to avoid
///   simply does not apply here.
/// - **The persisted representation stays additive.** `serde(rename_all =
///   "snake_case")` derives `"rejected"` / `"routed_to_discussion"` for the
///   new variants; every ledger row written before this change still
///   deserializes unchanged, and nothing here touches `ApprovalLedgerRow`'s
///   shape.
/// - **The digest-mismatch enforcement in [`super::record_decision`] is
///   untouched.** It still downgrades any `requested_decision` — including
///   the two new variants — to `Requeued` on a mismatched digest, and
///   `should_execute` still requires an exact `Approved` match. Neither
///   check inspects the variant set, so nothing to update there either.
/// - **A wrapping type would have duplicated, not simplified.** `harvest_
///   approve`/`approve_and_run` and `planning_pipeline` would then hold two
///   different decision types over the *same* `ApprovalLedgerRow`/`record_
///   decision`/query surface, forcing every ledger query (`decisions_per_
///   day`, `time_to_approval`) to either special-case two enums or convert
///   between them at the boundary — exactly the "second, thinner approval
///   mechanism living side by side with the first" this block's own `why`
///   field says to avoid.
///
/// `approve` reuses the existing [`Approved`] variant directly (the same
/// outcome, not a new one); only `reject` and `discuss` needed new
/// vocabulary, because `Skipped` (harvest: "not applicable, move on") and
/// `RoutedToSession` (harvest: "a human will finish this by hand in a
/// session") are semantically different verdicts from `planning_pipeline`'s
/// "explicitly declined" and "route to `EN.19.E`'s `DiscussFurtherNode` for
/// more back-and-forth before deciding" — reusing them would blur the audit
/// trail's meaning for both call sites reading the ledger back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerDecision {
    Approved,
    Skipped,
    RoutedToSession,
    Requeued,
    /// `PLANNING_PIPELINE`'s `ApprovalGateNode` (`EN.19.D`): the operator
    /// explicitly declined the completed stage's output. Distinct from
    /// [`Skipped`] (harvest: not applicable) — a reject is a considered
    /// "no", not an absence of a decision.
    Rejected,
    /// `PLANNING_PIPELINE`'s `ApprovalGateNode` (`EN.19.D`): the operator
    /// wants more back-and-forth before deciding, routed to `EN.19.E`'s
    /// `DiscussFurtherNode`. Distinct from [`RoutedToSession`] (harvest: a
    /// human finishes the work by hand) — a discuss verdict still expects
    /// to return to this same gate afterward.
    RoutedToDiscussion,
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
