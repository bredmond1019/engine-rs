//! `driver.rs` — the `TerminalDriver` trait seam (`EN.9.B` task 2).
//!
//! Follows the `HttpPost` seam shape exactly (`crates/engine-core/src/nodes/
//! http_post.rs:35,63,137`): production code reaches for a real,
//! tmux-backed implementation ([`TmuxDriver`]) while node tests inject a
//! recording stub ([`StubTerminalDriver`]) — no live `tmux` process spawned
//! in the gated `cargo nextest` suite. Both are held as `Arc<dyn
//! TerminalDriver>` so Phase-2 nodes (`EN.9.D`/`EN.9.E`) can swap either in
//! behind one field.
//!
//! `TmuxDriver` constructs no argv of its own — every operation routes
//! through the pure `*_args` builders already in [`crate::tmux`], executed
//! with [`crate::tmux::run_tmux_async`]. `StubTerminalDriver` records the
//! exact same argv (via the same builders) so a test asserting on "what
//! would have been sent to tmux" and a test asserting on "what the stub
//! recorded" are checking the identical shape.
//!
//! Behind the non-default `tokio` feature — `bastion` consumes `term-core`
//! blocking-only and must keep paying nothing for this module.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use crate::capture_cache::CaptureCache;
use crate::hold::{HoldError, OperatorHold};
use crate::lease::{LeaseError, SessionLease};
use crate::tmux::{
    self, capture_pane_args, display_message_args, kill_session_args, list_sessions_args,
    new_session_args, send_enter_args, send_keys_args, send_named_key_args, set_option_args,
    show_option_args, TmuxError,
};

/// Default per-invocation timeout for [`TmuxDriver`]'s async calls. Callers
/// that need a different bound (e.g. the lease's `steal_after` knob, or a
/// test exercising the timeout path) use [`TmuxDriver::new`].
pub const DEFAULT_TMUX_TIMEOUT: Duration = Duration::from_secs(5);

/// The tmux operations the Phase-2 terminal nodes need, behind one
/// object-safe, async trait. Mirrors `HttpPost`'s role: production code and
/// test code hold the same `Arc<dyn TerminalDriver>` and never know which
/// implementation they got.
#[async_trait]
pub trait TerminalDriver: Send + Sync {
    /// List all tmux sessions; returns the raw `list-sessions -F` output.
    async fn list_sessions(&self) -> Result<String, TmuxError>;

    /// Capture the last-pane output of `session_name`; returns raw text.
    async fn capture_pane(&self, session_name: &str) -> Result<String, TmuxError>;

    /// Create a detached tmux session, optionally starting in `dir`.
    async fn new_session(&self, session_name: &str, dir: Option<&str>) -> Result<(), TmuxError>;

    /// Remove a tmux session.
    async fn kill_session(&self, session_name: &str) -> Result<(), TmuxError>;

    /// Send `keys` literally to `session_name` — the FIRST of `send_keys`'s
    /// two invocations. Exposed separately (rather than folded into
    /// [`TerminalDriver::send_keys`]) so a caller sitting between the two
    /// invocations — the per-session guarded sender (`EN.9.B` task 6) — can
    /// observe a literal-send success followed by an Enter-send failure and
    /// react (send a `C-u` line-clear) instead of losing that distinction
    /// behind one bundled `Result`.
    async fn send_literal(&self, session_name: &str, keys: &str) -> Result<(), TmuxError>;

    /// Send the Enter keypress to `session_name` — the SECOND of
    /// `send_keys`'s two invocations. See [`TerminalDriver::send_literal`].
    async fn send_enter(&self, session_name: &str) -> Result<(), TmuxError>;

    /// Send `keys` literally to `session_name`, followed by an Enter
    /// keypress — the same two-invocation shape as `tmux::send_keys`.
    /// Default: [`TerminalDriver::send_literal`] then
    /// [`TerminalDriver::send_enter`], the Enter only attempted if the
    /// literal send succeeded (mirroring a real tmux invocation, which
    /// never reaches the second call after the first fails).
    async fn send_keys(&self, session_name: &str, keys: &str) -> Result<(), TmuxError> {
        self.send_literal(session_name, keys).await?;
        self.send_enter(session_name).await
    }

    /// Send a single named key (e.g. `Escape`, `C-c`) to `session_name`.
    async fn send_named_key(&self, session_name: &str, key: &str) -> Result<(), TmuxError>;

    /// Write a global tmux option (`set-option -g <name> <value>`) —
    /// including an `@`-prefixed user option, used by the session lease
    /// (`EN.9.B` task 3+) to stash its metadata.
    async fn set_option(&self, name: &str, value: &str) -> Result<(), TmuxError>;

    /// Read back a global tmux option previously written with
    /// [`TerminalDriver::set_option`].
    async fn show_option(&self, name: &str) -> Result<String, TmuxError>;

    /// Print an expanded tmux format string (`display-message -p`) for
    /// `session_name` — used by the operator hold (`EN.9.B` task 5) to read
    /// `#{session_attached}` as the raw-`tmux attach` fallback signal for a
    /// session no managed attach path saw.
    async fn display_message(&self, session_name: &str, format: &str) -> Result<String, TmuxError>;
}

/// The live driver: every operation delegates to an existing pure `*_args`
/// builder in [`crate::tmux`] plus [`tmux::run_tmux_async`]. It reimplements
/// no argv construction.
#[derive(Debug, Clone)]
pub struct TmuxDriver {
    timeout: Duration,
    capture_cache: CaptureCache,
}

impl Default for TmuxDriver {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TMUX_TIMEOUT,
            capture_cache: CaptureCache::new(),
        }
    }
}

impl TmuxDriver {
    /// Build a driver whose async tmux invocations are bounded by `timeout`
    /// — a wedged tmux call is cancelled rather than occupying the caller
    /// indefinitely (see `run_tmux_async`'s `kill_on_drop` behavior).
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            capture_cache: CaptureCache::new(),
        }
    }

    /// Build a driver whose `capture_pane` short-TTL cache uses `ttl`
    /// instead of [`crate::capture_cache::DEFAULT_CAPTURE_TTL`] — the
    /// override a test (or a future policy knob) reaches for.
    #[must_use]
    pub fn with_capture_ttl(mut self, ttl: Duration) -> Self {
        self.capture_cache = CaptureCache::with_ttl(ttl);
        self
    }
}

#[async_trait]
impl TerminalDriver for TmuxDriver {
    async fn list_sessions(&self) -> Result<String, TmuxError> {
        tmux::run_tmux_async(&list_sessions_args(), self.timeout).await
    }

    async fn capture_pane(&self, session_name: &str) -> Result<String, TmuxError> {
        let timeout = self.timeout;
        self.capture_cache
            .get_or_capture(session_name, || async move {
                tmux::run_tmux_async(&capture_pane_args(session_name), timeout).await
            })
            .await
    }

    async fn new_session(&self, session_name: &str, dir: Option<&str>) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&new_session_args(session_name, dir), self.timeout).await?;
        Ok(())
    }

    async fn kill_session(&self, session_name: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&kill_session_args(session_name), self.timeout).await?;
        Ok(())
    }

    async fn send_literal(&self, session_name: &str, keys: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&send_keys_args(session_name, keys), self.timeout).await?;
        Ok(())
    }

    async fn send_enter(&self, session_name: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&send_enter_args(session_name), self.timeout).await?;
        Ok(())
    }

    async fn send_named_key(&self, session_name: &str, key: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&send_named_key_args(session_name, key), self.timeout).await?;
        Ok(())
    }

    async fn set_option(&self, name: &str, value: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&set_option_args(name, value), self.timeout).await?;
        Ok(())
    }

    async fn show_option(&self, name: &str) -> Result<String, TmuxError> {
        tmux::run_tmux_async(&show_option_args(name), self.timeout).await
    }

    async fn display_message(&self, session_name: &str, format: &str) -> Result<String, TmuxError> {
        tmux::run_tmux_async(&display_message_args(session_name, format), self.timeout).await
    }
}

/// Sleep `delay` if non-zero — a no-op fast path for the overwhelming
/// majority of stub calls in tests that never configure
/// [`StubTerminalDriver::set_send_delay`].
async fn maybe_sleep(delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

/// A cloneable, freshly-materializable stand-in for a `Result<String,
/// TmuxError>`. `TmuxError` cannot itself derive `Clone` (its `Io` variant
/// wraps a non-`Clone` `std::io::Error`), so configured stub responses are
/// stored in this shape instead and converted to a real `Result` on each
/// call — that is what lets one configured failure be read by more than one
/// `send_keys`/`capture_pane`/etc. call in a test without being consumed.
#[derive(Debug, Clone)]
pub enum StubOutcome {
    /// Succeed, producing this string as the operation's return value (or
    /// as the input to `.map(|_| ())` for unit-returning operations).
    Ok(String),
    NotInstalled,
    NoServer,
    ExitError {
        code: i32,
        stderr: String,
    },
    Timeout {
        action: &'static str,
        after: Duration,
    },
}

impl StubOutcome {
    /// A successful outcome carrying an empty string — the default for
    /// every operation until a test configures otherwise.
    #[must_use]
    pub fn empty_ok() -> Self {
        StubOutcome::Ok(String::new())
    }

    fn into_string_result(self) -> Result<String, TmuxError> {
        match self {
            StubOutcome::Ok(s) => Ok(s),
            StubOutcome::NotInstalled => Err(TmuxError::NotInstalled),
            StubOutcome::NoServer => Err(TmuxError::NoServer),
            StubOutcome::ExitError { code, stderr } => Err(TmuxError::ExitError { code, stderr }),
            StubOutcome::Timeout { action, after } => Err(TmuxError::Timeout { action, after }),
        }
    }

    fn into_unit_result(self) -> Result<(), TmuxError> {
        self.into_string_result().map(|_| ())
    }
}

/// Recording, test-only `TerminalDriver`: mirrors `StubHttpPost` — an
/// `Arc<Mutex<..>>` of the argv sequences it received, plus a configurable
/// per-operation response, so node tests can assert on outbound argv and on
/// failure handling without spawning tmux. Every recorded argv entry is
/// built with the exact same `*_args` builder `TmuxDriver` would have used,
/// so a recorded call and a real invocation are byte-for-byte comparable.
#[derive(Clone)]
pub struct StubTerminalDriver {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    list_sessions_result: Arc<Mutex<StubOutcome>>,
    capture_pane_result: Arc<Mutex<StubOutcome>>,
    new_session_result: Arc<Mutex<StubOutcome>>,
    kill_session_result: Arc<Mutex<StubOutcome>>,
    send_literal_result: Arc<Mutex<StubOutcome>>,
    send_enter_result: Arc<Mutex<StubOutcome>>,
    send_named_key_result: Arc<Mutex<StubOutcome>>,
    set_option_result: Arc<Mutex<StubOutcome>>,
    /// Default `show_option` answer used when `show_option_by_name` has no
    /// entry for the requested option name.
    show_option_result: Arc<Mutex<StubOutcome>>,
    /// Per-option-name `show_option` overrides. Real tmux stores
    /// `@engine_lease@<session>` and `@operator_hold@<session>` as
    /// distinct options; a test exercising the guarded sender (`EN.9.B`
    /// task 6) needs to configure the lease's answer independently from
    /// the hold's, which the single flat `show_option_result` cannot do.
    show_option_by_name: Arc<Mutex<HashMap<String, StubOutcome>>>,
    display_message_result: Arc<Mutex<StubOutcome>>,
    /// Optional artificial delay `send_literal`/`send_enter` sleep before
    /// returning — lets a concurrency test (task 6) use a paused tokio
    /// clock to prove two sessions' sends overlap instead of serialize,
    /// without relying on real wall-clock timing.
    send_delay: Arc<Mutex<Duration>>,
}

impl Default for StubTerminalDriver {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            list_sessions_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            capture_pane_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            new_session_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            kill_session_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_literal_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_enter_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_named_key_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            set_option_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            show_option_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            show_option_by_name: Arc::new(Mutex::new(HashMap::new())),
            display_message_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_delay: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }
}

impl StubTerminalDriver {
    /// A stub where every operation succeeds by default — the common case
    /// for node tests that only care about outbound argv.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The argv sequence recorded so far, in call order. Each element is
    /// one tmux invocation's full argv (`args[0] == "tmux"`); a multi-
    /// invocation operation like `send_keys` contributes one element per
    /// invocation, in the order they would have run.
    #[must_use]
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, argv: Vec<String>) {
        self.calls.lock().unwrap().push(argv);
    }

    pub fn set_list_sessions_result(&self, outcome: StubOutcome) {
        *self.list_sessions_result.lock().unwrap() = outcome;
    }

    pub fn set_capture_pane_result(&self, outcome: StubOutcome) {
        *self.capture_pane_result.lock().unwrap() = outcome;
    }

    pub fn set_new_session_result(&self, outcome: StubOutcome) {
        *self.new_session_result.lock().unwrap() = outcome;
    }

    pub fn set_kill_session_result(&self, outcome: StubOutcome) {
        *self.kill_session_result.lock().unwrap() = outcome;
    }

    pub fn set_send_literal_result(&self, outcome: StubOutcome) {
        *self.send_literal_result.lock().unwrap() = outcome;
    }

    pub fn set_send_enter_result(&self, outcome: StubOutcome) {
        *self.send_enter_result.lock().unwrap() = outcome;
    }

    pub fn set_send_named_key_result(&self, outcome: StubOutcome) {
        *self.send_named_key_result.lock().unwrap() = outcome;
    }

    pub fn set_set_option_result(&self, outcome: StubOutcome) {
        *self.set_option_result.lock().unwrap() = outcome;
    }

    pub fn set_show_option_result(&self, outcome: StubOutcome) {
        *self.show_option_result.lock().unwrap() = outcome;
    }

    pub fn set_display_message_result(&self, outcome: StubOutcome) {
        *self.display_message_result.lock().unwrap() = outcome;
    }

    /// Override `show_option`'s answer for one exact option `name` only,
    /// independent of the flat [`Self::set_show_option_result`] default —
    /// how a test gives the lease option and the operator-hold option
    /// different answers in the same scenario.
    pub fn set_show_option_result_for(&self, name: impl Into<String>, outcome: StubOutcome) {
        self.show_option_by_name
            .lock()
            .unwrap()
            .insert(name.into(), outcome);
    }

    /// Make every `send_literal`/`send_enter` call sleep `delay` before
    /// returning — a controllable stand-in for real tmux latency, meant to
    /// be paired with `#[tokio::test(start_paused = true)]` so concurrency
    /// assertions run on virtual, not wall-clock, time.
    pub fn set_send_delay(&self, delay: Duration) {
        *self.send_delay.lock().unwrap() = delay;
    }
}

#[async_trait]
impl TerminalDriver for StubTerminalDriver {
    async fn list_sessions(&self) -> Result<String, TmuxError> {
        self.record(list_sessions_args());
        self.list_sessions_result
            .lock()
            .unwrap()
            .clone()
            .into_string_result()
    }

    async fn capture_pane(&self, session_name: &str) -> Result<String, TmuxError> {
        self.record(capture_pane_args(session_name));
        self.capture_pane_result
            .lock()
            .unwrap()
            .clone()
            .into_string_result()
    }

    async fn new_session(&self, session_name: &str, dir: Option<&str>) -> Result<(), TmuxError> {
        self.record(new_session_args(session_name, dir));
        self.new_session_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn kill_session(&self, session_name: &str) -> Result<(), TmuxError> {
        self.record(kill_session_args(session_name));
        self.kill_session_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn send_literal(&self, session_name: &str, keys: &str) -> Result<(), TmuxError> {
        self.record(send_keys_args(session_name, keys));
        let delay = *self.send_delay.lock().unwrap();
        maybe_sleep(delay).await;
        self.send_literal_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn send_enter(&self, session_name: &str) -> Result<(), TmuxError> {
        self.record(send_enter_args(session_name));
        let delay = *self.send_delay.lock().unwrap();
        maybe_sleep(delay).await;
        self.send_enter_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn send_named_key(&self, session_name: &str, key: &str) -> Result<(), TmuxError> {
        self.record(send_named_key_args(session_name, key));
        self.send_named_key_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn set_option(&self, name: &str, value: &str) -> Result<(), TmuxError> {
        self.record(set_option_args(name, value));
        self.set_option_result
            .lock()
            .unwrap()
            .clone()
            .into_unit_result()
    }

    async fn show_option(&self, name: &str) -> Result<String, TmuxError> {
        self.record(show_option_args(name));
        let per_name = self.show_option_by_name.lock().unwrap().get(name).cloned();
        per_name
            .unwrap_or_else(|| self.show_option_result.lock().unwrap().clone())
            .into_string_result()
    }

    async fn display_message(&self, session_name: &str, format: &str) -> Result<String, TmuxError> {
        self.record(display_message_args(session_name, format));
        self.display_message_result
            .lock()
            .unwrap()
            .clone()
            .into_string_result()
    }
}

/// The parameters of one [`GuardedSender::send_keys`] attempt. `run_id` /
/// `nonce` / `identity` / `lease_expires_at_ms` are exactly the fields the
/// caller already holds from having *acquired* the lease
/// ([`crate::lease::SessionLease::acquire`]) before ever reaching a send —
/// this call renews (verifies + extends) rather than re-acquiring, per
/// `hold.rs`'s documented invariant that the lease is retained, never
/// released, for the duration of a hold.
pub struct GuardedSendRequest<'a> {
    pub session_name: &'a str,
    pub keys: &'a str,
    pub run_id: &'a str,
    pub nonce: &'a str,
    pub identity: &'a str,
    pub lease_expires_at_ms: u64,
    pub now_ms: u64,
}

/// Why [`GuardedSender::send_keys`] did not deliver `keys`.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The lease renewal (verifying we still hold the session) failed —
    /// no send was attempted.
    #[error("lease verification failed: {0}")]
    Lease(#[source] LeaseError),
    /// The operator hold refused the send — no send was attempted.
    #[error("send refused: {0}")]
    Hold(#[source] HoldError),
    /// The Enter keypress failed after the literal send succeeded, and the
    /// `C-u` line-clear recovery succeeded — the pane is clean again, but
    /// the original error is still what the caller needs to see.
    #[error("send_keys failed: {0}")]
    SendFailed(#[source] TmuxError),
    /// The Enter keypress failed AND the `C-u` recovery that followed it
    /// also failed — the pane may hold a half-typed line. The worst
    /// outcome, so both errors are surfaced rather than one swallowing the
    /// other.
    #[error("send_keys failed: {source}; C-u line-clear recovery ALSO failed: {recovery}")]
    SendFailedRecoveryFailed {
        #[source]
        source: TmuxError,
        recovery: TmuxError,
    },
}

impl From<LeaseError> for SendError {
    fn from(e: LeaseError) -> Self {
        SendError::Lease(e)
    }
}

impl From<HoldError> for SendError {
    fn from(e: HoldError) -> Self {
        SendError::Hold(e)
    }
}

/// The `C-u` line-clear key sent to recover a pane left half-typed by a
/// literal-succeeded/Enter-failed `send_keys`.
const LINE_CLEAR_KEY: &str = "C-u";

/// Wraps a [`TerminalDriver`] with the per-session send serialization, the
/// lease/hold verification, and the `C-u` recovery `EN.9.B` task 6
/// requires. Every send path goes through here — never call
/// `TerminalDriver::send_keys` directly on a driver a node holds.
///
/// The per-session lock is keyed by session name (never one global lock,
/// which would serialize unrelated sessions against each other) and is
/// held across the full literal+Enter(+recovery) sequence, so two
/// concurrent sends to the SAME session can never interleave their
/// invocations at the pane.
pub struct GuardedSender<'a> {
    driver: &'a dyn TerminalDriver,
    lease: SessionLease<'a>,
    hold: OperatorHold<'a>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl<'a> GuardedSender<'a> {
    #[must_use]
    pub fn new(driver: &'a dyn TerminalDriver) -> Self {
        Self {
            driver,
            lease: SessionLease::new(driver),
            hold: OperatorHold::new(driver),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// The per-session lock, created on first use and reused thereafter —
    /// looking one up never blocks on another session's held lock, only
    /// the brief `std::sync::Mutex` guarding the map itself.
    fn session_lock(&self, session_name: &str) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(session_name.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Verify the lease and the hold, then send `req.keys` (literal +
    /// Enter) to `req.session_name` under that session's exclusive lock,
    /// recovering with a `C-u` line-clear if the Enter send fails after
    /// the literal send succeeded.
    pub async fn send_keys(&self, req: GuardedSendRequest<'_>) -> Result<(), SendError> {
        // Verify (and extend) the lease first: a send must never proceed
        // without the caller demonstrably still holding the session.
        self.lease
            .renew(
                req.session_name,
                req.nonce,
                req.lease_expires_at_ms,
                req.run_id,
                req.identity,
            )
            .await?;

        // Verify no operator hold is active. Reads are unaffected by this
        // check (`hold.rs`'s documented asymmetry) — only this send path
        // calls `guard_send`.
        self.hold.guard_send(req.session_name, req.now_ms).await?;

        let lock = self.session_lock(req.session_name);
        let _guard = lock.lock().await;

        self.driver
            .send_literal(req.session_name, req.keys)
            .await
            .map_err(SendError::SendFailed)?;

        // A deliberate yield between the literal and Enter invocations:
        // widens the window a concurrent same-session sender's own literal
        // send would need to land in for the per-session lock (not luck)
        // to be what prevents an interleaved pane.
        tokio::task::yield_now().await;

        if let Err(enter_err) = self.driver.send_enter(req.session_name).await {
            return match self
                .driver
                .send_named_key(req.session_name, LINE_CLEAR_KEY)
                .await
            {
                Ok(()) => Err(SendError::SendFailed(enter_err)),
                Err(recovery_err) => Err(SendError::SendFailedRecoveryFailed {
                    source: enter_err,
                    recovery: recovery_err,
                }),
            };
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC: `StubTerminalDriver` satisfies the full `TerminalDriver` surface
    /// and records every call's argv — asserted here as `Arc<dyn
    /// TerminalDriver>`, matching how Phase-2 nodes will hold it.
    fn as_trait_object(stub: StubTerminalDriver) -> Arc<dyn TerminalDriver> {
        Arc::new(stub)
    }

    #[tokio::test]
    async fn stub_records_an_exact_argv_sequence_for_send_keys() {
        let stub = StubTerminalDriver::new();
        let driver = as_trait_object(stub.clone());

        driver
            .send_keys("proj-abc", "cargo test")
            .await
            .expect("stub defaults to success");

        let calls = stub.calls();
        assert_eq!(
            calls,
            vec![
                send_keys_args("proj-abc", "cargo test"),
                send_enter_args("proj-abc"),
            ]
        );
    }

    #[tokio::test]
    async fn stub_configured_failure_surfaces_the_same_tmux_error_variant_the_live_driver_would_produce(
    ) {
        let stub = StubTerminalDriver::new();
        stub.set_capture_pane_result(StubOutcome::ExitError {
            code: 1,
            stderr: "can't find session: nope".to_string(),
        });
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub.clone());

        let result = driver.capture_pane("nope").await;

        match result {
            Err(TmuxError::ExitError { code, stderr }) => {
                assert_eq!(code, 1);
                assert_eq!(stderr, "can't find session: nope");
            }
            other => panic!("expected ExitError, got {other:?}"),
        }
        assert_eq!(stub.calls(), vec![capture_pane_args("nope")]);
    }

    #[tokio::test]
    async fn stub_no_server_outcome_maps_to_the_no_server_variant() {
        let stub = StubTerminalDriver::new();
        stub.set_list_sessions_result(StubOutcome::NoServer);
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub);

        let result = driver.list_sessions().await;
        assert!(matches!(result, Err(TmuxError::NoServer)));
    }

    #[tokio::test]
    async fn stub_send_keys_failure_never_records_the_enter_keypress() {
        let stub = StubTerminalDriver::new();
        stub.set_send_literal_result(StubOutcome::NotInstalled);
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub.clone());

        let result = driver.send_keys("proj-abc", "cargo test").await;

        assert!(matches!(result, Err(TmuxError::NotInstalled)));
        // Only the literal send-keys invocation is recorded — a real tmux
        // invocation never reaches the Enter keypress after this failure.
        assert_eq!(stub.calls(), vec![send_keys_args("proj-abc", "cargo test")]);
    }

    #[tokio::test]
    async fn stub_new_session_records_the_builder_argv_and_succeeds_by_default() {
        let stub = StubTerminalDriver::new();
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub.clone());

        driver
            .new_session("proj-abc", Some("/tmp/proj"))
            .await
            .expect("stub defaults to success");

        assert_eq!(
            stub.calls(),
            vec![new_session_args("proj-abc", Some("/tmp/proj"))]
        );
    }

    #[tokio::test]
    async fn stub_set_option_and_show_option_round_trip_argv() {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::Ok("abc123".to_string()));
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub.clone());

        driver
            .set_option("@engine_lease", "abc123")
            .await
            .expect("stub defaults to success");
        let value = driver
            .show_option("@engine_lease")
            .await
            .expect("configured to succeed");

        assert_eq!(value, "abc123");
        assert_eq!(
            stub.calls(),
            vec![
                set_option_args("@engine_lease", "abc123"),
                show_option_args("@engine_lease"),
            ]
        );
    }

    #[tokio::test]
    async fn stub_kill_session_and_send_named_key_record_builder_argv() {
        let stub = StubTerminalDriver::new();
        let driver: Arc<dyn TerminalDriver> = as_trait_object(stub.clone());

        driver.send_named_key("proj-abc", "C-c").await.unwrap();
        driver.kill_session("proj-abc").await.unwrap();

        assert_eq!(
            stub.calls(),
            vec![
                send_named_key_args("proj-abc", "C-c"),
                kill_session_args("proj-abc"),
            ]
        );
    }

    // ── GuardedSender (task 6) ──────────────────────────────────────────

    use crate::hold::operator_hold_option_name;
    use crate::lease::{Lease, LEASE_OPTION};

    fn lease_option_name(session_name: &str) -> String {
        format!("{LEASE_OPTION}@{session_name}")
    }

    /// A stub pre-configured so `GuardedSender::send_keys` clears BOTH
    /// gates: `show_option` answers a live lease matching `nonce` for the
    /// lease option, an empty `@operator_hold` for the hold option (the
    /// two option names differ, exercised independently via
    /// `set_show_option_result_for` — the flat single-value
    /// `show_option_result` cannot distinguish them), and
    /// `display_message` reports `session_attached=0`.
    fn ready_stub(
        session_name: &str,
        nonce: &str,
        run_id: &str,
        identity: &str,
    ) -> StubTerminalDriver {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result_for(
            lease_option_name(session_name),
            StubOutcome::Ok(
                Lease {
                    run_id: run_id.to_string(),
                    nonce: nonce.to_string(),
                    identity: identity.to_string(),
                    expires_at_ms: 60_000,
                }
                .to_value(),
            ),
        );
        stub.set_show_option_result_for(
            operator_hold_option_name(session_name),
            StubOutcome::empty_ok(),
        );
        stub.set_display_message_result(StubOutcome::Ok("0".to_string()));
        stub
    }

    /// Only the `send-keys`-subcommand calls, in order — filters out the
    /// `show-option`/`display-message` calls the lease/hold verification
    /// also records, isolating exactly the literal/Enter/C-u sequence an
    /// interleave test cares about.
    fn send_subcommand_calls(stub: &StubTerminalDriver) -> Vec<Vec<String>> {
        stub.calls()
            .into_iter()
            .filter(|argv| argv.get(1).map(String::as_str) == Some("send-keys"))
            .collect()
    }

    #[tokio::test]
    async fn concurrent_sends_to_one_session_never_interleave() {
        let session = "proj-abc";
        let nonce = "nonce-1";
        let stub = ready_stub(session, nonce, "run-1", "worker-1");
        let sender = GuardedSender::new(&stub);

        let req = |keys: &'static str| GuardedSendRequest {
            session_name: session,
            keys,
            run_id: "run-1",
            nonce,
            identity: "worker-1",
            lease_expires_at_ms: 60_000,
            now_ms: 0,
        };

        let (a, b) = tokio::join!(sender.send_keys(req("AAA")), sender.send_keys(req("BBB")));
        a.expect("A succeeds");
        b.expect("B succeeds");

        let calls = send_subcommand_calls(&stub);
        assert_eq!(calls.len(), 4, "two complete literal+Enter pairs");
        // Whichever literal ran first, its Enter must be the VERY NEXT
        // send-keys call — never the other sender's literal. That is what
        // "never interleave" means at the argv level: a mutex bug would
        // put the second literal at index 1 instead.
        assert_eq!(calls[1], send_enter_args(session));
        assert_eq!(calls[3], send_enter_args(session));
        assert!(
            calls[0] == send_keys_args(session, "AAA")
                || calls[0] == send_keys_args(session, "BBB")
        );
        assert!(
            calls[2] == send_keys_args(session, "AAA")
                || calls[2] == send_keys_args(session, "BBB")
        );
        assert_ne!(
            calls[0], calls[2],
            "the two literals sent were the two distinct commands"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_sends_to_different_sessions_are_not_serialized() {
        // Real (small) wall-clock delay, not a paused virtual clock —
        // `term-core`'s `tokio` feature is `full` without `test-util`, so
        // `start_paused` is unavailable. The margin below (< 3 legs when
        // 4 sequential legs would prove a serialization bug) is generous
        // enough not to flake under normal scheduling jitter.
        let nonce = "nonce-1";
        let stub_a = ready_stub("sess-a", nonce, "run-1", "worker-1");
        let stub_b = ready_stub("sess-b", nonce, "run-1", "worker-1");
        let delay = Duration::from_millis(80);
        stub_a.set_send_delay(delay);
        stub_b.set_send_delay(delay);
        let sender_a = GuardedSender::new(&stub_a);
        let sender_b = GuardedSender::new(&stub_b);

        let req = |session: &'static str| GuardedSendRequest {
            session_name: session,
            keys: "hello",
            run_id: "run-1",
            nonce,
            identity: "worker-1",
            lease_expires_at_ms: 60_000,
            now_ms: 0,
        };

        let start = std::time::Instant::now();
        let (a, b) = tokio::join!(
            sender_a.send_keys(req("sess-a")),
            sender_b.send_keys(req("sess-b"))
        );
        a.expect("A succeeds");
        b.expect("B succeeds");
        let elapsed = start.elapsed();

        // Each session's own send pays two legs of `delay` (literal, then
        // Enter). If the two DIFFERENT sessions were wrongly serialized
        // against each other, the total would double to ~4 legs. Not
        // serialized: the elapsed time stays near the depth of ONE
        // session's own chain, not the sum of both.
        assert!(
            elapsed < delay * 3,
            "different sessions must overlap, not serialize: elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    async fn enter_failure_sends_c_u_recovery_and_returns_the_original_error() {
        let session = "proj-abc";
        let nonce = "nonce-1";
        let stub = ready_stub(session, nonce, "run-1", "worker-1");
        stub.set_send_enter_result(StubOutcome::ExitError {
            code: 1,
            stderr: "pane gone".to_string(),
        });
        let sender = GuardedSender::new(&stub);

        let result = sender
            .send_keys(GuardedSendRequest {
                session_name: session,
                keys: "cargo test",
                run_id: "run-1",
                nonce,
                identity: "worker-1",
                lease_expires_at_ms: 60_000,
                now_ms: 0,
            })
            .await;

        match result {
            Err(SendError::SendFailed(TmuxError::ExitError { code, stderr })) => {
                assert_eq!(code, 1);
                assert_eq!(stderr, "pane gone");
            }
            other => panic!("expected the ORIGINAL Enter error, got {other:?}"),
        }

        let calls = send_subcommand_calls(&stub);
        assert_eq!(
            calls,
            vec![
                send_keys_args(session, "cargo test"),
                send_enter_args(session),
                send_named_key_args(session, LINE_CLEAR_KEY),
            ],
            "literal, failed Enter, then the C-u recovery"
        );
    }

    #[tokio::test]
    async fn failed_c_u_recovery_is_surfaced_not_swallowed() {
        let session = "proj-abc";
        let nonce = "nonce-1";
        let stub = ready_stub(session, nonce, "run-1", "worker-1");
        stub.set_send_enter_result(StubOutcome::ExitError {
            code: 1,
            stderr: "pane gone".to_string(),
        });
        stub.set_send_named_key_result(StubOutcome::NoServer);
        let sender = GuardedSender::new(&stub);

        let result = sender
            .send_keys(GuardedSendRequest {
                session_name: session,
                keys: "cargo test",
                run_id: "run-1",
                nonce,
                identity: "worker-1",
                lease_expires_at_ms: 60_000,
                now_ms: 0,
            })
            .await;

        match result {
            Err(SendError::SendFailedRecoveryFailed { source, recovery }) => {
                assert!(matches!(source, TmuxError::ExitError { code: 1, .. }));
                assert!(matches!(recovery, TmuxError::NoServer));
            }
            other => panic!("expected both the original AND the recovery error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_never_proceeds_without_a_verified_lease() {
        let session = "proj-abc";
        // No lease configured at all: `show_option` defaults to empty_ok,
        // so `SessionLease::renew` sees no existing lease and returns
        // `NotOurs` before any send-keys call is made.
        let stub = StubTerminalDriver::new();
        stub.set_display_message_result(StubOutcome::Ok("0".to_string()));
        let sender = GuardedSender::new(&stub);

        let result = sender
            .send_keys(GuardedSendRequest {
                session_name: session,
                keys: "cargo test",
                run_id: "run-1",
                nonce: "nonce-1",
                identity: "worker-1",
                lease_expires_at_ms: 60_000,
                now_ms: 0,
            })
            .await;

        assert!(matches!(result, Err(SendError::Lease(_))));
        assert!(
            send_subcommand_calls(&stub).is_empty(),
            "no send was attempted"
        );
    }

    #[tokio::test]
    async fn send_never_proceeds_while_a_hold_is_active() {
        let session = "proj-abc";
        let nonce = "nonce-1";
        let stub = ready_stub(session, nonce, "run-1", "worker-1");
        // Override the hold signal back to attached — the lease is still
        // fine, but a live operator attach must still refuse the send.
        stub.set_display_message_result(StubOutcome::Ok("1".to_string()));
        let sender = GuardedSender::new(&stub);

        let result = sender
            .send_keys(GuardedSendRequest {
                session_name: session,
                keys: "cargo test",
                run_id: "run-1",
                nonce,
                identity: "worker-1",
                lease_expires_at_ms: 60_000,
                now_ms: 0,
            })
            .await;

        assert!(matches!(result, Err(SendError::Hold(_))));
        assert!(
            send_subcommand_calls(&stub).is_empty(),
            "no send was attempted"
        );
    }
}
