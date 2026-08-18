//! `TerminalSessionNode` — ensure a tmux session exists, acquire its lease,
//! optionally launch it in a working directory (`EN.9.D` task 2). READ-ONLY:
//! no sends, no waits beyond the session-creation call itself.
//!
//! ORDERING IS THE POINT. `crate::workflow::node_context` (`workflow.rs:587`)
//! snapshots `pre_call_ctx` before calling `Node::process` and restores it
//! wholesale on `Err` (`workflow.rs:599`) — everything this node writes to
//! `ctx` is discarded the instant it returns an error. A tmux option lives
//! outside `ctx` entirely and survives that restore, which is the only
//! reason a half-created session stays discoverable after a mid-create
//! failure. So `@engine_run_id`/`@engine_created_at` are written via
//! `set_option` FIRST — before `list_sessions`, before `new_session`,
//! before the lease acquire — deliberately ahead of every other fallible
//! call this node makes. Do not reorder these for readability.
//!
//! Session discovery is namespaced per session name, mirroring
//! `term_core::lease`'s `option_name` (`lease.rs:252-259`): `set-option -g`
//! is process-wide, not session-scoped, so the session name is folded into
//! the option name itself (`@engine_run_id@<session_name>`) rather than
//! relying on the option's *value* alone to disambiguate concurrent
//! sessions sharing one tmux server.
//!
//! Back-edge re-entry (the same node identity re-executing within the same
//! run) must reuse the existing session and its existing lease rather than
//! create a second. `session_name_for` already makes the session name a
//! pure function of `run_id` + node identity, so re-entry resolves to the
//! same name; the lease's nonce is made equally deterministic here (the
//! session name itself) so a re-entrant `SessionLease::acquire` hits
//! `lease.rs`'s same-nonce "re-acquiring our own still-live lease is a
//! no-op success" path instead of colliding as a foreign lease.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use engine_contract::TaskContext;
use term_core::driver::TerminalDriver;
use term_core::lease::{AcquireRequest, SessionLease};
use term_core::model::parse_session_line;

use crate::node::{Node, NodeError};
use crate::workflow::read_run_id;
use crate::workflows::put_result;

use super::identity::session_name_for;

/// The `Node::name()` identity `TerminalSessionNode` runs under, and the
/// `ctx.nodes` key its output is stamped onto. Read by `TerminalObserveNode`
/// (task 4) as its session-identity read-preference fallback.
pub const NODE_NAME: &str = "TerminalSessionNode";

/// Default lease TTL a `TerminalSessionNode` acquires for — long enough to
/// cover one node's own work, renewed by later Phase-3 nodes (`EN.9.E`) as
/// they act on the same session.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(300);

/// The tmux user-option base name this node stamps the owning run id under.
/// Namespaced per session via [`namespaced_option`], following
/// `term_core::lease`'s `option_name` convention.
pub const ENGINE_RUN_ID_OPTION: &str = "@engine_run_id";

/// The tmux user-option base name this node stamps the session's creation
/// timestamp under (RFC 3339). Namespaced per session via
/// [`namespaced_option`].
pub const ENGINE_CREATED_AT_OPTION: &str = "@engine_created_at";

/// Namespace a base tmux user-option name by session, so two sessions on
/// one tmux server never collide on what is otherwise a process-wide
/// `set-option -g`. Mirrors `term_core::lease::option_name` exactly.
fn namespaced_option(base: &str, session_name: &str) -> String {
    format!("{base}@{session_name}")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Ensure the session exists (create one, acquire the lease, optionally
/// launch in a working directory) — no sends, no waits beyond the
/// session-creation call itself.
pub struct TerminalSessionNode {
    driver: Arc<dyn TerminalDriver>,
    /// Working directory the session is created in when it does not yet
    /// exist (`tmux new-session -c <dir>`) — the "optionally launch" half
    /// of this node's job. `None` creates the session in tmux's default
    /// directory.
    dir: Option<String>,
    /// How long an acquired lease is valid for before it is considered
    /// expired.
    lease_ttl: Duration,
    /// How long past expiry a FOREIGN lease must have aged before this
    /// node may steal it. `None` is fail-closed (`term_core::lease`'s
    /// default): an expired-but-present foreign lease is never acquired.
    steal_after: Option<Duration>,
}

impl TerminalSessionNode {
    /// Construct with the given driver, no launch directory, the default
    /// lease TTL, and fail-closed stealing (no `steal_after`).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            dir: None,
            lease_ttl: DEFAULT_LEASE_TTL,
            steal_after: None,
        }
    }

    /// Set the working directory a newly created session launches in.
    #[must_use]
    pub fn with_dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Override the default lease TTL.
    #[must_use]
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    /// Allow stealing a foreign, expired lease once it has aged past
    /// `steal_after` beyond its `expires_at`. Unset (the default) is
    /// fail-closed: an expired foreign lease is never acquired.
    #[must_use]
    pub fn with_steal_after(mut self, steal_after: Duration) -> Self {
        self.steal_after = Some(steal_after);
        self
    }

    /// Whether `session_name` already appears in `list_sessions`' raw
    /// output. Malformed lines are skipped rather than erroring the whole
    /// listing — a listing this node cannot fully parse should not block
    /// it from seeing the one session it actually cares about.
    fn session_present(list_output: &str, session_name: &str) -> bool {
        list_output
            .lines()
            .filter_map(|line| parse_session_line(line).ok())
            .any(|session| session.name == session_name)
    }
}

#[async_trait::async_trait]
impl Node for TerminalSessionNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let run_id = read_run_id(&ctx.metadata).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no run_id stamped on ctx.metadata — cannot derive a session name"
            ))
        })?;
        let session_name = session_name_for(&run_id, NODE_NAME);

        // ORDERING IS THE POINT (see module doc): these two `set_option`
        // calls are the FIRST fallible work this node does — ahead of
        // `list_sessions`, `new_session`, and the lease acquire — so a
        // failure anywhere after them still leaves the session
        // discoverable by `@engine_run_id` once `pre_call_ctx` discards
        // everything else this call would have written.
        self.driver
            .set_option(
                &namespaced_option(ENGINE_RUN_ID_OPTION, &session_name),
                &run_id,
            )
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to stamp {ENGINE_RUN_ID_OPTION}: {err}"
                ))
            })?;
        let created_at = Utc::now().to_rfc3339();
        self.driver
            .set_option(
                &namespaced_option(ENGINE_CREATED_AT_OPTION, &session_name),
                &created_at,
            )
            .await
            .map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to stamp {ENGINE_CREATED_AT_OPTION}: {err}"
                ))
            })?;

        let listed =
            self.driver.list_sessions().await.map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: list_sessions failed: {err}"))
            })?;
        let already_present = Self::session_present(&listed, &session_name);

        if !already_present {
            self.driver
                .new_session(&session_name, self.dir.as_deref())
                .await
                .map_err(|err| NodeError::new(format!("{NODE_NAME}: new_session failed: {err}")))?;
        }

        // Deterministic nonce (the session name itself, which is already a
        // pure function of run_id + node identity) so a back-edge
        // re-entry's acquire hits `lease.rs`'s same-nonce no-op-success
        // path rather than colliding as a foreign lease with a fresh
        // random nonce every call.
        let nonce = session_name.clone();
        let now = now_ms();
        let lease = SessionLease::new(self.driver.as_ref());
        let acquired = lease
            .acquire(AcquireRequest {
                session_name: &session_name,
                run_id: &run_id,
                nonce: &nonce,
                identity: NODE_NAME,
                expires_at_ms: now + self.lease_ttl.as_millis() as u64,
                now_ms: now,
                steal_after: self.steal_after,
            })
            .await
            .map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: lease acquisition failed: {err}"))
            })?;

        put_result(
            &mut ctx,
            self.name(),
            serde_json::json!({
                "session_name": session_name,
                "lease_nonce": acquired.nonce,
                "created": !already_present,
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
    use term_core::driver::{StubOutcome, StubTerminalDriver};

    fn ctx_with_run_id(run_id: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.metadata["run_id"] = serde_json::json!(run_id);
        ctx
    }

    fn list_sessions_line(session_name: &str) -> String {
        format!("{session_name}\t0\t1\t0\t\t/tmp")
    }

    /// `StubTerminalDriver::show_option` returns one configured value per
    /// name regardless of what `set_option` just wrote (it does not model
    /// tmux's actual storage — see `lease.rs`'s
    /// `two_concurrent_acquirers_resolve_to_exactly_one_holder...` test).
    /// Since this node's lease nonce is deterministic (the session name
    /// itself), pointing the lease option at a value carrying that same
    /// nonce makes every `acquire` call in a test hit the "re-acquiring
    /// our own still-live lease" no-op-success path, whether it is
    /// logically the first acquire or a back-edge re-entry.
    fn seed_own_lease(driver: &StubTerminalDriver, run_id: &str, session_name: &str) {
        let far_future = now_ms() + Duration::from_secs(3600).as_millis() as u64;
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(format!("{run_id}:{session_name}:{NODE_NAME}:{far_future}")),
        );
    }

    #[tokio::test]
    async fn creates_a_session_when_absent() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = session_name_for("run-1", NODE_NAME);
        seed_own_lease(&driver, "run-1", &session_name);
        let node = TerminalSessionNode::new(driver.clone());

        let ctx = node.process(ctx_with_run_id("run-1")).await.unwrap();

        let calls = driver.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.get(1).map(String::as_str) == Some("new-session")),
            "expected a new-session call, got {calls:?}"
        );
        let stored = ctx.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["created"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn back_edge_reentry_issues_no_second_new_session_call() {
        let driver = Arc::new(StubTerminalDriver::new());
        let session_name = session_name_for("run-1", NODE_NAME);
        seed_own_lease(&driver, "run-1", &session_name);
        let node = TerminalSessionNode::new(driver.clone());

        // First entry: session absent, creates it.
        let ctx = node.process(ctx_with_run_id("run-1")).await.unwrap();
        assert_eq!(
            ctx.nodes[NODE_NAME]["session_name"],
            serde_json::json!(session_name)
        );

        // Second (back-edge) entry: driver now reports the session present.
        driver.set_list_sessions_result(StubOutcome::Ok(list_sessions_line(&session_name)));
        let before = driver.calls().len();
        let ctx2 = node.process(ctx_with_run_id("run-1")).await.unwrap();
        let after_calls = driver.calls();
        let new_session_calls_since = after_calls[before..]
            .iter()
            .filter(|c| c.get(1).map(String::as_str) == Some("new-session"))
            .count();
        assert_eq!(
            new_session_calls_since, 0,
            "back-edge re-entry issued a new_session call: {after_calls:?}"
        );
        assert_eq!(ctx2.nodes[NODE_NAME]["created"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn failed_creation_still_leaves_engine_run_id_stamped_first() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_new_session_result(StubOutcome::ExitError {
            code: 1,
            stderr: "boom".to_string(),
        });
        let node = TerminalSessionNode::new(driver.clone());

        let result = node.process(ctx_with_run_id("run-1")).await;
        assert!(result.is_err(), "expected new_session failure to surface");

        let calls = driver.calls();
        let set_option_idx = calls
            .iter()
            .position(|c| c.get(1).map(String::as_str) == Some("set-option"))
            .expect("expected a set-option call recording @engine_run_id");
        let new_session_idx = calls
            .iter()
            .position(|c| c.get(1).map(String::as_str) == Some("new-session"))
            .expect("expected a new-session call");
        assert!(
            set_option_idx < new_session_idx,
            "set_option ({set_option_idx}) must precede the failing new_session call \
             ({new_session_idx}): {calls:?}"
        );
        // The stamped option name carries the run id in its value and is
        // namespaced by session name in its key.
        let stamp_argv = &calls[set_option_idx];
        assert!(
            stamp_argv
                .iter()
                .any(|a| a.starts_with(ENGINE_RUN_ID_OPTION)),
            "expected the first set-option call to stamp {ENGINE_RUN_ID_OPTION}: {stamp_argv:?}"
        );
    }

    #[tokio::test]
    async fn foreign_live_lease_surfaces_as_node_error_not_silent_skip() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = TerminalSessionNode::new(driver.clone());

        // Session already present (skip creation) but the lease option
        // shows a foreign, unexpired lease under a different nonce.
        let session_name = session_name_for("run-1", NODE_NAME);
        driver.set_list_sessions_result(StubOutcome::Ok(list_sessions_line(&session_name)));
        let far_future = now_ms() + Duration::from_secs(3600).as_millis() as u64;
        let foreign_lease = format!("other-run:other-nonce:OtherNode:{far_future}");
        driver.set_show_option_result_for(
            format!("@engine_lease@{session_name}"),
            StubOutcome::Ok(foreign_lease),
        );

        let result = node.process(ctx_with_run_id("run-1")).await;
        assert!(
            result.is_err(),
            "expected a foreign live lease to surface as an error, got {result:?}"
        );
    }
}
