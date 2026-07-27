//! Shared test-only `Node` fixtures, reused across `engine-serve`'s unit
//! tests (`src/http.rs`) and integration tests (`tests/*.rs`) so a fixture's
//! behavior can't drift out of sync between three independent copies — the
//! exact failure mode that made a prior copy's polling-loop workaround for a
//! since-fixed race invisible in the other two.
//!
//! Gated behind `cfg(any(test, feature = "test-fixtures"))`: unit tests
//! inside this crate get it for free via `cfg(test)`; integration test
//! binaries (which link against `engine-serve` as an ordinary external
//! crate, not under `cfg(test)`) need the `test-fixtures` feature enabled,
//! which the crate's own `[dev-dependencies]` entry in `Cargo.toml` does for
//! every `tests/*.rs` binary. Never compiled into a normal (non-test)
//! build of the crate.

use std::sync::Arc;

use async_trait::async_trait;
use engine_contract::TaskContext;
use engine_core::{Node, NodeError};
use tokio::sync::Notify;

/// A node that blocks in `process` until `release` is notified — gives a
/// test a deterministic window to observe a run mid-flight before letting it
/// proceed.
///
/// `identity` is both the registry key and the map key this node writes
/// under in `TaskContext::nodes`/`node_runs` (contract §1). Use
/// [`WaitNode::new`] for a single-`WaitNode` fixture graph (identity
/// `"WaitNode"`); use [`WaitNode::named`] when a multi-node graph needs more
/// than one `WaitNode` instance, each under its own identity.
pub struct WaitNode {
    pub identity: &'static str,
    pub release: Arc<Notify>,
}

impl WaitNode {
    pub fn new(release: Arc<Notify>) -> Self {
        Self {
            identity: "WaitNode",
            release,
        }
    }

    pub fn named(identity: &'static str, release: Arc<Notify>) -> Self {
        Self { identity, release }
    }
}

#[async_trait]
impl Node for WaitNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        self.release.notified().await;
        ctx.nodes.insert(
            self.identity.to_string(),
            serde_json::json!({ "ran": true }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        self.identity
    }
}
