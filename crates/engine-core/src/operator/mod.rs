//! `engine-core::operator` — the operator-facing payload contract (`EN.8.A`).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md`: what reaches the
//! operator is a validated shape, not a convention, and the channel a
//! workflow sends it over (`notification` vs. `session-<slug>`) is declared
//! at gate-definition time, never discovered or degraded at emit time.
//!
//! This module starts with [`limits`] (task 1) — the confirmed WhatsApp
//! interactive-reply limits that the payload/validation types (tasks 2-3)
//! and the gate-definition channel declaration (task 4) build on. [`payload`]
//! (task 2) defines the payload shape itself: an inline rendered summary,
//! its named response options, and the digest computed over both. [`validate`]
//! (task 3) is the only path to a [`ValidatedOperatorPayload`] — the type
//! the `notification` channel accepts — so a payload that fails validation
//! has no route onto that channel; it forces the gate to declare `session`
//! instead. [`channel`] (task 4) is [`OperatorChannel`] itself, the
//! declaration attached to `crate::nodes::harvest_gate::HarvestGate` so
//! which channel a gate routes to is readable off its definition without
//! executing the workflow.

pub mod channel;
pub mod limits;
pub mod payload;
#[cfg(test)]
mod tests;
pub mod validate;

pub use channel::OperatorChannel;
pub use limits::OperatorPayloadLimits;
pub use payload::{OperatorPayload, OperatorResponseOption};
pub use validate::{validate, OperatorValidationError, ValidatedOperatorPayload};
