//! `engine-core` — the Node/Workflow runner and graph validator for engine-rs.
//!
//! Stub crate for EN.0.A (Cargo workspace + CI). Real types (`Node`, `Workflow`,
//! `WorkflowSchema`, `NodeConfig`, validator) land in later Phase 0/1 blocks — see
//! `docs/architecture.md` for the module map.

/// Placeholder identifying this crate; exists so the workspace has at least one
/// non-trivial symbol to build and test against before the real types land.
pub fn crate_name() -> &'static str {
    "engine-core"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_engine_core() {
        assert_eq!(crate_name(), "engine-core");
    }
}
