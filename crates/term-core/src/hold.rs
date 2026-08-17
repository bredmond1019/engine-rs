//! `hold.rs` — the operator hold, and the read/write asymmetry it enforces
//! (`EN.9.B` task 5).
//!
//! Two hold signals feed one status, in precedence order:
//!
//! 1. `@operator_hold` — a tmux user-option written by managed attach paths
//!    the moment they take the pane. Authoritative when present.
//! 2. `#{session_attached}` (via `tmux display-message -p`) — the fallback
//!    for a RAW `tmux attach` that no managed path ever saw, so a hold is
//!    never silently missed just because nothing wrote the option.
//!
//! Both signals are presence/truthy flags, not a tri-state "explicitly not
//! held" — so "check `@operator_hold` first, fall back to
//! `#{session_attached}`" and "either signal being true holds" describe the
//! same decision. This module computes it as an OR and documents the
//! precedence reading here rather than modelling a third state neither tmux
//! signal can express.
//!
//! **The asymmetry is the point, and it is enforced in exactly one place:**
//! a hold pauses SENDS and never pauses READS. `capture_pane` stays live
//! under a hold — this module exposes no read-side guard at all, which
//! *is* the "reads continue" property, not an oversight. Only
//! [`OperatorHold::guard_send`] can return [`HoldError::Paused`].
//!
//! Detaching does not immediately resume sends: once a session has been
//! seen attached, sends stay paused for a grace window (default
//! [`DEFAULT_DETACH_GRACE`]) after the last attached observation, so a
//! human who glances away for a second does not race a queued send. The
//! grace clock is injected as an explicit `now_ms` on every call — exactly
//! the pattern `lease.rs` uses — so tests exercise the 60s boundary without
//! sleeping real time.
//!
//! **The lease is retained, not released, for the duration of a hold.**
//! This module never touches `@engine_lease` (`crate::lease`) in either
//! direction — it has no reference to a `SessionLease` at all — so that
//! invariant holds by construction: nothing here can release what a caller
//! is holding, and a caller integrating both (task 6) must keep renewing
//! the lease through a hold rather than reaching for `release`.
//!
//! Grace and `steal_after`-style tuning are shipped here as named defaults
//! with call-site overrides, NOT as a per-workflow `Policy` surface — that
//! boundary is `EN.9.G`, built over this Phase-1 block.
//!
//! Behind the non-default `tokio` feature — `bastion` consumes `term-core`
//! blocking-only and must keep paying nothing for this module.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::driver::TerminalDriver;
use crate::tmux::TmuxError;

/// The tmux user-option name managed attach paths write on take, per
/// [`operator_hold_option_name`]. Mirrors `crate::lease::LEASE_OPTION`'s
/// per-session namespacing so unrelated sessions never collide on one
/// shared global option.
pub const OPERATOR_HOLD_OPTION: &str = "@operator_hold";

/// The tmux format string read via `display-message -p` for the raw-attach
/// fallback signal.
pub const SESSION_ATTACHED_FORMAT: &str = "#{session_attached}";

/// Default grace window sends stay paused for after the last observed
/// attach, once a session has been attached at least once. Named constant
/// with a constructor override (`OperatorHold::with_grace`) — the knob
/// becomes a real per-workflow policy surface only in `EN.9.G`.
pub const DEFAULT_DETACH_GRACE: Duration = Duration::from_secs(60);

/// Why [`OperatorHold::guard_send`] refused a send.
#[derive(Debug, thiserror::Error)]
pub enum HoldError {
    /// The session is attached right now, or was attached within the
    /// detach grace window.
    #[error("sends are paused: operator hold is active (attached={attached}, grace_remaining={grace_remaining:?})")]
    Paused {
        attached: bool,
        grace_remaining: Option<Duration>,
    },
    /// The underlying tmux call failed.
    #[error("driver error: {0}")]
    Driver(#[source] TmuxError),
}

impl From<TmuxError> for HoldError {
    fn from(e: TmuxError) -> Self {
        HoldError::Driver(e)
    }
}

/// The resolved hold status for one session at one point in time (`now_ms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldStatus {
    /// Raw `@operator_hold` flag, as read this call.
    pub operator_hold: bool,
    /// Raw `#{session_attached}` flag, as read this call.
    pub session_attached: bool,
    /// `operator_hold || session_attached` — attached RIGHT NOW, by either
    /// signal. Used only to update the grace-window bookkeeping; callers
    /// wanting "should sends be paused" want [`HoldStatus::sends_paused`],
    /// not this field, since it does not account for the detach grace.
    pub attached_now: bool,
    /// Whether sends should be refused: true while attached, and for the
    /// detach-grace window after the most recent attached observation.
    pub sends_paused: bool,
}

/// The operator hold: reads the two tmux signals and remembers, per
/// session, the last time either was observed true — the state a stateless
/// tmux read-back cannot supply on its own but the detach grace needs.
pub struct OperatorHold<'a> {
    driver: &'a dyn TerminalDriver,
    grace: Duration,
    last_attached_ms: Mutex<HashMap<String, u64>>,
}

impl<'a> OperatorHold<'a> {
    /// Build a hold checker using [`DEFAULT_DETACH_GRACE`].
    #[must_use]
    pub fn new(driver: &'a dyn TerminalDriver) -> Self {
        Self::with_grace(driver, DEFAULT_DETACH_GRACE)
    }

    /// Build a hold checker whose detach grace is `grace` instead of the
    /// default — the override a test (or a future `EN.9.G` policy knob)
    /// reaches for.
    #[must_use]
    pub fn with_grace(driver: &'a dyn TerminalDriver, grace: Duration) -> Self {
        Self {
            driver,
            grace,
            last_attached_ms: Mutex::new(HashMap::new()),
        }
    }

    /// Read both signals for `session_name`, update the grace bookkeeping
    /// against `now_ms`, and return the resolved status. Read-only: this
    /// never blocks or refuses anything by itself — [`OperatorHold::guard_send`]
    /// is what turns a paused status into a refusal.
    pub async fn check(&self, session_name: &str, now_ms: u64) -> Result<HoldStatus, TmuxError> {
        let operator_hold = self.read_operator_hold(session_name).await?;
        let session_attached = self.read_session_attached(session_name).await?;
        let attached_now = operator_hold || session_attached;

        let grace_remaining = {
            let mut last = self.last_attached_ms.lock().unwrap();
            if attached_now {
                last.insert(session_name.to_string(), now_ms);
                None // attached right now: "paused", no grace math needed
            } else {
                match last.get(session_name).copied() {
                    Some(last_ms) => {
                        let elapsed = now_ms.saturating_sub(last_ms);
                        let grace_ms = self.grace.as_millis() as u64;
                        if elapsed < grace_ms {
                            Some(Duration::from_millis(grace_ms - elapsed))
                        } else {
                            None // past grace: fully resumed
                        }
                    }
                    // Never observed attached: nothing to be in grace for.
                    None => None,
                }
            }
        };

        let sends_paused = attached_now || grace_remaining.is_some();

        Ok(HoldStatus {
            operator_hold,
            session_attached,
            attached_now,
            sends_paused,
        })
    }

    /// The gate every send path (`EN.9.B` task 6) must call before acting.
    /// Reads never call this — that omission is the read/write asymmetry.
    pub async fn guard_send(&self, session_name: &str, now_ms: u64) -> Result<(), HoldError> {
        let status = self.check(session_name, now_ms).await?;
        if status.sends_paused {
            let grace_remaining = if status.attached_now {
                None
            } else {
                let last = self.last_attached_ms.lock().unwrap();
                last.get(session_name).map(|&last_ms| {
                    let elapsed = now_ms.saturating_sub(last_ms);
                    let grace_ms = self.grace.as_millis() as u64;
                    Duration::from_millis(grace_ms.saturating_sub(elapsed))
                })
            };
            return Err(HoldError::Paused {
                attached: status.attached_now,
                grace_remaining,
            });
        }
        Ok(())
    }

    async fn read_operator_hold(&self, session_name: &str) -> Result<bool, TmuxError> {
        let raw = self
            .driver
            .show_option(&operator_hold_option_name(session_name))
            .await?;
        Ok(!raw.trim().is_empty())
    }

    async fn read_session_attached(&self, session_name: &str) -> Result<bool, TmuxError> {
        let raw = self
            .driver
            .display_message(session_name, SESSION_ATTACHED_FORMAT)
            .await?;
        Ok(parse_session_attached(&raw))
    }
}

/// Parse a `#{session_attached}` capture — tmux prints the bare digit
/// (`"1"` attached, `"0"` not) with a trailing newline. Anything else
/// (empty, garbled, an error string that slipped through) is treated as
/// NOT attached — fail-open on the READ signal only, never on the write
/// gate, since an unattached false-negative here just means the raw-attach
/// fallback under-reports and `@operator_hold` (when present) still holds.
#[must_use]
pub fn parse_session_attached(raw: &str) -> bool {
    raw.trim() == "1"
}

/// The per-session tmux option name a managed attach path writes/clears.
/// Namespaces the session name into the option name itself, mirroring
/// `crate::lease`'s `option_name` — `set-option -g` is process-wide, so two
/// sessions must not collide on one shared global option.
#[must_use]
pub fn operator_hold_option_name(session_name: &str) -> String {
    format!("{OPERATOR_HOLD_OPTION}@{session_name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{StubOutcome, StubTerminalDriver};

    fn attached_stub() -> StubTerminalDriver {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::empty_ok()); // @operator_hold unset
        stub.set_display_message_result(StubOutcome::Ok("1".to_string())); // session_attached=1
        stub
    }

    fn detached_stub() -> StubTerminalDriver {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::empty_ok());
        stub.set_display_message_result(StubOutcome::Ok("0".to_string()));
        stub
    }

    #[test]
    fn parses_the_bare_attached_digit() {
        assert!(parse_session_attached("1"));
        assert!(parse_session_attached("1\n"));
        assert!(!parse_session_attached("0"));
        assert!(!parse_session_attached(""));
        assert!(!parse_session_attached("garbled"));
    }

    // AC: an attached operator pauses sends while reads continue.
    #[tokio::test]
    async fn attached_pauses_sends_but_never_gates_reads() {
        let stub = attached_stub();
        let hold = OperatorHold::new(&stub);

        let status = hold.check("proj-abc", 0).await.expect("stub succeeds");
        assert!(status.sends_paused);

        let guard = hold.guard_send("proj-abc", 0).await;
        assert!(matches!(
            guard,
            Err(HoldError::Paused { attached: true, .. })
        ));

        // No guard exists for reads at all — capture_pane is unaffected by
        // the hold and succeeds through the same stub driver directly.
        let read = stub.capture_pane("proj-abc").await;
        assert!(read.is_ok());
    }

    // AC: `@operator_hold` alone holds even when `#{session_attached}`
    // reports unattached.
    #[tokio::test]
    async fn operator_hold_flag_holds_even_when_session_attached_reports_zero() {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::Ok("1".to_string())); // @operator_hold set
        stub.set_display_message_result(StubOutcome::Ok("0".to_string())); // session_attached=0
        let hold = OperatorHold::new(&stub);

        let status = hold.check("proj-abc", 0).await.expect("stub succeeds");
        assert!(status.operator_hold);
        assert!(!status.session_attached);
        assert!(status.attached_now);
        assert!(status.sends_paused);
    }

    // AC: sends resume 60s after detach, and not before, verified against
    // an injected clock (no real sleeping).
    #[tokio::test]
    async fn detach_keeps_sends_paused_through_the_grace_window_then_resumes() {
        let stub = attached_stub();
        let hold = OperatorHold::with_grace(&stub, Duration::from_secs(60));

        // Observed attached at t=0.
        let attached_status = hold.check("proj-abc", 0).await.expect("stub succeeds");
        assert!(attached_status.sends_paused);

        // Now detached; reconfigure the stub's live signals accordingly.
        stub.set_display_message_result(StubOutcome::Ok("0".to_string()));

        // At grace - 1s: still within the window, sends stay paused.
        let just_before = hold.check("proj-abc", 59_000).await.expect("stub succeeds");
        assert!(!just_before.attached_now);
        assert!(
            just_before.sends_paused,
            "expected sends still paused 1s before the 60s grace elapses"
        );
        assert!(hold.guard_send("proj-abc", 59_000).await.is_err());

        // At grace + 1s: past the window, sends resume.
        let after = hold.check("proj-abc", 61_000).await.expect("stub succeeds");
        assert!(
            !after.sends_paused,
            "expected sends resumed past the 60s grace"
        );
        assert!(hold.guard_send("proj-abc", 61_000).await.is_ok());
    }

    #[tokio::test]
    async fn never_attached_session_is_never_paused() {
        let stub = detached_stub();
        let hold = OperatorHold::new(&stub);

        let status = hold.check("proj-fresh", 0).await.expect("stub succeeds");
        assert!(!status.attached_now);
        assert!(!status.sends_paused);
        assert!(hold.guard_send("proj-fresh", 0).await.is_ok());
    }

    // AC: precedence when the two signals disagree — either signal alone
    // is sufficient to hold.
    #[tokio::test]
    async fn either_signal_alone_is_sufficient_to_hold() {
        // operator_hold true, session_attached false.
        let a = StubTerminalDriver::new();
        a.set_show_option_result(StubOutcome::Ok("1".to_string()));
        a.set_display_message_result(StubOutcome::Ok("0".to_string()));
        let hold_a = OperatorHold::new(&a);
        assert!(hold_a.check("s", 0).await.unwrap().sends_paused);

        // operator_hold unset (falls back), session_attached true.
        let b = StubTerminalDriver::new();
        b.set_show_option_result(StubOutcome::empty_ok());
        b.set_display_message_result(StubOutcome::Ok("1".to_string()));
        let hold_b = OperatorHold::new(&b);
        assert!(hold_b.check("s", 0).await.unwrap().sends_paused);

        // Neither signal: not held.
        let c = StubTerminalDriver::new();
        c.set_show_option_result(StubOutcome::empty_ok());
        c.set_display_message_result(StubOutcome::Ok("0".to_string()));
        let hold_c = OperatorHold::new(&c);
        assert!(!hold_c.check("s", 0).await.unwrap().sends_paused);
    }

    // AC: no per-workflow `Policy` type is introduced here — grace is a
    // named default with a constructor override, checked structurally by
    // this test compiling against a bare `Duration`, not a `Policy` type.
    #[tokio::test]
    async fn grace_override_is_a_bare_duration_not_a_policy_type() {
        let stub = attached_stub();
        let custom_grace: Duration = Duration::from_millis(10);
        let hold = OperatorHold::with_grace(&stub, custom_grace);
        assert!(hold.check("s", 0).await.unwrap().sends_paused);
    }

    #[tokio::test]
    async fn operator_hold_option_name_is_namespaced_per_session() {
        assert_eq!(
            operator_hold_option_name("proj-abc"),
            "@operator_hold@proj-abc"
        );
        assert_ne!(
            operator_hold_option_name("proj-abc"),
            operator_hold_option_name("proj-def")
        );
    }
}
