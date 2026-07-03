//! `engine-contract` — data-contract serde types for engine-rs (the `events` row,
//! `task_context`, `NodeRun`).
//!
//! Stub crate for EN.0.A (Cargo workspace + CI). Real types land once the orchestrator
//! data-contract (`orchestrator/docs/data-contract.md`) is ported — see
//! `docs/architecture.md` for the module map.

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
