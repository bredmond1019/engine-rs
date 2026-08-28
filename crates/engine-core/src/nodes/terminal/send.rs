//! `TerminalSendNode` — the guarded, floor-checked terminal write node
//! (`EN.9.E` task 2).
//!
//! One send goes through, in order: `crate::policy::command_floor`
//! (a fixed 5-regex denylist floor against ACCIDENTS, never described here
//! as an authorization boundary — it has no de-obfuscation normalizer and
//! known-obfuscated variants pass it; see `command_floor`'s own module doc),
//! then `send_id` back-edge idempotency (a repeated `send_id` is a no-op
//! success, zero driver send calls), then the session lease re-verified as
//! still ours via [`term_core::lease::SessionLease::renew`] (fail-closed:
//! `NotOurs`/expired/foreign all refuse the send), all three held under one
//! per-session `tokio::sync::Mutex<()>` so two concurrent `process` calls
//! against the same session can never interleave keystrokes into one pane.
//!
//! Session identity is resolved through the same `session_input:
//! InputBinding` convention `identity.rs`/`observe.rs` establish, falling
//! back to [`super::session::NODE_NAME`]. `command`/`send_id` are read off
//! `ctx.event` — this node's own per-run arguments, not an upstream node's
//! stored result — following `opportunity_edit.rs`'s "read the edit's
//! arguments off `ctx.event`" precedent.
//!
//! Idempotency state (the last `send_id` this session accepted) is written
//! to a tmux user-option namespaced by session name — OUTSIDE `ctx`,
//! deliberately, mirroring `session.rs`'s `ORDERING IS THE POINT` doc: a
//! back-edge re-entry may arrive with a freshly-constructed `ctx` (the
//! `TerminalSessionNode` tests already establish that pattern), so
//! anything the node needs to survive across re-entries must live in
//! driver-observable state, not in `ctx.nodes`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use engine_contract::TaskContext;
use serde_json::Value;
use term_core::driver::{GuardedSendRequest, GuardedSender, SendError, TerminalDriver};
use tokio::sync::Mutex as AsyncMutex;

use crate::node::{InputBinding, Node, NodeError};
use crate::policy::command_floor::{self, CommandDecision};
use crate::workflow::read_run_id;
use crate::workflows::{get_result, put_result};

use super::identity::HasSessionInput;
use super::session::{self, DEFAULT_LEASE_TTL};

/// The `Node::name()` identity `TerminalSendNode` runs under, and the
/// `ctx.nodes` key its output is stamped onto.
pub const NODE_NAME: &str = "TerminalSendNode";

/// The tmux user-option base name the last-accepted `send_id` is stamped
/// under, namespaced per session exactly like `session.rs`'s
/// `ENGINE_RUN_ID_OPTION`/`ENGINE_CREATED_AT_OPTION`.
pub const LAST_SEND_ID_OPTION: &str = "@engine_last_send_id";

fn namespaced_option(base: &str, session_name: &str) -> String {
    format!("{base}@{session_name}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The guarded, floor-checked terminal write node: `command_floor`, `send_id`
/// idempotency, and lease re-verification, all under a per-session mutex.
pub struct TerminalSendNode {
    driver: Arc<dyn TerminalDriver>,
    /// Resolves which `TerminalSessionNode` (by `ctx.nodes` identity) this
    /// node reads its target session name and lease nonce from. Unbound
    /// falls back to [`session::NODE_NAME`].
    session_input: InputBinding,
    /// One `tokio::sync::Mutex<()>` per session name, held across the
    /// idempotency check, lease renewal, and send — the "two nodes cannot
    /// interleave keystrokes into one pane" guard. Keyed by session name
    /// rather than a single global lock so sends against unrelated
    /// sessions never contend with each other. Guarded by a short-lived
    /// `std::sync::Mutex` only to get-or-insert the per-session entry;
    /// the entry itself is the async lock actually held across the send.
    session_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl TerminalSendNode {
    /// Construct with the given driver and an unbound `session_input`
    /// (falls back to [`session::NODE_NAME`]).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            session_input: InputBinding::unbound(),
            session_locks: StdMutex::new(HashMap::new()),
        }
    }

    /// Read the upstream `TerminalSessionNode`'s stored `session_name` and
    /// `lease_nonce` from `ctx.nodes`, per the bound (or defaulted)
    /// `session_input` identity — the same read-preference-with-fallback
    /// shape `observe.rs` uses.
    fn read_upstream_session(&self, ctx: &TaskContext) -> Result<(String, String), NodeError> {
        let bound = self.session_input.resolve(session::NODE_NAME);
        let stored = get_result(ctx, bound).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no session recorded by {bound} — TerminalSendNode must run \
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
        let lease_nonce = stored
            .get("lease_nonce")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {bound}'s stored result is missing `lease_nonce`"
                ))
            })?;
        Ok((session_name, lease_nonce))
    }

    /// Read this send's own arguments — `command` and `send_id` — off
    /// `ctx.event`, following `opportunity_edit.rs`'s precedent for
    /// per-run node arguments that are not an upstream node's result.
    fn read_send_args(ctx: &TaskContext) -> Result<(String, String), NodeError> {
        let command = ctx
            .event
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!("{NODE_NAME}: ctx.event is missing `command`"))
            })?;
        let send_id = ctx
            .event
            .get("send_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!("{NODE_NAME}: ctx.event is missing `send_id`"))
            })?;
        Ok((command, send_id))
    }

    /// Get-or-create this session's per-session async lock. The
    /// `std::sync::Mutex` guarding the map is held only long enough to
    /// clone/insert an `Arc`, never across the `.lock().await` on the
    /// returned mutex itself.
    fn session_lock(&self, session_name: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks
            .entry(session_name.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

impl HasSessionInput for TerminalSendNode {
    fn session_input_mut(&mut self) -> &mut InputBinding {
        &mut self.session_input
    }
}

#[async_trait::async_trait]
impl Node for TerminalSendNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let run_id = read_run_id(&ctx.metadata).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no run_id stamped on ctx.metadata — cannot verify the lease"
            ))
        })?;
        let (session_name, lease_nonce) = self.read_upstream_session(&ctx)?;
        let (command, send_id) = Self::read_send_args(&ctx)?;

        // The command floor is a fixed, non-configurable safety floor
        // against ACCIDENTS (5 regex rules, no de-obfuscation normalizer)
        // — never an authorization boundary. Checked before anything else:
        // a denied command must never reach the lease check, the mutex, or
        // the driver.
        if let CommandDecision::Deny { reason, matched } = command_floor::evaluate_command(&command)
        {
            return Err(NodeError::new(format!(
                "{NODE_NAME}: command refused by the command floor ({reason}): {matched:?}"
            )));
        }

        let lock = self.session_lock(&session_name);
        let _guard = lock.lock().await;

        // send_id idempotency: a re-entry carrying an already-recorded
        // send_id is a no-op success and issues zero driver send calls.
        // Read from a tmux user-option (driver-observable state), never
        // from `ctx`, so this holds even when a back-edge re-entry arrives
        // with a freshly-constructed `ctx`.
        let send_id_option = namespaced_option(LAST_SEND_ID_OPTION, &session_name);
        let already_sent = self
            .driver
            .show_option(&send_id_option)
            .await
            .ok()
            .is_some_and(|recorded| recorded == send_id);

        if already_sent {
            put_result(
                &mut ctx,
                self.name(),
                serde_json::json!({
                    "session_name": session_name,
                    "send_id": send_id,
                    "sent": false,
                    "deduplicated": true,
                }),
            );
            return Ok(ctx);
        }

        // Route the send through `GuardedSender`, which renews the lease
        // (fail-closed: a foreign or expired lease refuses the send before
        // anything reaches the driver), then consults the operator hold,
        // then performs the literal+Enter send with the `C-u` line-clear
        // recovery — all under its own per-session lock.
        let guarded = GuardedSender::new(self.driver.as_ref());
        guarded
            .send_keys(GuardedSendRequest {
                session_name: &session_name,
                keys: &command,
                run_id: &run_id,
                nonce: &lease_nonce,
                identity: NODE_NAME,
                lease_expires_at_ms: now_ms() + DEFAULT_LEASE_TTL.as_millis() as u64,
                now_ms: now_ms(),
            })
            .await
            .map_err(|err| match err {
                SendError::Lease(lease_err) => NodeError::new(format!(
                    "{NODE_NAME}: send refused — lease is no longer ours: {lease_err}"
                )),
                SendError::Hold(hold_err) => NodeError::new(format!(
                    "{NODE_NAME}: send refused by operator hold: {hold_err}"
                )),
                other => NodeError::new(format!("{NODE_NAME}: send_keys failed: {other}")),
            })?;

        // Record acceptance AFTER the send actually happened, so a send
        // that fails mid-way is never marked accepted.
        self.driver
            .set_option(&send_id_option, &send_id)
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to record send_id {send_id_option}: {err}"
                ))
            })?;

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "session_name": session_name,
                "send_id": send_id,
                "sent": true,
                "deduplicated": false,
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
    use super::*;
    use std::time::Duration;
    use term_core::driver::{StubOutcome, StubTerminalDriver};

    fn ctx_with_upstream_session(
        run_id: &str,
        session_name: &str,
        lease_nonce: &str,
        command: &str,
        send_id: &str,
    ) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::json!({
                "command": command,
                "send_id": send_id,
            }),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.metadata["run_id"] = serde_json::json!(run_id);
        ctx.nodes.insert(
            session::NODE_NAME.to_string(),
            serde_json::json!({
                "session_name": session_name,
                "lease_nonce": lease_nonce,
                "created": true,
            }),
        );
        ctx
    }

    /// Seed the driver so a lease `renew` under `nonce` succeeds: the
    /// read-back must show a live lease carrying that exact nonce.
    fn seed_live_lease(driver: &StubTerminalDriver, session_name: &str, nonce: &str) {
        let far_future = now_ms() + Duration::from_secs(3600).as_millis() as u64;
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(format!("some-run:{nonce}:TerminalSessionNode:{far_future}")),
        );
    }

    fn send_keys_calls(driver: &StubTerminalDriver) -> usize {
        driver
            .calls()
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("send-keys"))
            .count()
    }

    #[tokio::test]
    async fn rm_rf_is_refused_with_a_typed_error_and_no_send() {
        let driver = Arc::new(StubTerminalDriver::new());
        seed_live_lease(&driver, "eng-run1_TerminalSessionNode", "nonce-1");
        let node = TerminalSendNode::new(driver.clone());

        let ctx = ctx_with_upstream_session(
            "run-1",
            "eng-run1_TerminalSessionNode",
            "nonce-1",
            "rm -rf /",
            "send-1",
        );
        let result = node.process(ctx).await;

        assert!(result.is_err(), "expected rm -rf / to be refused");
        assert_eq!(
            send_keys_calls(&driver),
            0,
            "a refused command must issue zero send-keys calls"
        );
    }

    #[tokio::test]
    async fn allowed_command_is_sent_and_recorded() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        let node = TerminalSendNode::new(driver.clone());

        let ctx =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let out = node.process(ctx).await.unwrap();

        assert_eq!(send_keys_calls(&driver), 2);
        let stored = out.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["sent"], serde_json::json!(true));
        assert_eq!(stored["deduplicated"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn back_edge_with_repeated_send_id_issues_zero_send_keys_calls() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        let node = TerminalSendNode::new(driver.clone());

        let first =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        node.process(first).await.unwrap();
        assert_eq!(send_keys_calls(&driver), 2);

        // `StubTerminalDriver::show_option` returns one configured value
        // per name regardless of what `set_option` just wrote (it does not
        // model tmux's actual storage — see `session.rs`'s
        // `seed_own_lease` doc comment for the same caveat). Seed the
        // read-back a real tmux would now show for the send_id option this
        // node just wrote, so the second call's idempotency check observes
        // what production would.
        driver.set_show_option_result_for(
            format!("@engine_last_send_id@{session_name}"),
            StubOutcome::Ok("send-1".to_string()),
        );

        // Back-edge re-entry: a FRESH ctx (mirroring `session.rs`'s own
        // back-edge test), same send_id.
        let second =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let out = node.process(second).await.unwrap();

        assert_eq!(
            send_keys_calls(&driver),
            2,
            "a repeated send_id must issue zero additional send-keys calls"
        );
        let stored = out.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["sent"], serde_json::json!(false));
        assert_eq!(stored["deduplicated"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn a_different_send_id_on_the_same_session_still_sends() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        let node = TerminalSendNode::new(driver.clone());

        let first =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        node.process(first).await.unwrap();

        let second =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo test", "send-2");
        node.process(second).await.unwrap();

        assert_eq!(send_keys_calls(&driver), 4);
    }

    #[tokio::test]
    async fn foreign_lease_refuses_the_send() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        let far_future = now_ms() + Duration::from_secs(3600).as_millis() as u64;
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(format!("other-run:other-nonce:OtherNode:{far_future}")),
        );
        let node = TerminalSendNode::new(driver.clone());

        let ctx =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let result = node.process(ctx).await;

        assert!(
            result.is_err(),
            "expected a foreign lease to refuse the send"
        );
        assert_eq!(send_keys_calls(&driver), 0);
    }

    #[tokio::test]
    async fn expired_lease_with_no_steal_refuses_the_send() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        // Read-back shows nothing (never-set option) => `renew` treats it
        // as `NotOurs` since the nonce cannot match.
        let lease_option = format!("@engine_lease@{session_name}");
        driver.set_show_option_result_for(
            lease_option.clone(),
            StubOutcome::invalid_option(&lease_option),
        );
        let node = TerminalSendNode::new(driver.clone());

        let ctx =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let result = node.process(ctx).await;

        assert!(
            result.is_err(),
            "expected an absent/expired lease to refuse the send"
        );
        assert_eq!(send_keys_calls(&driver), 0);
    }

    #[tokio::test]
    async fn active_operator_hold_refuses_the_send_with_zero_driver_calls() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        driver.set_show_option_result_for(
            format!("@operator_hold@{session_name}"),
            StubOutcome::Ok("1".to_string()),
        );
        let node = TerminalSendNode::new(driver.clone());

        let ctx =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let result = node.process(ctx).await;

        let err = result.expect_err("expected an active operator hold to refuse the send");
        assert!(
            err.to_string().contains("operator hold"),
            "expected the error to name the operator hold, got: {err}"
        );
        assert_eq!(
            send_keys_calls(&driver),
            0,
            "an active operator hold must issue zero driver send-keys calls"
        );
    }

    #[tokio::test]
    async fn literal_succeeded_enter_failed_triggers_c_u_line_clear_recovery() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        driver.set_send_enter_result(StubOutcome::NoServer);
        let node = TerminalSendNode::new(driver.clone());

        let ctx =
            ctx_with_upstream_session("run-1", session_name, "nonce-1", "cargo build", "send-1");
        let result = node.process(ctx).await;

        assert!(result.is_err(), "expected the Enter failure to surface");
        let calls = driver.calls();
        assert!(
            calls.iter().any(|c| c.iter().any(|a| a == "C-u")),
            "expected the C-u line-clear recovery to have been sent, got: {calls:?}"
        );
    }

    /// Obfuscated bypasses are OUT OF SCOPE for `command_floor` (no
    /// de-obfuscation normalizer) — this test documents that gap
    /// explicitly rather than letting a future reader assume it is
    /// covered.
    #[tokio::test]
    async fn obfuscated_rm_rf_variants_currently_pass_the_floor_documented_gap() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = "eng-run1_TerminalSessionNode";
        seed_live_lease(&driver, session_name, "nonce-1");
        let node = TerminalSendNode::new(driver.clone());

        // `r'm -rf /` and a base64-encoded payload both dodge the literal
        // `\brm\b` pattern — `command_floor`'s own doc calls this out as a
        // known limitation, not a bypass this block is scoped to close.
        for (idx, command) in ["r\\m -rf /", "echo cm0gLXJmIC8= | base64 -d | sh"]
            .into_iter()
            .enumerate()
        {
            let ctx = ctx_with_upstream_session(
                "run-1",
                session_name,
                "nonce-1",
                command,
                &format!("send-obfuscated-{idx}"),
            );
            let result = node.process(ctx).await;
            assert!(
                result.is_ok(),
                "documented gap: {command:?} was expected to pass the floor, got {result:?}"
            );
        }
    }
}
