//! `TerminalObserveNode` — single-shot capture + detect against a session
//! `TerminalSessionNode` (task 2) already ensured (`EN.9.D` task 4).
//! READ-ONLY: exactly one `capture_pane`, one `term_core::detect::detect`,
//! no polling, no waits, no sends.
//!
//! Session identity is resolved through the same `session_input:
//! InputBinding` field convention `identity.rs` (task 1) establishes —
//! [`super::identity::WithSessionInput::with_session_input_from`], never
//! `crate::node::NodeExt::with_input_from` (inert passthrough, block record
//! `N4`). Unbound, it falls back to [`super::session::NODE_NAME`] — the
//! `TerminalSessionNode` this graph is expected to run after
//! (`terminal_probe/graph.rs`, task 5). The upstream node's stored
//! `session_name` is read from its `ctx.nodes` entry, following the
//! `revise.rs:59-69` read-preference-with-fallback shape.
//!
//! The captured screen is bounded via task 3's [`super::pane::bound_pane_tail`]
//! BEFORE anything is stamped into `ctx` — an unbounded capture must never
//! reach `put_result`, since `ctx` is what gets serialized to jsonb twice
//! per node. The policy applied is [`super::pane::default_pane_tail_policy`]
//! resolved against whether the upstream session was adopted: the upstream
//! node's `created` flag is `false` exactly when this run did not create
//! the session (a back-edge reuse of an existing session, or a session this
//! run never made at all) — `adopted = !created`.
//!
//! `usage` stays `null` and `cost_usd` is NEVER stamped (CLAUDE.md standing
//! rule 6's "stamp the resolved value" cuts the other way here: this node
//! calls no model, and a stamped zero would be indistinguishable from a
//! real zero-cost model call in `PolicyAggregate`). What IS stamped,
//! following standing rule 6, is the resolved `PaneTailPolicy` value.

use std::sync::{Arc, LazyLock};

use engine_contract::TaskContext;
use serde_json::Value;
use term_core::detect::manifest::{parse_manifest, CompiledManifest};
use term_core::detect::{detect, AgentDetection, CLAUDE_MANIFEST_TOML};
use term_core::driver::TerminalDriver;

use crate::node::{InputBinding, Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::identity::HasSessionInput;
use super::pane::{bound_pane_tail, default_pane_tail_policy, PaneLimits};
use super::session;

/// The `Node::name()` identity `TerminalObserveNode` runs under, and the
/// `ctx.nodes` key its output is stamped onto.
pub const NODE_NAME: &str = "TerminalObserveNode";

/// The production Claude detect manifest, compiled once and reused across
/// every `process` call — compiling recompiles regexes for nothing on a
/// manifest that never changes at runtime.
static CLAUDE_MANIFEST: LazyLock<CompiledManifest> = LazyLock::new(|| {
    parse_manifest(CLAUDE_MANIFEST_TOML)
        .expect("CLAUDE_MANIFEST_TOML is a fixed, valid manifest")
        .compile()
        .expect("CLAUDE_MANIFEST_TOML's rules are fixed, valid gates")
});

/// One `capture_pane`, one `detect`, single shot — no polling, no waits, no
/// sends.
pub struct TerminalObserveNode {
    driver: Arc<dyn TerminalDriver>,
    /// Resolves which `TerminalSessionNode` (by `ctx.nodes` identity) this
    /// node reads its target session name from. Unbound falls back to
    /// [`session::NODE_NAME`].
    session_input: InputBinding,
}

impl TerminalObserveNode {
    /// Construct with the given driver and an unbound `session_input`
    /// (falls back to [`session::NODE_NAME`]).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            session_input: InputBinding::unbound(),
        }
    }

    /// Read the upstream `TerminalSessionNode`'s stored `session_name` and
    /// `created` flag from `ctx.nodes`, per the bound (or defaulted)
    /// `session_input` identity.
    fn read_upstream_session(&self, ctx: &TaskContext) -> Result<(String, bool), NodeError> {
        let bound = self.session_input.resolve(session::NODE_NAME);
        let stored = get_result(ctx, bound).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no session recorded by {bound} — TerminalObserveNode must run \
                 after a TerminalSessionNode"
            ))
        })?;
        let session_name = stored
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {bound}'s stored result is missing `session_name`"
                ))
            })?;
        let created = stored
            .get("created")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((session_name, created))
    }
}

impl HasSessionInput for TerminalObserveNode {
    fn session_input_mut(&mut self) -> &mut InputBinding {
        &mut self.session_input
    }
}

#[async_trait::async_trait]
impl Node for TerminalObserveNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let (session_name, created) = self.read_upstream_session(&ctx)?;

        let screen = self
            .driver
            .capture_pane(&session_name)
            .await
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: capture_pane failed: {err}")))?;

        let AgentDetection {
            state,
            blocked_reason,
            ..
        } = detect(&screen, &CLAUDE_MANIFEST);

        // adopted = !created: this run did not create the session (a
        // back-edge reuse, or a session it never made at all) exactly when
        // the upstream `TerminalSessionNode` reported `created: false`.
        let adopted = !created;
        let policy = default_pane_tail_policy(adopted);
        let bounded = bound_pane_tail(&screen, policy, PaneLimits::default());

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "session_name": session_name,
                "state": state.as_str(),
                "blocked_reason": blocked_reason.map(|r| r.as_str()),
                "pane_tail": bounded.pane_tail,
                "pane_sha256": bounded.pane_sha256,
                "pane_truncated": bounded.pane_truncated,
                "pane_tail_policy": policy.as_str(),
                "usage": Value::Null,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::WithSessionInput;
    use super::*;
    use term_core::driver::{StubOutcome, StubTerminalDriver};

    fn ctx_with_upstream_session(session_name: &str, created: bool) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.nodes.insert(
            session::NODE_NAME.to_string(),
            serde_json::json!({
                "session_name": session_name,
                "lease_nonce": "some-nonce",
                "created": created,
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn issues_exactly_one_capture_pane_and_one_detect_per_run() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("idle prompt output".to_string()));
        let node = TerminalObserveNode::new(driver.clone());

        let ctx = node
            .process(ctx_with_upstream_session(
                "eng-run1_TerminalSessionNode",
                true,
            ))
            .await
            .unwrap();

        let capture_calls = driver
            .calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("capture-pane"))
            .count();
        assert_eq!(capture_calls, 1, "expected exactly one capture_pane call");

        let stored = ctx.nodes.get(NODE_NAME).unwrap();
        assert!(
            stored.get("state").is_some(),
            "expected a detect state to be stamped"
        );
    }

    #[tokio::test]
    async fn stamped_result_carries_bounded_pane_fields_and_resolved_policy() {
        let driver = Arc::new(StubTerminalDriver::new());
        let raw: String = (0..2000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        driver.set_capture_pane_result(StubOutcome::Ok(raw));
        let node = TerminalObserveNode::new(driver.clone());

        // created: true -> not adopted -> Text policy -> pane_tail present.
        let ctx = node
            .process(ctx_with_upstream_session(
                "eng-run1_TerminalSessionNode",
                true,
            ))
            .await
            .unwrap();

        let stored = ctx.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["pane_truncated"], serde_json::json!(true));
        assert!(stored["pane_sha256"].is_string());
        assert!(stored["pane_tail"].is_string());
        assert_eq!(stored["pane_tail_policy"], serde_json::json!("text"));
    }

    #[tokio::test]
    async fn adopted_session_resolves_to_hash_only_policy() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("some output".to_string()));
        let node = TerminalObserveNode::new(driver.clone());

        // created: false -> adopted -> HashOnly policy -> no pane_tail text.
        let ctx = node
            .process(ctx_with_upstream_session(
                "eng-run1_TerminalSessionNode",
                false,
            ))
            .await
            .unwrap();

        let stored = ctx.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["pane_tail"], serde_json::Value::Null);
        assert!(stored["pane_sha256"].is_string());
        assert_eq!(stored["pane_tail_policy"], serde_json::json!("hash-only"));
    }

    #[tokio::test]
    async fn usage_is_null_and_no_cost_usd_key_is_ever_stamped() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("output".to_string()));
        let node = TerminalObserveNode::new(driver.clone());

        let ctx = node
            .process(ctx_with_upstream_session(
                "eng-run1_TerminalSessionNode",
                true,
            ))
            .await
            .unwrap();

        let stored = ctx.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["usage"], serde_json::Value::Null);
        assert!(
            stored.get("cost_usd").is_none(),
            "cost_usd must never be stamped by TerminalObserveNode"
        );
    }

    #[tokio::test]
    async fn with_session_input_from_binds_to_a_specific_upstream_identity() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("output".to_string()));
        let node = TerminalObserveNode::new(driver.clone())
            .with_session_input_from("SomeOtherSessionNode");

        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.nodes.insert(
            "SomeOtherSessionNode".to_string(),
            serde_json::json!({
                "session_name": "eng-bound-session",
                "created": true,
            }),
        );

        let result = node.process(ctx).await.unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(
            stored["session_name"],
            serde_json::json!("eng-bound-session")
        );
    }

    #[tokio::test]
    async fn missing_upstream_session_surfaces_as_node_error() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = TerminalObserveNode::new(driver);

        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };

        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "expected a missing upstream session to error"
        );
    }
}
