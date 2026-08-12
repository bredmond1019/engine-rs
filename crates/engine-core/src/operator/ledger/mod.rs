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
//! its file-backed default impl, and the XDG path resolver.

pub mod record;
pub mod store;

pub use record::{ApprovalLedgerRow, LedgerDecision};
#[cfg(test)]
pub use store::InMemoryApprovalLedger;
pub use store::{default_ledger_path, ApprovalLedger, FileApprovalLedger};
