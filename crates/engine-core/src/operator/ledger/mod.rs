//! `engine-core::operator::ledger` — the approval ledger (`EN.8.C`).
//!
//! Persists `{digest, decision, who, timestamp, rendered diff}` for every
//! operator gate decision, from day one, with time-to-approval derivable —
//! see `planning/8.C-approval-ledger/tasks.md` for the full block spec.
//!
//! This module starts with [`record`] (task 1): [`ApprovalLedgerRow`], the
//! plain data carrier every later task in this block builds on, and
//! [`LedgerDecision`], the closed set of outcomes a gate decision can
//! record.

pub mod record;

pub use record::{ApprovalLedgerRow, LedgerDecision};
