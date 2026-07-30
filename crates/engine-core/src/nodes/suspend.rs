//! `SuspendNode` (`EN.6.F` task 5) — the workflow-authored half of suspend/
//! resume. A `Node` cannot stop the walk itself (`Node::process` only
//! transforms a `TaskContext`), so `process` only *requests* suspension via
//! [`crate::suspend::request_suspension`]; `Workflow::walk` (`EN.6.F` task 4)
//! is the only thing that finalizes a suspend and picks the resume pointer.
//!
//! Modeled on `MaterializeDocNode::with_enabled`
//! (`crate::nodes::materialize_doc`, `:137`/`:184`): `enabled: false` is the
//! default and is an **in-place no-op** — the node stays in the declared
//! graph at every setting, so the node set never varies by policy (standing
//! rule 6). The resolved `enabled` value is always stamped into
//! `ctx.nodes[self.name()]` so telemetry can attribute behavior to it.
//!
//! **No `harness.json`/named-profile entry for `enabled` yet.** Standing
//! rule 6 requires every policy knob to appear in the three named profiles,
//! but no production workflow adopts `SuspendNode` in this block — there is
//! nothing to route to it, and documenting a knob nothing reads would be
//! worse than waiting for `EN.6.H` (see `tasks.md`'s CLAUDE.md-sections
//! note). This is a deliberate, spec-sanctioned exception, not a rule-6
//! miss.

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::put_result;

/// The `Node::name()` identity `SuspendNode` runs under by default, and the
/// `ctx.nodes` key its result is stamped onto when no identity override
/// (`NodeExt::with_identity`) is applied.
pub const DEFAULT_IDENTITY: &str = "SuspendNode";

/// The gate a [`SuspendNode`] optionally consults in addition to `enabled`.
type SuspendPredicate = Box<dyn Fn(&TaskContext) -> bool + Send + Sync>;

/// Requests that the walk suspend after this node finishes, when `enabled`
/// (and, if set, `predicate`) says so. Finalization — computing `resume_at`,
/// stamping the marker, snapshotting the ledger — belongs entirely to
/// `Workflow::walk`; this node only flips
/// `metadata.suspension.requested = true`.
pub struct SuspendNode {
    identity: String,
    enabled: bool,
    predicate: Option<SuspendPredicate>,
    reason_label: String,
}

impl SuspendNode {
    /// Construct under `identity`, `enabled: false` (behavior-stable
    /// default — a `SuspendNode` added to a graph does nothing until
    /// explicitly turned on), no predicate (an enabled node always
    /// requests), and an empty reason label.
    #[must_use]
    pub fn new(identity: impl Into<String>) -> Self {
        Self {
            identity: identity.into(),
            enabled: false,
            predicate: None,
            reason_label: String::new(),
        }
    }

    /// Enable/disable the node (default `false`). When `false`, `process`
    /// never requests suspension regardless of `predicate` — a verified
    /// in-place no-op.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Gate suspension on `ctx` at process time, in addition to `enabled`.
    /// A predicate returning `false` suppresses suspension even when
    /// `enabled` is `true`. Absent (the default), an enabled node always
    /// requests suspension.
    #[must_use]
    pub fn with_predicate(
        mut self,
        predicate: impl Fn(&TaskContext) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.predicate = Some(Box::new(predicate));
        self
    }

    /// A human-readable label carried into the stamped result (and, via
    /// `Workflow::walk`'s finalization, available for operator-facing
    /// surfaces) describing why this node requests suspension.
    #[must_use]
    pub fn with_reason_label(mut self, label: impl Into<String>) -> Self {
        self.reason_label = label.into();
        self
    }
}

#[async_trait::async_trait]
impl Node for SuspendNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let mut ctx = ctx;
        let should = self.enabled && self.predicate.as_ref().map_or(true, |p| p(&ctx));
        if should {
            crate::suspend::request_suspension(&mut ctx.metadata);
        }
        // Standing rule 6: stamp the RESOLVED knob so telemetry can
        // attribute observed behavior to the setting that caused it.
        put_result(
            &mut ctx,
            self.name(),
            json!({
                "suspended": should,
                "enabled": self.enabled,
                "reason": self.reason_label,
            }),
        );
        Ok(ctx)
    }

    fn name(&self) -> &str {
        &self.identity
    }

    // Not a `Router` — `as_router()` stays the trait default (`None`).
}

impl Default for SuspendNode {
    fn default() -> Self {
        Self::new(DEFAULT_IDENTITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeExt;
    use crate::workflows::get_result;

    fn ctx() -> TaskContext {
        TaskContext {
            event: json!({}),
            nodes: Default::default(),
            metadata: json!({}),
            node_runs: Default::default(),
        }
    }

    #[tokio::test]
    async fn default_is_a_no_op_that_stamps_suspended_false_and_leaves_metadata_unrequested() {
        let node = SuspendNode::new(DEFAULT_IDENTITY);
        let out = node.process(ctx()).await.expect("process succeeds");

        assert!(!crate::suspend::suspension_requested(&out.metadata));
        assert!(!crate::suspend::is_suspended(&out.metadata));

        let stamped = get_result(&out, DEFAULT_IDENTITY).expect("result stamped");
        assert_eq!(stamped["suspended"], json!(false));
        assert_eq!(stamped["enabled"], json!(false));
    }

    #[tokio::test]
    async fn with_enabled_true_requests_suspension_and_stamps_suspended_true() {
        let node = SuspendNode::new(DEFAULT_IDENTITY).with_enabled(true);
        let out = node.process(ctx()).await.expect("process succeeds");

        assert!(crate::suspend::suspension_requested(&out.metadata));

        let stamped = get_result(&out, DEFAULT_IDENTITY).expect("result stamped");
        assert_eq!(stamped["suspended"], json!(true));
        assert_eq!(stamped["enabled"], json!(true));
    }

    #[tokio::test]
    async fn a_false_predicate_suppresses_suspension_even_when_enabled() {
        let node = SuspendNode::new(DEFAULT_IDENTITY)
            .with_enabled(true)
            .with_predicate(|_ctx| false);
        let out = node.process(ctx()).await.expect("process succeeds");

        assert!(!crate::suspend::suspension_requested(&out.metadata));

        let stamped = get_result(&out, DEFAULT_IDENTITY).expect("result stamped");
        assert_eq!(stamped["suspended"], json!(false));
        // The resolved `enabled` value is present in every case, even when
        // the predicate suppresses the actual request.
        assert_eq!(stamped["enabled"], json!(true));
    }

    #[tokio::test]
    async fn a_true_predicate_allows_suspension_when_enabled() {
        let node = SuspendNode::new(DEFAULT_IDENTITY)
            .with_enabled(true)
            .with_predicate(|_ctx| true);
        let out = node.process(ctx()).await.expect("process succeeds");

        assert!(crate::suspend::suspension_requested(&out.metadata));
    }

    #[tokio::test]
    async fn resolved_enabled_is_present_in_ctx_nodes_in_every_case() {
        for enabled in [false, true] {
            let node = SuspendNode::new(DEFAULT_IDENTITY).with_enabled(enabled);
            let out = node.process(ctx()).await.expect("process succeeds");
            let stamped = get_result(&out, DEFAULT_IDENTITY).expect("result stamped");
            assert_eq!(stamped["enabled"], json!(enabled));
        }
    }

    #[tokio::test]
    async fn identity_override_via_with_identity_relabels_the_stamp() {
        let node = SuspendNode::new(DEFAULT_IDENTITY)
            .with_enabled(true)
            .with_identity("SuspendNode#gate-1");
        let out = node.process(ctx()).await.expect("process succeeds");

        assert!(get_result(&out, DEFAULT_IDENTITY).is_none());
        let stamped = get_result(&out, "SuspendNode#gate-1").expect("relabeled result stamped");
        assert_eq!(stamped["suspended"], json!(true));
    }

    #[tokio::test]
    async fn reason_label_is_carried_into_the_stamped_result() {
        let node = SuspendNode::new(DEFAULT_IDENTITY)
            .with_enabled(true)
            .with_reason_label("awaiting operator approval");
        let out = node.process(ctx()).await.expect("process succeeds");

        let stamped = get_result(&out, DEFAULT_IDENTITY).expect("result stamped");
        assert_eq!(stamped["reason"], json!("awaiting operator approval"));
    }
}
