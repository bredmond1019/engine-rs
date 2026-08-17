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

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::capture_cache::CaptureCache;
use crate::tmux::{
    self, capture_pane_args, kill_session_args, list_sessions_args, new_session_args,
    send_enter_args, send_keys_args, send_named_key_args, set_option_args, show_option_args,
    TmuxError,
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

    /// Send `keys` literally to `session_name`, followed by an Enter
    /// keypress — the same two-invocation shape as `tmux::send_keys`.
    async fn send_keys(&self, session_name: &str, keys: &str) -> Result<(), TmuxError>;

    /// Send a single named key (e.g. `Escape`, `C-c`) to `session_name`.
    async fn send_named_key(&self, session_name: &str, key: &str) -> Result<(), TmuxError>;

    /// Write a global tmux option (`set-option -g <name> <value>`) —
    /// including an `@`-prefixed user option, used by the session lease
    /// (`EN.9.B` task 3+) to stash its metadata.
    async fn set_option(&self, name: &str, value: &str) -> Result<(), TmuxError>;

    /// Read back a global tmux option previously written with
    /// [`TerminalDriver::set_option`].
    async fn show_option(&self, name: &str) -> Result<String, TmuxError>;
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

    async fn send_keys(&self, session_name: &str, keys: &str) -> Result<(), TmuxError> {
        tmux::run_tmux_async(&send_keys_args(session_name, keys), self.timeout).await?;
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
    send_keys_result: Arc<Mutex<StubOutcome>>,
    send_named_key_result: Arc<Mutex<StubOutcome>>,
    set_option_result: Arc<Mutex<StubOutcome>>,
    show_option_result: Arc<Mutex<StubOutcome>>,
}

impl Default for StubTerminalDriver {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            list_sessions_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            capture_pane_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            new_session_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            kill_session_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_keys_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            send_named_key_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            set_option_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
            show_option_result: Arc::new(Mutex::new(StubOutcome::empty_ok())),
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

    pub fn set_send_keys_result(&self, outcome: StubOutcome) {
        *self.send_keys_result.lock().unwrap() = outcome;
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

    async fn send_keys(&self, session_name: &str, keys: &str) -> Result<(), TmuxError> {
        // Mirrors `TmuxDriver::send_keys` / `tmux::send_keys`: the literal
        // send is recorded first, and the Enter keypress is only recorded
        // (and sent) if the literal send would have succeeded — a real
        // tmux invocation never reaches the second call otherwise.
        self.record(send_keys_args(session_name, keys));
        let outcome = self.send_keys_result.lock().unwrap().clone();
        match outcome {
            StubOutcome::Ok(_) => {
                self.record(send_enter_args(session_name));
                Ok(())
            }
            other => other.into_unit_result(),
        }
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
        self.show_option_result
            .lock()
            .unwrap()
            .clone()
            .into_string_result()
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
        stub.set_send_keys_result(StubOutcome::NotInstalled);
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
}
