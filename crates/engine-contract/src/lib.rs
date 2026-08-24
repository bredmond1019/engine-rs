//! `engine-contract` — data-contract serde types for engine-rs (the `events` row,
//! `task_context`, `NodeRun`).
//!
//! Ports `orchestrator/docs/data-contract.md` v1.0.1 byte-for-byte (per engine-rs
//! governing principle 4). Any drift from that contract breaks `bastion` — see
//! `planning/EN.0.B-data-contract-postgres/tasks.md`.

pub mod envelope;
pub mod events;
pub mod journal;
pub mod task_context;

pub use envelope::{ChannelType, IngressEnvelope, ReplyContext, SourcePayload};
pub use events::EventsRow;
pub use journal::{JournalDecisionKind, JournalRow};
pub use task_context::{NodeRun, NodeRunStatus, TaskContext, Usage};

/// Placeholder identifying this crate; exists so the workspace has at least one
/// non-trivial symbol to build and test against before the real types land.
pub fn crate_name() -> &'static str {
    "engine-contract"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_engine_contract() {
        assert_eq!(crate_name(), "engine-contract");
    }
}
