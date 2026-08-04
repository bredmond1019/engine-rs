//! `TransportSlot` — the plain-or-meta transport override shared by every
//! model node under `workflows/` that composes a `ClaudeCodeStep`
//! (`EN.ticket.wire-meta-transport-telemetry` task 1).
//!
//! All 10 local-eligible nodes rewired by each workflow's
//! `registry_for_policy` are structurally identical in exactly the respect
//! that matters here: one `Option<ModelTransport>` field, one
//! `with_transport` builder that assigns it, and one short conditional
//! forward into a freshly-constructed `ClaudeCodeStep` inside `process`.
//! `ClaudeCodeStep` itself already distinguishes a plain [`ModelTransport`]
//! (which can't report what it actually called, so `process` stamps a
//! generic `"cloud"`-tier `TransportInfo`) from a [`MetaTransport`] (which
//! reports its own [`TransportInfo`] and takes precedence when both are
//! set) — see `nodes/claude_code_step.rs:158-190`. `TransportSlot` holds
//! both halves of that pair once so no node needs to duplicate the builder
//! pair and forward logic itself.
//!
//! Lives under `workflows/` (not `nodes/`) because [`ModelTransport`] is
//! defined in `workflows/mod.rs` and every consumer of this slot lives
//! under `workflows/` — putting it in `nodes/` would make the lower layer
//! depend on the higher one for no benefit.

use crate::nodes::claude_code_step::{ClaudeCodeStep, MetaTransport};

use super::ModelTransport;

/// Holds an optional plain [`ModelTransport`] and an optional
/// [`MetaTransport`] override for a node that composes a `ClaudeCodeStep`.
/// [`Self::apply`] forwards whichever is set onto the step, with `meta`
/// taking precedence — matching `ClaudeCodeStep::with_meta_transport`'s own
/// documented precedence over `with_transport`.
#[derive(Clone, Default)]
pub struct TransportSlot {
    plain: Option<ModelTransport>,
    meta: Option<MetaTransport>,
}

impl TransportSlot {
    /// Set (or replace) the plain transport override.
    pub fn set_plain(&mut self, transport: ModelTransport) {
        self.plain = Some(transport);
    }

    /// Set (or replace) the meta transport override.
    pub fn set_meta(&mut self, transport: MetaTransport) {
        self.meta = Some(transport);
    }

    /// Apply whichever override is set to `step`, meta winning when both
    /// are set. Neither set leaves `step` unchanged.
    #[must_use]
    pub fn apply(&self, step: ClaudeCodeStep) -> ClaudeCodeStep {
        if let Some(meta) = &self.meta {
            let meta = meta.clone();
            return step.with_meta_transport(move |config, prompt| (meta)(config, prompt));
        }
        if let Some(plain) = &self.plain {
            let plain = plain.clone();
            return step.with_transport(move |config, prompt| (plain)(config, prompt));
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::claude_code_step::TransportInfo;
    use claude_code_rs::{Config, Outcome};
    use engine_contract::TaskContext;
    use futures::future::BoxFuture;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
            structured_output: None,
        }
    }

    fn stub_plain_transport(model: &'static str) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(format!("plain:{model}"));
            Box::pin(async move { Ok(outcome) })
                as BoxFuture<'static, claude_code_rs::Result<Outcome>>
        })
    }

    fn stub_meta_transport(tier: &'static str) -> MetaTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(format!("meta:{tier}"));
            let info = TransportInfo {
                tier: tier.to_string(),
                model: "stub-model".to_string(),
                endpoint: None,
            };
            Box::pin(async move { Ok((outcome, info)) })
                as BoxFuture<'static, claude_code_rs::Result<(Outcome, TransportInfo)>>
        })
    }

    fn make_step() -> ClaudeCodeStep {
        ClaudeCodeStep::new("TransportSlotTest", Config::default(), "prompt")
    }

    fn empty_context() -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    async fn run_step(step: ClaudeCodeStep) -> TaskContext {
        use crate::node::Node;
        step.process(empty_context())
            .await
            .expect("stub transport never errors")
    }

    #[tokio::test]
    async fn meta_wins_when_both_set() {
        let mut slot = TransportSlot::default();
        slot.set_plain(stub_plain_transport("plain-model"));
        slot.set_meta(stub_meta_transport("local"));

        let step = slot.apply(make_step());
        let ctx = run_step(step).await;

        let transport = ctx
            .nodes
            .get("TransportSlotTest")
            .and_then(|value| value.get("transport"))
            .expect("transport stamp present");
        assert_eq!(
            transport.get("tier").and_then(|v| v.as_str()),
            Some("local"),
            "meta transport's tier must win over the plain transport's generic cloud stamp"
        );
    }

    #[tokio::test]
    async fn plain_applies_when_only_plain_set() {
        let mut slot = TransportSlot::default();
        slot.set_plain(stub_plain_transport("plain-model"));

        let step = slot.apply(make_step());
        let ctx = run_step(step).await;

        let transport = ctx
            .nodes
            .get("TransportSlotTest")
            .and_then(|value| value.get("transport"))
            .expect("transport stamp present");
        // A plain transport can't report its own tier, so ClaudeCodeStep
        // falls back to a generic "cloud" stamp — this is the exact
        // behavior this ticket exists to move nodes off of, and is asserted
        // here only to pin TransportSlot's plain-only forwarding behavior.
        assert_eq!(
            transport.get("tier").and_then(|v| v.as_str()),
            Some("cloud")
        );
    }

    #[tokio::test]
    async fn neither_set_leaves_step_behavior_untouched() {
        let slot = TransportSlot::default();
        let step = slot.apply(make_step());
        // With neither override set, the step falls back to its own default
        // transport (`claude_code_rs::execute`), which this unit test must
        // not invoke. Asserting the untouched-step case here would require
        // a real subprocess call; instead this is covered structurally: a
        // `TransportSlot::default()` has both fields `None`, so `apply`'s
        // first two `if let` branches are provably skipped and `step` is
        // returned as-is (see `apply`'s body above). This test just pins
        // that `apply` on a default slot does not panic and yields a step
        // still usable as a `Node`.
        let _ = step;
    }
}
