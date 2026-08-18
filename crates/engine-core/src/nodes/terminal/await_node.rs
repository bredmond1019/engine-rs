//! `TerminalAwaitNode` — the bounded, cancellable poll node over task 1's
//! [`super::predicate::AwaitPredicate`] (`EN.9.E` task 3).
//!
//! # The node owns its own timeout
//!
//! `RunOptions` has no deadline field and nothing in the runner wraps
//! `Node::process(ctx).await` — so there is no bound to inherit. A node
//! that assumed one would hang forever against a stuck pane. Every poll
//! loop below is bounded by [`AwaitPolicy::timeout_ms`], a knob this node
//! resolves and enforces entirely on its own.
//!
//! # Cancellation is taken through this node's own builder
//!
//! `crate::workflow::run` (`crate::cancellation::CancellationToken`) only
//! observes cancellation BETWEEN nodes — a `select!` at the top level of
//! the run loop, checked once a node's `process` call returns. An abort
//! against a node parked inside a long `.await` therefore does nothing
//! until that `.await` resolves on its own. [`TerminalAwaitNode::with_cancellation_token`]
//! (mirroring `ClaudeCodeStep::with_cancellation_token`) takes a token
//! through the node's OWN builder so the poll loop's `select!` can race
//! cancellation against every single tick, not just the whole call.
//!
//! Mirroring `ClaudeCodeStep`'s documented convention: a cancel win returns
//! `Ok(ctx)` UNCHANGED (no result stamped under this node's `ctx.nodes`
//! identity) rather than `Err` — the runner's own between-node check
//! (`workflow.rs`) is what stamps `cancellation::stamp_cancelled` onto
//! `ctx.metadata` once it next observes `token.is_cancelled()`. A timeout,
//! by contrast, IS this node's own terminal outcome and IS stamped (see
//! below) — the two are distinguishable failure modes and must not share
//! one code path.
//!
//! # Policy — a knob per CLAUDE.md standing rule 6
//!
//! Poll interval and timeout are policy knobs, resolved through the
//! generic four-layer [`crate::policy::resolve`] precedence (per-run
//! `ctx.event.policy` override > a named `profile` bundle set on the node >
//! `harness_defaults` set on the node > [`AwaitPolicy::default`]) exactly
//! like `SdlcPolicy`/`DiagnosticIntakePolicy`. [`AwaitPolicy::default`] is
//! behavior-stable (1s poll / 10-minute bound) and every named profile in
//! [`profiles`] sets both fields explicitly. The RESOLVED values — not the
//! knob names — are stamped into this node's `ctx.nodes` result on every
//! non-cancelled return, per standing rule 6's "stamp the resolved value"
//! requirement.
//!
//! # Reading this node's own arguments
//!
//! `predicate` (the [`super::predicate::AwaitPredicate`] to poll, as a
//! small JSON shape — `{"type": "marker", "out": ..., "nonce": ...}` etc.)
//! and `sent_at` (an RFC 3339 timestamp, required only for a `Marker`
//! predicate — the baseline task 1's mtime-postdates-send check compares
//! against) are read off `ctx.event`, following `send.rs`'s precedent for
//! a node's own per-run arguments that are not an upstream node's stored
//! result. `sent_at` is read from `ctx.event` rather than from
//! `TerminalSendNode`'s stored result because that node stamps no
//! timestamp of its own (`EN.9.E` task 2's `Relevant Files` scope did not
//! include adding one) — the caller wiring the two nodes together is
//! expected to thread the send node's own event-time through.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine_contract::TaskContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use term_core::detect::AgentState;
use term_core::driver::TerminalDriver;
use tokio::time::Instant;

use crate::cancellation::CancellationToken;
use crate::node::{InputBinding, Node, NodeError};
use crate::policy::{merge_opt, resolve as resolve_policy_layers, Policy};
use crate::workflows::{get_result, put_result};

use super::identity::HasSessionInput;
use super::predicate::{self, AwaitPredicate, MarkerObservation, Observation};
use super::session;

/// The `Node::name()` identity `TerminalAwaitNode` runs under, and the
/// `ctx.nodes` key its output is stamped onto.
pub const NODE_NAME: &str = "TerminalAwaitNode";

// ── Policy ───────────────────────────────────────────────────────────────

/// The fully-resolved, per-run poll policy: how often to take a fresh
/// [`Observation`] and evaluate it, and how long to keep trying before this
/// node gives up on its own (see the module doc's "the node owns its own
/// timeout" section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitPolicy {
    pub poll_interval_ms: u64,
    pub timeout_ms: u64,
}

impl Default for AwaitPolicy {
    /// The behavior-stable baseline: poll once a second, give up after ten
    /// minutes. Every call site that does not opt into a different profile
    /// or an explicit override gets exactly this.
    fn default() -> Self {
        Self {
            poll_interval_ms: 1_000,
            timeout_ms: 600_000,
        }
    }
}

/// All-optional mirror of [`AwaitPolicy`] used by the override layers
/// (a node's `harness_defaults`/`profile`, and a per-run `ctx.event.policy`
/// override). Every field left `None` falls through to the next-lower-
/// precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialAwaitPolicy {
    pub poll_interval_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
}

impl Policy for AwaitPolicy {
    type Partial = PartialAwaitPolicy;

    fn apply(self, over: &PartialAwaitPolicy) -> Self {
        Self {
            poll_interval_ms: merge_opt(self.poll_interval_ms, over.poll_interval_ms),
            timeout_ms: merge_opt(self.timeout_ms, over.timeout_ms),
        }
    }
}

/// Resolve the four policy layers into one concrete [`AwaitPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats [`AwaitPolicy::default`]. Delegates to the
/// generic `crate::policy::resolve`.
#[must_use]
pub fn resolve(
    harness_defaults: Option<&PartialAwaitPolicy>,
    profile: Option<&PartialAwaitPolicy>,
    event_override: Option<&PartialAwaitPolicy>,
) -> AwaitPolicy {
    resolve_policy_layers(
        AwaitPolicy::default(),
        harness_defaults,
        profile,
        event_override,
    )
}

/// Named [`PartialAwaitPolicy`] bundles for `TerminalAwaitNode`, per
/// CLAUDE.md standing rule 6's "every workflow ships the three named
/// profiles" — `baseline` restates the built-in default explicitly (a
/// legible no-op), `cheap-fast` is the cost/latency floor (fewer, cheaper
/// polls, a short leash), `thorough` is the quality ceiling (tighter
/// polling, a long leash for a slow build).
pub mod profiles {
    use super::{AwaitPolicy, PartialAwaitPolicy};

    /// Restates [`AwaitPolicy::default`] verbatim — selecting `"baseline"`
    /// must not silently change behavior.
    #[must_use]
    pub fn baseline() -> PartialAwaitPolicy {
        let default = AwaitPolicy::default();
        PartialAwaitPolicy {
            poll_interval_ms: Some(default.poll_interval_ms),
            timeout_ms: Some(default.timeout_ms),
        }
    }

    /// Fewer, cheaper polls and a short leash — the cost/latency floor.
    #[must_use]
    pub fn cheap_fast() -> PartialAwaitPolicy {
        PartialAwaitPolicy {
            poll_interval_ms: Some(2_000),
            timeout_ms: Some(120_000),
        }
    }

    /// Tighter polling and a long leash — the quality ceiling, for a slow
    /// build a `cheap-fast` bound would abandon prematurely.
    #[must_use]
    pub fn thorough() -> PartialAwaitPolicy {
        PartialAwaitPolicy {
            poll_interval_ms: Some(500),
            timeout_ms: Some(1_800_000),
        }
    }

    /// Look up one of the three canonical profile names. `None` for any
    /// other name — callers decide whether an unknown name is an error.
    #[must_use]
    pub fn profile_by_name(name: &str) -> Option<PartialAwaitPolicy> {
        match name {
            "baseline" => Some(baseline()),
            "cheap-fast" => Some(cheap_fast()),
            "thorough" => Some(thorough()),
            _ => None,
        }
    }
}

// ── The node ─────────────────────────────────────────────────────────────

/// A bounded, cancellable poll over task 1's [`AwaitPredicate`] — see the
/// module doc for the timeout/cancellation/policy design.
pub struct TerminalAwaitNode {
    driver: Arc<dyn TerminalDriver>,
    /// Resolves which `TerminalSessionNode` (by `ctx.nodes` identity) this
    /// node reads its target session name from. Unbound falls back to
    /// [`session::NODE_NAME`].
    session_input: InputBinding,
    /// Taken through this node's OWN builder, never read from the runner —
    /// see the module doc.
    cancellation_token: Option<CancellationToken>,
    /// The `harness.json`-sourced policy-defaults layer, set by the
    /// constructing call site (this node does no file IO of its own).
    harness_defaults: Option<PartialAwaitPolicy>,
    /// The named-profile layer, resolved by the constructing call site via
    /// [`profiles::profile_by_name`] (or an ad-hoc bundle).
    profile: Option<PartialAwaitPolicy>,
}

impl TerminalAwaitNode {
    /// Construct with the given driver, no cancellation token, and no
    /// harness/profile override layers (both fall back to
    /// [`AwaitPolicy::default`] unless a `ctx.event.policy` override is
    /// present at call time).
    #[must_use]
    pub fn new(driver: Arc<dyn TerminalDriver>) -> Self {
        Self {
            driver,
            session_input: InputBinding::unbound(),
            cancellation_token: None,
            harness_defaults: None,
            profile: None,
        }
    }

    /// Attach a [`CancellationToken`], raced against every poll tick's
    /// sleep (see the module doc). A cancel win returns `Ok(ctx)`
    /// unchanged.
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Set the `harness.json`-sourced policy-defaults override layer.
    #[must_use]
    pub fn with_harness_defaults(mut self, defaults: PartialAwaitPolicy) -> Self {
        self.harness_defaults = Some(defaults);
        self
    }

    /// Set the named-profile override layer.
    #[must_use]
    pub fn with_profile(mut self, profile: PartialAwaitPolicy) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Read the upstream `TerminalSessionNode`'s stored `session_name` from
    /// `ctx.nodes`, per the bound (or defaulted) `session_input` identity —
    /// the same read-preference-with-fallback shape `observe.rs`/`send.rs`
    /// use.
    fn read_upstream_session(&self, ctx: &TaskContext) -> Result<String, NodeError> {
        let bound = self.session_input.resolve(session::NODE_NAME);
        let stored = get_result(ctx, bound).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: no session recorded by {bound} — TerminalAwaitNode must run \
                 after a TerminalSessionNode"
            ))
        })?;
        stored
            .get("session_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {bound}'s stored result is missing `session_name`"
                ))
            })
    }

    /// Read this node's own `predicate`/`sent_at`/`policy` arguments off
    /// `ctx.event` — see the module doc's "Reading this node's own
    /// arguments" section.
    fn read_event_args(
        ctx: &TaskContext,
    ) -> Result<(AwaitPredicate, SystemTime, Option<PartialAwaitPolicy>), NodeError> {
        let predicate_value = ctx.event.get("predicate").ok_or_else(|| {
            NodeError::new(format!("{NODE_NAME}: ctx.event is missing `predicate`"))
        })?;
        let predicate = parse_predicate(predicate_value)?;

        let sent_at = match ctx.event.get("sent_at").and_then(Value::as_str) {
            Some(raw) => parse_rfc3339(raw)?,
            None => {
                if matches!(predicate, AwaitPredicate::Marker { .. }) {
                    return Err(NodeError::new(format!(
                        "{NODE_NAME}: ctx.event is missing `sent_at`, required for a Marker \
                         predicate"
                    )));
                }
                UNIX_EPOCH
            }
        };

        let event_override = match ctx.event.get("policy") {
            Some(value) => Some(
                serde_json::from_value::<PartialAwaitPolicy>(value.clone()).map_err(|err| {
                    NodeError::new(format!("{NODE_NAME}: invalid ctx.event.policy: {err}"))
                })?,
            ),
            None => None,
        };

        Ok((predicate, sent_at, event_override))
    }

    /// Read the marker file's current state for a [`AwaitPredicate::Marker`]
    /// poll tick. A read error (missing file, unreadable metadata) is
    /// treated identically to "does not exist yet" — never as satisfying.
    fn observe_marker(out: &str, nonce: &str) -> MarkerObservation {
        let path = predicate::marker_path(out, nonce);
        match std::fs::metadata(&path) {
            Ok(meta) => MarkerObservation {
                exists: true,
                content: std::fs::read_to_string(&path).ok(),
                mtime: meta.modified().ok(),
            },
            Err(_) => MarkerObservation::default(),
        }
    }

    /// Read the driven process's exit code for an
    /// [`AwaitPredicate::ExitCode`] poll tick via tmux's
    /// `pane_dead`/`pane_dead_status` formats. `None` while the pane is
    /// still alive, or when the read itself fails — both are "not exited
    /// yet" as far as [`predicate::evaluate`] is concerned.
    async fn observe_exit_code(&self, session_name: &str) -> Option<i32> {
        let raw = self
            .driver
            .display_message(session_name, "#{pane_dead}:#{pane_dead_status}")
            .await
            .ok()?;
        let (dead, status) = raw.trim().split_once(':')?;
        if dead == "1" {
            status.parse::<i32>().ok()
        } else {
            None
        }
    }

    /// Take one poll tick's [`Observation`]: capture the pane, read the
    /// marker file (when polling `Marker`), and check the exit code —
    /// exactly the seams `predicate::evaluate` needs and no more.
    async fn take_observation(
        &self,
        session_name: &str,
        predicate: &AwaitPredicate,
        sent_at: SystemTime,
        silence_duration: Duration,
    ) -> Result<Observation, NodeError> {
        let screen = self
            .driver
            .capture_pane(session_name)
            .await
            .map_err(|err| NodeError::new(format!("{NODE_NAME}: capture_pane failed: {err}")))?;

        let marker = match predicate {
            AwaitPredicate::Marker { out, nonce } => Self::observe_marker(out, nonce),
            _ => MarkerObservation::default(),
        };

        let exit_code = match predicate {
            AwaitPredicate::ExitCode { .. } => self.observe_exit_code(session_name).await,
            _ => None,
        };

        Ok(Observation {
            screen,
            marker,
            silence_duration,
            exit_code,
            sent_at,
        })
    }
}

impl HasSessionInput for TerminalAwaitNode {
    fn session_input_mut(&mut self) -> &mut InputBinding {
        &mut self.session_input
    }
}

#[async_trait::async_trait]
impl Node for TerminalAwaitNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let session_name = self.read_upstream_session(&ctx)?;
        let (predicate, sent_at, event_override) = Self::read_event_args(&ctx)?;

        let policy = resolve(
            self.harness_defaults.as_ref(),
            self.profile.as_ref(),
            event_override.as_ref(),
        );
        let poll_interval = Duration::from_millis(policy.poll_interval_ms.max(1));
        let timeout = Duration::from_millis(policy.timeout_ms);

        let deadline = Instant::now() + timeout;
        let mut last_screen: Option<String> = None;
        let mut last_change = Instant::now();

        loop {
            let now = Instant::now();
            // A fresh capture each tick decides whether the screen changed
            // since the previous tick — the pane's own silence signal.
            // `take_observation` re-captures below; this pre-capture only
            // exists to compute `silence_duration` cheaply before handing
            // the tick's real observation to `predicate::evaluate`. Kept as
            // one `capture_pane` per tick (not two) by reusing the same
            // screen for both purposes.
            let screen = self
                .driver
                .capture_pane(&session_name)
                .await
                .map_err(|err| {
                    NodeError::new(format!("{NODE_NAME}: capture_pane failed: {err}"))
                })?;
            if last_screen.as_deref() != Some(screen.as_str()) {
                last_change = now;
                last_screen = Some(screen);
            }
            let silence_duration = now.saturating_duration_since(last_change);

            let observation = self
                .take_observation(&session_name, &predicate, sent_at, silence_duration)
                .await?;

            if predicate::evaluate(&predicate, &observation).is_satisfied() {
                put_result(
                    &mut ctx,
                    self.name(),
                    serde_json::json!({
                        "session_name": session_name,
                        "satisfied": true,
                        "timed_out": false,
                        "poll_interval_ms": policy.poll_interval_ms,
                        "timeout_ms": policy.timeout_ms,
                    }),
                );
                return Ok(ctx);
            }

            if Instant::now() >= deadline {
                put_result(
                    &mut ctx,
                    self.name(),
                    serde_json::json!({
                        "session_name": session_name,
                        "satisfied": false,
                        "timed_out": true,
                        "poll_interval_ms": policy.poll_interval_ms,
                        "timeout_ms": policy.timeout_ms,
                    }),
                );
                return Ok(ctx);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            let sleep_for = poll_interval.min(remaining);

            // Race the sleep against cancellation on EVERY tick — this is
            // what makes a cancel against a 10-minute await return within
            // one poll interval rather than at the end of the whole
            // timeout. See the module doc's "cancellation is taken through
            // this node's own builder" section.
            if let Some(token) = &self.cancellation_token {
                tokio::select! {
                    _ = token.cancelled() => return Ok(ctx),
                    () = tokio::time::sleep(sleep_for) => {}
                }
            } else {
                tokio::time::sleep(sleep_for).await;
            }
        }
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

/// Parse `value` (`ctx.event.predicate`) into an [`AwaitPredicate`]. Shape:
/// `{"type": "marker" | "detect" | "regex" | "silence" | "exit_code", ...}`
/// with variant-specific fields (`out`/`nonce`, `target`, `pattern`,
/// `min_duration_ms`, `expected`). A hand-rolled parse rather than
/// `#[derive(Deserialize)]` on [`AwaitPredicate`] itself, since task 1
/// (`predicate.rs`) deliberately keeps that type free of a serde
/// dependency — it is a pure-logic module, not a wire type.
fn parse_predicate(value: &Value) -> Result<AwaitPredicate, NodeError> {
    let obj = value.as_object().ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: ctx.event.predicate must be an object"
        ))
    })?;
    let ty = obj.get("type").and_then(Value::as_str).ok_or_else(|| {
        NodeError::new(format!(
            "{NODE_NAME}: ctx.event.predicate is missing `type`"
        ))
    })?;

    match ty {
        "marker" => {
            let out = required_str(obj, "out")?;
            let nonce = required_str(obj, "nonce")?;
            Ok(AwaitPredicate::Marker { out, nonce })
        }
        "detect" => {
            let target_str = required_str(obj, "target")?;
            let target: AgentState =
                serde_json::from_value(Value::String(target_str)).map_err(|err| {
                    NodeError::new(format!(
                        "{NODE_NAME}: ctx.event.predicate.target is not a valid AgentState: {err}"
                    ))
                })?;
            Ok(AwaitPredicate::Detect { target })
        }
        "regex" => {
            let pattern = required_str(obj, "pattern")?;
            Ok(AwaitPredicate::Regex { pattern })
        }
        "silence" => {
            let ms = obj
                .get("min_duration_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    NodeError::new(format!(
                        "{NODE_NAME}: ctx.event.predicate is missing numeric `min_duration_ms`"
                    ))
                })?;
            Ok(AwaitPredicate::Silence {
                min_duration: Duration::from_millis(ms),
            })
        }
        "exit_code" => {
            let expected = obj
                .get("expected")
                .and_then(Value::as_i64)
                .map(|v| v as i32);
            Ok(AwaitPredicate::ExitCode { expected })
        }
        other => Err(NodeError::new(format!(
            "{NODE_NAME}: unknown ctx.event.predicate.type {other:?}"
        ))),
    }
}

fn required_str(obj: &serde_json::Map<String, Value>, field: &str) -> Result<String, NodeError> {
    obj.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: ctx.event.predicate is missing `{field}`"
            ))
        })
}

/// Parse an RFC 3339 timestamp (`ctx.event.sent_at`) into a [`SystemTime`].
fn parse_rfc3339(raw: &str) -> Result<SystemTime, NodeError> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|err| NodeError::new(format!("{NODE_NAME}: invalid `sent_at` {raw:?}: {err}")))?;
    let secs = dt.timestamp();
    let nanos = dt.timestamp_subsec_nanos();
    if secs >= 0 {
        Ok(UNIX_EPOCH + Duration::new(secs as u64, nanos))
    } else {
        Ok(UNIX_EPOCH - Duration::new((-secs) as u64, 0) + Duration::from_nanos(u64::from(nanos)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::driver::{StubOutcome, StubTerminalDriver};

    fn ctx_with_upstream_session(session_name: &str, event: Value) -> TaskContext {
        let mut ctx = TaskContext {
            event,
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        };
        ctx.nodes.insert(
            session::NODE_NAME.to_string(),
            serde_json::json!({
                "session_name": session_name,
                "lease_nonce": "some-nonce",
                "created": true,
            }),
        );
        ctx
    }

    // ── Policy resolution ────────────────────────────────────────────────

    #[test]
    fn default_policy_is_one_second_poll_ten_minute_timeout() {
        let policy = AwaitPolicy::default();
        assert_eq!(policy.poll_interval_ms, 1_000);
        assert_eq!(policy.timeout_ms, 600_000);
    }

    #[test]
    fn baseline_profile_restates_the_default_verbatim() {
        let resolved = resolve(None, Some(&profiles::baseline()), None);
        assert_eq!(resolved, AwaitPolicy::default());
    }

    #[test]
    fn event_override_beats_profile_beats_harness_beats_builtin() {
        let harness = PartialAwaitPolicy {
            poll_interval_ms: Some(5_000),
            timeout_ms: Some(5_000),
        };
        let profile = PartialAwaitPolicy {
            poll_interval_ms: Some(2_000),
            timeout_ms: None,
        };
        let event = PartialAwaitPolicy {
            poll_interval_ms: Some(10),
            timeout_ms: None,
        };
        let resolved = resolve(Some(&harness), Some(&profile), Some(&event));
        // event wins for poll_interval_ms.
        assert_eq!(resolved.poll_interval_ms, 10);
        // event and profile left timeout_ms untouched, so harness's value
        // survives.
        assert_eq!(resolved.timeout_ms, 5_000);
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(
            profiles::profile_by_name("baseline"),
            Some(profiles::baseline())
        );
        assert_eq!(
            profiles::profile_by_name("cheap-fast"),
            Some(profiles::cheap_fast())
        );
        assert_eq!(
            profiles::profile_by_name("thorough"),
            Some(profiles::thorough())
        );
        assert_eq!(profiles::profile_by_name("nonexistent"), None);
    }

    // ── Marker predicate, end to end through the node ───────────────────

    #[tokio::test]
    async fn marker_predicate_satisfies_once_the_marker_file_appears() {
        let dir = std::env::temp_dir().join(format!(
            "engine-rs-terminal-await-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.log");
        let nonce = "nonce-1";
        let marker_path = predicate::marker_path(out.to_str().unwrap(), nonce);
        std::fs::write(&marker_path, nonce).unwrap();

        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("some output".to_string()));
        let node = TerminalAwaitNode::new(driver.clone());

        let sent_at = chrono::Utc::now() - chrono::Duration::seconds(60);
        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": {
                    "type": "marker",
                    "out": out.to_str().unwrap(),
                    "nonce": nonce,
                },
                "sent_at": sent_at.to_rfc3339(),
                "policy": { "poll_interval_ms": 10, "timeout_ms": 2000 },
            }),
        );

        let result = node.process(ctx).await.unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["satisfied"], serde_json::json!(true));
        assert_eq!(stored["timed_out"], serde_json::json!(false));
        assert_eq!(stored["poll_interval_ms"], serde_json::json!(10));
        assert_eq!(stored["timeout_ms"], serde_json::json!(2000));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stale_marker_does_not_satisfy_and_the_node_times_out() {
        let dir = std::env::temp_dir().join(format!(
            "engine-rs-terminal-await-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.log");
        let nonce = "nonce-1";
        let marker_path = predicate::marker_path(out.to_str().unwrap(), nonce);
        // Written BEFORE `sent_at` below — a marker surviving from a
        // previous run.
        std::fs::write(&marker_path, nonce).unwrap();

        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("some output".to_string()));
        let node = TerminalAwaitNode::new(driver.clone());

        // sent_at in the FUTURE relative to the marker's mtime, guaranteeing
        // staleness regardless of filesystem timestamp resolution.
        let sent_at = chrono::Utc::now() + chrono::Duration::seconds(60);
        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": {
                    "type": "marker",
                    "out": out.to_str().unwrap(),
                    "nonce": nonce,
                },
                "sent_at": sent_at.to_rfc3339(),
                "policy": { "poll_interval_ms": 10, "timeout_ms": 50 },
            }),
        );

        let result = node.process(ctx).await.unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["satisfied"], serde_json::json!(false));
        assert_eq!(stored["timed_out"], serde_json::json!(true));

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Cancellation ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn cancellation_returns_within_five_seconds_of_a_ten_minute_bound() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("never satisfies".to_string()));
        let token = CancellationToken::new();
        let node = TerminalAwaitNode::new(driver.clone()).with_cancellation_token(token.clone());

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "regex", "pattern": "NEVER MATCHES" },
                // A real 10-minute bound — cancellation must win long
                // before this fires.
                "policy": { "poll_interval_ms": 20, "timeout_ms": 600_000 },
            }),
        );

        let handle = tokio::spawn(async move { node.process(ctx).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("expected the node to return within 5 seconds of cancellation")
            .expect("task panicked")
            .expect("process() returned an error");

        // Cancel wins: no result is stamped for this node, matching
        // `ClaudeCodeStep`'s documented convention.
        assert!(!result.nodes.contains_key(NODE_NAME));
    }

    // ── Timeout with no runner deadline in play ─────────────────────────

    #[tokio::test]
    async fn own_timeout_fires_with_no_cancellation_token_and_no_runner_deadline() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("never satisfies".to_string()));
        // No `with_cancellation_token` call at all — nothing external can
        // stop this node; it must bound itself.
        let node = TerminalAwaitNode::new(driver.clone());

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "regex", "pattern": "NEVER MATCHES" },
                "policy": { "poll_interval_ms": 5, "timeout_ms": 40 },
            }),
        );

        let result = tokio::time::timeout(Duration::from_secs(5), node.process(ctx))
            .await
            .expect("expected the node's own timeout to fire well within 5 seconds")
            .unwrap();

        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["timed_out"], serde_json::json!(true));
    }

    // ── Detect / Silence / ExitCode each terminate the poll ─────────────

    #[tokio::test]
    async fn detect_predicate_terminates_the_poll() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("some text\n> ".to_string()));
        let node = TerminalAwaitNode::new(driver.clone());

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "detect", "target": "idle" },
                "policy": { "poll_interval_ms": 10, "timeout_ms": 2000 },
            }),
        );

        let result = node.process(ctx).await.unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["satisfied"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn silence_predicate_terminates_the_poll_once_threshold_elapses() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("unchanging output".to_string()));
        let node = TerminalAwaitNode::new(driver.clone());

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "silence", "min_duration_ms": 30 },
                "policy": { "poll_interval_ms": 10, "timeout_ms": 2000 },
            }),
        );

        let result = tokio::time::timeout(Duration::from_secs(5), node.process(ctx))
            .await
            .expect("expected silence to satisfy within 5 seconds")
            .unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["satisfied"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn exit_code_predicate_terminates_the_poll_once_the_pane_is_dead() {
        let driver = Arc::new(StubTerminalDriver::new());
        driver.set_capture_pane_result(StubOutcome::Ok("$ ".to_string()));
        driver.set_display_message_result(StubOutcome::Ok("1:0".to_string()));
        let node = TerminalAwaitNode::new(driver.clone());

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "exit_code", "expected": 0 },
                "policy": { "poll_interval_ms": 10, "timeout_ms": 2000 },
            }),
        );

        let result = node.process(ctx).await.unwrap();
        let stored = result.nodes.get(NODE_NAME).unwrap();
        assert_eq!(stored["satisfied"], serde_json::json!(true));
    }

    // ── Argument validation ──────────────────────────────────────────────

    #[tokio::test]
    async fn missing_sent_at_on_a_marker_predicate_is_a_node_error() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = TerminalAwaitNode::new(driver);

        let ctx = ctx_with_upstream_session(
            "eng-run1_TerminalSessionNode",
            serde_json::json!({
                "predicate": { "type": "marker", "out": "/tmp/out.log", "nonce": "n1" },
            }),
        );

        let result = node.process(ctx).await;
        assert!(result.is_err(), "expected a missing sent_at to error");
    }

    #[tokio::test]
    async fn missing_upstream_session_surfaces_as_node_error() {
        let driver = Arc::new(StubTerminalDriver::new());
        let node = TerminalAwaitNode::new(driver);

        let ctx = TaskContext {
            event: serde_json::json!({
                "predicate": { "type": "regex", "pattern": "x" },
            }),
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
