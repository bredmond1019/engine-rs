//! `FanOutNode` (`EN.6.G` task 1) — constructs N identity-distinguished
//! instances of the same underlying node type and delegates to
//! [`crate::parallel::ParallelNode`] to run them concurrently.
//!
//! Per this ticket's investigation (see `planning/en-6g-schedule-source-fan-out-aggregate/tasks.md`),
//! `EN.5.E`'s [`crate::node::NodeExt::with_identity`] already gives
//! `ParallelNode`'s merge a distinct `ctx.nodes` key per branch instance —
//! the actual gap was that nothing *constructed* N such instances from one
//! node-building closure. `FanOutNode` is that constructor: it holds a
//! `count` and a boxed `Fn(usize) -> Box<dyn Node>` builder, calls the
//! builder once per index, wraps each result with `.with_identity(..)`
//! under a deterministic `"{base_name}[{i}]"` key, and hands the N wrapped
//! instances to a fresh `ParallelNode`.
//!
//! `impl Node for Box<dyn Node>` (below) is what makes `.with_identity()`
//! callable on the builder's `Box<dyn Node>` output — `NodeExt` is a
//! blanket impl over `N: Node + Sized`, and a boxed trait object needs its
//! own forwarding `Node` impl to satisfy that bound. This is added here
//! (not in `crate::node`, which this ticket's task scope leaves untouched)
//! because `FanOutNode` is the first caller that needs a boxed, type-erased
//! node to carry an identity override.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError, NodeExt};
use crate::parallel::ParallelNode;
use crate::routing::Router;

/// Forwarding `Node` impl for `Box<dyn Node>` so a type-erased node can still
/// be wrapped with [`crate::node::NodeExt::with_identity`] (which requires
/// `Self: Sized` — a `Box<dyn Node>` pointer is `Sized` even though the
/// value it points to is not).
#[async_trait::async_trait]
impl Node for Box<dyn Node> {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        (**self).process(ctx).await
    }

    fn name(&self) -> &str {
        (**self).name()
    }

    fn as_router(&self) -> Option<&dyn Router> {
        (**self).as_router()
    }
}

/// Fans one incoming `TaskContext` out to `count` identity-distinguished
/// instances of the node type `builder` constructs, running them
/// concurrently via [`ParallelNode`] and merging their outputs back — see
/// module docs.
pub struct FanOutNode {
    identity: String,
    base_name: String,
    count: usize,
    builder: Box<dyn Fn(usize) -> Box<dyn Node> + Send + Sync>,
}

impl FanOutNode {
    /// Build a `FanOutNode` under `identity` that constructs `count`
    /// branch instances via `builder(0..count)`, each wrapped under the
    /// identity [`FanOutNode::branch_identity`] computes for `base_name`.
    pub fn new(
        identity: impl Into<String>,
        base_name: impl Into<String>,
        count: usize,
        builder: impl Fn(usize) -> Box<dyn Node> + Send + Sync + 'static,
    ) -> Self {
        Self {
            identity: identity.into(),
            base_name: base_name.into(),
            count,
            builder: Box::new(builder),
        }
    }

    /// The deterministic per-instance identity `FanOutNode` assigns to
    /// branch `i` of `base_name` — exposed so callers (e.g. `AggregateNode`)
    /// can be configured with the exact identities a given fan-out produced,
    /// without re-deriving the naming scheme.
    #[must_use]
    pub fn branch_identity(base_name: &str, i: usize) -> String {
        format!("{base_name}[{i}]")
    }
}

#[async_trait::async_trait]
impl Node for FanOutNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let branches: Vec<Box<dyn Node>> = (0..self.count)
            .map(|i| {
                let instance = (self.builder)(i);
                let identity = Self::branch_identity(&self.base_name, i);
                Box::new(instance.with_identity(identity)) as Box<dyn Node>
            })
            .collect();

        let parallel = ParallelNode::new(self.identity.clone(), branches);
        parallel.process(ctx).await
    }

    fn name(&self) -> &str {
        &self.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// A trivial node type, deliberately stamping `ctx.nodes` under its own
    /// `name()` (not a hardcoded per-instance string) — exactly the shape
    /// that collides under old (pre-`with_identity`) last-write-wins merge
    /// semantics when the same type runs N times in one `ParallelNode`.
    struct SourceNode {
        value: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl Node for SourceNode {
        async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
            ctx.nodes
                .insert(self.name().to_string(), self.value.clone());
            Ok(ctx)
        }

        fn name(&self) -> &str {
            "SourceNode"
        }
    }

    #[tokio::test]
    async fn fan_out_produces_n_distinct_ctx_nodes_entries_with_no_collision() {
        let fan_out = FanOutNode::new("FanOut", "Source", 3, |i| {
            Box::new(SourceNode {
                value: serde_json::json!({ "i": i }),
            }) as Box<dyn Node>
        });

        let out = fan_out
            .process(empty_context())
            .await
            .expect("fan-out should succeed");

        for i in 0..3 {
            let key = FanOutNode::branch_identity("Source", i);
            assert_eq!(out.nodes.get(&key), Some(&serde_json::json!({ "i": i })));
        }

        // Old (pre-with_identity) last-write-wins behavior would have all 3
        // branches collide on the shared type name "SourceNode" and only
        // one survive. Assert that collision key never appears, and that
        // all 3 distinct entries survive.
        assert!(!out.nodes.contains_key("SourceNode"));
        assert_eq!(out.nodes.len(), 3);
    }

    #[tokio::test]
    async fn fan_out_reports_its_own_identity() {
        let fan_out = FanOutNode::new("FanOut", "Source", 1, |_| {
            Box::new(SourceNode {
                value: serde_json::json!({}),
            }) as Box<dyn Node>
        });

        assert_eq!(fan_out.name(), "FanOut");
    }

    #[tokio::test]
    async fn branch_identity_is_deterministic_and_index_ordered() {
        assert_eq!(FanOutNode::branch_identity("Source", 0), "Source[0]");
        assert_eq!(FanOutNode::branch_identity("Source", 1), "Source[1]");
        assert_eq!(FanOutNode::branch_identity("Source", 2), "Source[2]");
    }
}
