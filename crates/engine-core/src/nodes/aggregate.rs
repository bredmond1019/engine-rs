//! `AggregateNode` (`EN.6.G` task 1) — joins the N identity-distinguished
//! `ctx.nodes` entries a [`crate::nodes::fan_out::FanOutNode`] produced into
//! one deterministically-ordered `Vec<serde_json::Value>` under its own
//! output key.
//!
//! Order is driven by the caller-supplied list of source identities (or, via
//! [`AggregateNode::for_fan_out`], the exact `"{base_name}[{i}]"` sequence
//! `FanOutNode` would have produced for `0..count`) — never by `HashMap`
//! iteration order, which `TaskContext::nodes`'s `HashMap<String, Value>`
//! backing does not guarantee.

use engine_contract::TaskContext;

use crate::node::{Node, NodeError};
use crate::nodes::fan_out::FanOutNode;

/// How an `AggregateNode` reacts when one of its declared source identities
/// is absent from `ctx.nodes`.
///
/// The default, [`MissingSource::Fail`], is today's behavior byte-for-byte:
/// a missing source hard-errors with the existing message. Opting into
/// [`MissingSource::Skip`] is the pairing partner for
/// [`crate::parallel::BranchFailure::Tolerate`] — a `Tolerate` fan-out feeding
/// a `Fail` aggregate reintroduces the exact bug one node later, so the two
/// settings are meant to be adopted together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingSource {
    /// Hard-error on any absent source, with the existing message. This is
    /// the default and today's behavior.
    #[default]
    Fail,
    /// Omit absent sources from the output, preserving the declared order
    /// (from `source_identities`, never `HashMap` iteration order) of the
    /// sources that are present.
    Skip,
}

/// Joins a fixed, ordered list of `ctx.nodes` source identities into one
/// `Vec<serde_json::Value>` under `output_key`, preserving the caller's
/// declared order regardless of `HashMap` iteration order.
pub struct AggregateNode {
    identity: String,
    source_identities: Vec<String>,
    output_key: String,
    missing_source: MissingSource,
}

impl AggregateNode {
    /// Build an `AggregateNode` under `identity` that reads exactly
    /// `source_identities` (in that order) out of `ctx.nodes` and writes
    /// the joined array back under `identity` itself.
    ///
    /// Defaults to [`MissingSource::Fail`] — today's behavior.
    pub fn new(identity: impl Into<String>, source_identities: Vec<String>) -> Self {
        let identity = identity.into();
        Self {
            output_key: identity.clone(),
            identity,
            source_identities,
            missing_source: MissingSource::default(),
        }
    }

    /// Set this node's [`MissingSource`] mode.
    #[must_use]
    pub fn with_missing_source(mut self, missing_source: MissingSource) -> Self {
        self.missing_source = missing_source;
        self
    }

    /// Build an `AggregateNode` reading exactly the `count` fan-out branch
    /// identities [`FanOutNode::branch_identity`] would produce for
    /// `base_name` over indices `0..count`, in that deterministic order —
    /// the common case of aggregating a `FanOutNode`'s direct output.
    #[must_use]
    pub fn for_fan_out(identity: impl Into<String>, base_name: &str, count: usize) -> Self {
        let source_identities = (0..count)
            .map(|i| FanOutNode::branch_identity(base_name, i))
            .collect();
        Self::new(identity, source_identities)
    }
}

#[async_trait::async_trait]
impl Node for AggregateNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut results = Vec::with_capacity(self.source_identities.len());
        for source in &self.source_identities {
            match ctx.nodes.get(source).cloned() {
                Some(value) => results.push(value),
                None => match self.missing_source {
                    MissingSource::Fail => {
                        return Err(NodeError::new(format!(
                            "AggregateNode '{}': missing source '{source}' in ctx.nodes",
                            self.identity
                        )));
                    }
                    MissingSource::Skip => {
                        // Omit this source; declared order over the
                        // remaining present sources is preserved because we
                        // walk `source_identities` in order and only push
                        // when present.
                    }
                },
            }
        }

        ctx.nodes
            .insert(self.output_key.clone(), serde_json::Value::Array(results));
        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::fan_out::FanOutNode;
    use std::collections::HashMap;

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// Mirrors `fan_out::tests::SourceNode` — a trivial node that stamps
    /// `ctx.nodes` under its own default `name()`.
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
    async fn aggregate_joins_fan_out_results_in_deterministic_order() {
        let fan_out = FanOutNode::new("FanOut", "Source", 3, |i| {
            Box::new(SourceNode {
                value: serde_json::json!({ "i": i }),
            }) as Box<dyn Node>
        });
        let aggregate = AggregateNode::for_fan_out("Aggregate", "Source", 3);

        let ctx = fan_out
            .process(empty_context())
            .await
            .expect("fan-out should succeed");
        let ctx = aggregate
            .process(ctx)
            .await
            .expect("aggregate should succeed");

        assert_eq!(
            ctx.nodes.get("Aggregate"),
            Some(&serde_json::json!([{ "i": 0 }, { "i": 1 }, { "i": 2 }]))
        );
    }

    #[tokio::test]
    async fn aggregate_order_matches_declared_source_identities_not_insertion_order() {
        let mut ctx = empty_context();
        // Insert out of order — HashMap iteration order would not
        // necessarily reflect this, and must not be what AggregateNode uses.
        ctx.nodes
            .insert("Source[2]".to_string(), serde_json::json!("c"));
        ctx.nodes
            .insert("Source[0]".to_string(), serde_json::json!("a"));
        ctx.nodes
            .insert("Source[1]".to_string(), serde_json::json!("b"));

        let aggregate = AggregateNode::for_fan_out("Aggregate", "Source", 3);
        let out = aggregate
            .process(ctx)
            .await
            .expect("aggregate should succeed");

        assert_eq!(
            out.nodes.get("Aggregate"),
            Some(&serde_json::json!(["a", "b", "c"]))
        );
    }

    #[tokio::test]
    async fn aggregate_errors_when_a_source_identity_is_missing() {
        let ctx = empty_context();
        let aggregate = AggregateNode::new("Aggregate", vec!["Missing".to_string()]);

        let result = aggregate.process(ctx).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn aggregate_reports_its_own_identity() {
        let aggregate = AggregateNode::new("Aggregate", vec![]);
        assert_eq!(aggregate.name(), "Aggregate");
    }

    #[tokio::test]
    async fn aggregate_fail_default_preserves_existing_error_message() {
        let ctx = empty_context();
        let aggregate = AggregateNode::new("Aggregate", vec!["Missing".to_string()]);

        let err = aggregate
            .process(ctx)
            .await
            .expect_err("missing source should hard-error under the default Fail mode");

        assert_eq!(
            err.to_string(),
            "AggregateNode 'Aggregate': missing source 'Missing' in ctx.nodes"
        );
    }

    #[tokio::test]
    async fn aggregate_skip_omits_a_missing_middle_source_and_preserves_declared_order() {
        // A middle source ("Source[1]") is absent. An implementation that
        // compacts by index rather than walking declared identities would
        // pass a last-entry-missing test and fail only here.
        let mut ctx = empty_context();
        ctx.nodes
            .insert("Source[0]".to_string(), serde_json::json!("a"));
        ctx.nodes
            .insert("Source[2]".to_string(), serde_json::json!("c"));

        let aggregate = AggregateNode::new(
            "Aggregate",
            vec![
                "Source[0]".to_string(),
                "Source[1]".to_string(),
                "Source[2]".to_string(),
            ],
        )
        .with_missing_source(MissingSource::Skip);

        let out = aggregate
            .process(ctx)
            .await
            .expect("skip mode should not error on a missing source");

        assert_eq!(
            out.nodes.get("Aggregate"),
            Some(&serde_json::json!(["a", "c"]))
        );
    }

    #[tokio::test]
    async fn aggregate_skip_with_no_sources_missing_matches_fail_output() {
        let aggregate = AggregateNode::for_fan_out("Aggregate", "Source", 3)
            .with_missing_source(MissingSource::Skip);
        let fan_out = FanOutNode::new("FanOut", "Source", 3, |i| {
            Box::new(SourceNode {
                value: serde_json::json!({ "i": i }),
            }) as Box<dyn Node>
        });

        let ctx = fan_out
            .process(empty_context())
            .await
            .expect("fan-out should succeed");
        let ctx = aggregate
            .process(ctx)
            .await
            .expect("aggregate should succeed");

        assert_eq!(
            ctx.nodes.get("Aggregate"),
            Some(&serde_json::json!([{ "i": 0 }, { "i": 1 }, { "i": 2 }]))
        );
    }
}
