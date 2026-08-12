//! `engine-core::operator` — the operator-facing payload contract (`EN.8.A`).
//!
//! Per `planning/8.A-operator-payload-contract/tasks.md`: what reaches the
//! operator is a validated shape, not a convention, and the channel a
//! workflow sends it over (`notification` vs. `session-<slug>`) is declared
//! at gate-definition time, never discovered or degraded at emit time.
//!
//! This module starts with [`limits`] (task 1) — the confirmed WhatsApp
//! interactive-reply limits that the payload/validation types (tasks 2-3)
//! and the gate-definition channel declaration (task 4) build on.

pub mod limits;

pub use limits::OperatorPayloadLimits;
