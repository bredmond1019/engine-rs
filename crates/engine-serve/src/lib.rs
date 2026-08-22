//! `engine-serve` — the `bastion serve` embedding: in-memory run state, trigger/dispatch,
//! HTTP surface.
//!
//! Stub crate for EN.0.A (Cargo workspace + CI). The trigger/dispatch path lands once
//! `engine-core` and `engine-store` are real — see `docs/architecture.md` for the module map.

pub mod abort;
pub mod approvals;
pub mod blocked_bridge;
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

/// The env var that resolves the tracing filter level (policy knob, CLAUDE.md standing rule 6).
///
/// Layered like every other policy knob in this repo: an explicit env var override, falling back
/// to a behavior-stable built-in default. Standard `tracing_subscriber::EnvFilter` syntax (e.g.
/// `info`, `engine_core=debug,warn`) is accepted.
pub const ENGINE_LOG_ENV: &str = "ENGINE_LOG";

/// The built-in default filter level. Chosen to reproduce today's `eprintln!` visibility: those
/// calls print unconditionally today, so `info` is the level that keeps existing operational
/// output visible without this knob's addition changing what a run emits by default.
const DEFAULT_LOG_FILTER: &str = "info";

/// Install the process-wide JSON tracing subscriber, once.
///
/// `engine-serve` is a library embedded in a host process (`bastion serve`) — there is no
/// `main.rs` under `crates/` for engine-rs to own an entry point in, so this is not called as a
/// side effect of linking the crate. The HOST calls this explicitly at startup.
///
/// Idempotent: uses [`tracing_subscriber::fmt::Subscriber::try_init`], which returns an error
/// (swallowed here) rather than panicking when a global subscriber is already installed — by a
/// prior call, by the host itself, or by a test harness. A library that panics its host on
/// double-init is worse than a library that just quietly keeps the first subscriber installed.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env(ENGINE_LOG_ENV)
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    // `flatten_event(true)` (EN.11.I task 5): moves each event's OWN fields (e.g.
    // `crate::workflow::node_context`'s `node`/`run_id`/`campaign_id`) up to the top level of
    // the JSON line instead of nesting them under a `"fields"` object — required for the
    // block's own acceptance criterion, `jq -e 'select(.run_id==$ID) | .node'`, to be a literal
    // top-level-key query rather than `.fields.run_id`/`.fields.node`.
    //
    // try_init (not init): a second call, or a host that already installed a global default
    // subscriber, is a no-op. The error variant carries no information worth surfacing here.
    let _ = fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(filter)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_engine_serve() {
        assert_eq!(crate_name(), "engine-serve");
    }

    #[test]
    fn init_tracing_is_idempotent() {
        // First call installs (or, if some earlier test in this binary already installed one,
        // is already a no-op) the global subscriber. The point under test is the SECOND call:
        // it must not panic just because a subscriber is already installed.
        init_tracing();
        init_tracing();
    }
}
