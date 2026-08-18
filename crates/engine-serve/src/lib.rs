//! `engine-serve` — the `bastion serve` embedding: in-memory run state, trigger/dispatch,
//! HTTP surface.
//!
//! Stub crate for EN.0.A (Cargo workspace + CI). The trigger/dispatch path lands once
//! `engine-core` and `engine-store` are real — see `docs/architecture.md` for the module map.

pub mod abort;
pub mod approvals;
pub mod dispatch;
pub mod durable;
pub mod email_webhooks;
pub mod http;
pub mod live_state;
pub mod orphan;
pub mod resume;
pub mod schedule;
pub mod stream;
pub mod suspend;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod workflows;

/// Placeholder identifying this crate; exists so the workspace has at least one
/// non-trivial symbol to build and test against before the real types land.
pub fn crate_name() -> &'static str {
    "engine-serve"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_engine_serve() {
        assert_eq!(crate_name(), "engine-serve");
    }
}
