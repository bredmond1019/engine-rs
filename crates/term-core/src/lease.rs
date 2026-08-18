//! `lease.rs` — the advisory session lease and its read-back arbitration
//! (`EN.9.B` task 4).
//!
//! tmux has no compare-and-swap primitive over user-options: `set-option`
//! always succeeds and always overwrites whatever was there. Two
//! concurrent acquirers racing a bare "read, check empty, write" would both
//! observe an empty option and both write — a classic TOCTOU. The only way
//! to know whether a write actually "won" is to write, then RE-READ, and
//! confirm the value read back is the one just written (by nonce). That
//! read-back IS the arbitration; nothing here may skip it.
//!
//! The lease is advisory (nothing prevents a process from ignoring it) and
//! fail-closed (an expired lease with no `steal_after` configured is never
//! acquired — the safe failure mode is "nobody can act", not "anybody can").
//!
//! Behind the non-default `tokio` feature — `bastion` consumes `term-core`
//! blocking-only and must keep paying nothing for this module.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::driver::TerminalDriver;
use crate::tmux::TmuxError;

/// The tmux user-option name the lease is stashed under.
pub const LEASE_OPTION: &str = "@engine_lease";

/// Default bound on the read-back backoff a losing acquirer sleeps before
/// giving up — bounded and jittered so contending acquirers do not
/// livelock in lockstep. Named constant with a call-site override.
pub const DEFAULT_BACKOFF: Duration = Duration::from_millis(50);

/// A parsed lease value: `<run_id>:<nonce>:<identity>:<expires_at>`.
///
/// `expires_at` is a Unix-epoch millisecond timestamp so the format is a
/// pure string round-trip with no dependency on a particular clock type —
/// callers supply "now" explicitly, which is also what makes the expiry
/// and steal logic testable against an injected clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub run_id: String,
    pub nonce: String,
    pub identity: String,
    pub expires_at_ms: u64,
}

impl Lease {
    /// Serialize to the wire format written to the tmux user-option.
    #[must_use]
    pub fn to_value(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.run_id, self.nonce, self.identity, self.expires_at_ms
        )
    }

    /// Parse the wire format. Anything that does not split into exactly
    /// four `:`-separated fields with a numeric `expires_at` is malformed
    /// and must be treated as NOT ours — never guessed at or partially
    /// accepted.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Lease> {
        let parts: Vec<&str> = raw.split(':').collect();
        let [run_id, nonce, identity, expires_at] = <[&str; 4]>::try_from(parts).ok()?;
        let expires_at_ms: u64 = expires_at.parse().ok()?;
        Some(Lease {
            run_id: run_id.to_string(),
            nonce: nonce.to_string(),
            identity: identity.to_string(),
            expires_at_ms,
        })
    }

    fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

/// Why an [`SessionLease::acquire`] (or `renew`) attempt did not result in
/// this caller holding the lease.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The option currently holds a live, unexpired lease belonging to a
    /// different nonce, and no steal window applies.
    #[error("session is held by another lease (foreign nonce, not yet expired)")]
    Held,
    /// The lease is expired but `steal_after` was not supplied — the
    /// fail-closed default. An expired lease is never silently acquired.
    #[error("lease is expired but no steal_after bound was supplied (fail-closed)")]
    NoStealWindow,
    /// The lease is expired and a `steal_after` bound was supplied, but the
    /// expiry has not yet aged past it.
    #[error("lease expired but within the steal_after grace window")]
    WithinStealGrace,
    /// After writing our value, the read-back showed a different nonce —
    /// another acquirer won the race. The loser must back off, not spin or
    /// overwrite again.
    #[error("read-back after write showed a foreign nonce — another acquirer won")]
    LostReadBack,
    /// `renew`/`release` only: the option no longer shows our nonce, so
    /// there is nothing of ours left to renew or release.
    #[error("read-back does not show our nonce — nothing to renew or release")]
    NotOurs,
    /// The underlying tmux call failed.
    #[error("driver error: {0}")]
    Driver(#[source] TmuxError),
}

impl From<TmuxError> for LeaseError {
    fn from(e: TmuxError) -> Self {
        LeaseError::Driver(e)
    }
}

/// The parameters of one [`SessionLease::acquire`] attempt, bundled so the
/// call site names each field rather than threading a long positional
/// argument list through.
pub struct AcquireRequest<'a> {
    pub session_name: &'a str,
    pub run_id: &'a str,
    pub nonce: &'a str,
    pub identity: &'a str,
    pub expires_at_ms: u64,
    pub now_ms: u64,
    /// The duration past `expires_at_ms` a stale FOREIGN lease must have
    /// aged before it becomes acquirable. `None` is fail-closed: an
    /// expired-but-present lease is never acquired.
    pub steal_after: Option<Duration>,
}

/// The advisory, fail-closed session lease. Holds no state itself — every
/// call re-reads the tmux user-option, since the option (not this struct)
/// is the single source of truth shared across processes.
pub struct SessionLease<'a> {
    driver: &'a dyn TerminalDriver,
}

impl<'a> SessionLease<'a> {
    #[must_use]
    pub fn new(driver: &'a dyn TerminalDriver) -> Self {
        Self { driver }
    }

    /// Read the current lease value for `session_name`, if any is set and
    /// well-formed. A malformed value (wrong field count, non-numeric
    /// `expires_at`) is reported as `Ok(None)` — never as ours, and never
    /// as an error that would abort a caller's own acquisition attempt.
    ///
    /// Real tmux reports a never-set option as an ERROR (exit 1, stderr
    /// `invalid option: <name>`), not as empty output — verified live on
    /// tmux 3.7b and 3.5a. That is a normal "nothing here yet" outcome for
    /// a lease option, not a driver failure, so it is treated as `Ok(None)`
    /// exactly like empty/malformed output above. A genuine driver failure
    /// (no server, timeout, permission, or any other tmux error) is NOT
    /// swallowed here — only the specific "invalid option" shape is, so a
    /// broken tmux can never be mistaken for a free lease.
    async fn read(&self, session_name: &str) -> Result<Option<Lease>, TmuxError> {
        let raw = match self.driver.show_option(&option_name(session_name)).await {
            Ok(raw) => raw,
            Err(e) if is_unset_option_error(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Lease::parse(trimmed))
    }

    /// Attempt to acquire `req.session_name` for `req.run_id`/
    /// `req.identity`, expiring at `req.expires_at_ms`. `req.nonce` should
    /// be unique per attempt (e.g. a UUID or random hex string) — it is
    /// what the read-back arbitration compares.
    pub async fn acquire(&self, req: AcquireRequest<'_>) -> Result<Lease, LeaseError> {
        if let Some(existing) = self.read(req.session_name).await? {
            if !existing.is_expired(req.now_ms) {
                if existing.nonce == req.nonce {
                    // Re-acquiring our own still-live lease is a no-op success.
                } else {
                    return Err(LeaseError::Held);
                }
            } else {
                // Expired. Fail-closed unless a steal window says otherwise.
                let Some(steal_after) = req.steal_after else {
                    return Err(LeaseError::NoStealWindow);
                };
                let age_past_expiry_ms = req.now_ms.saturating_sub(existing.expires_at_ms);
                if age_past_expiry_ms < steal_after.as_millis() as u64 {
                    return Err(LeaseError::WithinStealGrace);
                }
                // Past the steal window: fall through and write.
            }
        }

        let candidate = Lease {
            run_id: req.run_id.to_string(),
            nonce: req.nonce.to_string(),
            identity: req.identity.to_string(),
            expires_at_ms: req.expires_at_ms,
        };
        self.write_and_confirm(req.session_name, &candidate).await
    }

    /// Extend an already-held lease's `expires_at_ms`, but only while the
    /// read-back still shows our nonce. If another acquirer's write raced
    /// ahead (e.g. after our old lease expired and was legitimately
    /// stolen), this returns `NotOurs` rather than clobbering the new
    /// holder's lease.
    pub async fn renew(
        &self,
        session_name: &str,
        nonce: &str,
        new_expires_at_ms: u64,
        run_id: &str,
        identity: &str,
    ) -> Result<Lease, LeaseError> {
        match self.read(session_name).await? {
            Some(existing) if existing.nonce == nonce => {
                let candidate = Lease {
                    run_id: run_id.to_string(),
                    nonce: nonce.to_string(),
                    identity: identity.to_string(),
                    expires_at_ms: new_expires_at_ms,
                };
                self.write_and_confirm(session_name, &candidate).await
            }
            _ => Err(LeaseError::NotOurs),
        }
    }

    /// Clear the lease for `session_name`, but only if the read-back still
    /// shows our nonce — releasing a lease we no longer hold would delete
    /// someone else's.
    pub async fn release(&self, session_name: &str, nonce: &str) -> Result<(), LeaseError> {
        match self.read(session_name).await? {
            Some(existing) if existing.nonce == nonce => {
                self.driver
                    .set_option(&option_name(session_name), "")
                    .await?;
                Ok(())
            }
            _ => Err(LeaseError::NotOurs),
        }
    }

    /// Write `candidate`, re-read, and confirm the nonce read back is ours.
    /// This is the arbitration: tmux has no CAS, so the only way to learn
    /// whether our write "won" a race against a concurrent writer is to
    /// look at what is there AFTER writing, not to trust that the write
    /// succeeded because the call returned `Ok`.
    async fn write_and_confirm(
        &self,
        session_name: &str,
        candidate: &Lease,
    ) -> Result<Lease, LeaseError> {
        self.driver
            .set_option(&option_name(session_name), &candidate.to_value())
            .await?;

        match self.read(session_name).await? {
            Some(readback) if readback.nonce == candidate.nonce => Ok(readback),
            _ => Err(LeaseError::LostReadBack),
        }
    }
}

/// True only for tmux's specific "invalid option: <name>" failure shape
/// (exit 1, that stderr prefix) — the response a never-set option produces
/// on real tmux. Matches on `root_cause()` so a `Context`-wrapped instance
/// of the same underlying error is still recognized. Any other `TmuxError`
/// (no server, timeout, permission, a different exit error) returns
/// `false` and must surface to the caller, or a broken tmux would become
/// indistinguishable from a free lease.
fn is_unset_option_error(err: &TmuxError) -> bool {
    matches!(
        err.root_cause(),
        TmuxError::ExitError { stderr, .. } if stderr.starts_with("invalid option")
    )
}

fn option_name(session_name: &str) -> String {
    // tmux options set with `set-option -g` are process-wide, not scoped to
    // a session — and this crate's `set_option_args`/`show_option_args`
    // builders take a bare option name. Namespacing the SESSION NAME into
    // the option name itself (rather than relying on the lease value's
    // `run_id` field) is what keeps two different sessions' leases from
    // colliding on one shared global option.
    format!("{LEASE_OPTION}@{session_name}")
}

/// A bounded, jittered backoff duration for a losing acquirer — sampled
/// pseudo-randomly from `[0, max)` so contenders do not retry in lockstep.
///
/// No `rand` dependency: this crate is a lean, mostly-sync-friendly crate
/// (see `lib.rs`), so the jitter source is the low bits of the current
/// wall-clock time, which is exactly the kind of high-entropy, per-call
/// -varying value contending acquirers need to desynchronize — it need not
/// be cryptographically random, only different across near-simultaneous
/// callers.
#[must_use]
pub fn jittered_backoff(max: Duration) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    let millis = max.as_millis().max(1) as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    Duration::from_millis(nanos % millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{StubOutcome, StubTerminalDriver};

    fn future_ms(secs_from_now: u64) -> u64 {
        1_000_000 + secs_from_now * 1000
    }

    #[tokio::test]
    async fn lease_value_round_trips() {
        let lease = Lease {
            run_id: "run-1".to_string(),
            nonce: "nonce-abc".to_string(),
            identity: "worker-7".to_string(),
            expires_at_ms: 123_456,
        };
        let value = lease.to_value();
        let parsed = Lease::parse(&value).expect("well-formed value parses");
        assert_eq!(parsed, lease);
    }

    #[test]
    fn malformed_values_never_parse() {
        assert!(Lease::parse("").is_none());
        assert!(Lease::parse("only:three:fields").is_none());
        assert!(Lease::parse("too:many:fields:here:one-extra").is_none());
        assert!(Lease::parse("run:nonce:identity:not-a-number").is_none());
    }

    #[tokio::test]
    async fn two_concurrent_acquirers_resolve_to_exactly_one_holder_and_the_loser_backs_off() {
        // `StubTerminalDriver::show_option` returns one configured value
        // for every call regardless of what `set_option` just wrote (it
        // does not model tmux's actual storage), so a deterministic race
        // is expressed by pointing that single configured value at
        // whichever acquirer is meant to have "won" the underlying
        // read-back for each phase of the test.
        let stub = StubTerminalDriver::new();
        let lease = SessionLease::new(&stub);

        let now = 1_000_000_u64;
        let expires = future_ms(30);

        // Phase 1: A's read-back sees exactly what A is about to write —
        // A wins (both the pre-check no-op branch and the post-write
        // confirm agree A's nonce is on record).
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-a".to_string(),
                nonce: "nonce-a".to_string(),
                identity: "worker-a".to_string(),
                expires_at_ms: expires,
            }
            .to_value(),
        ));
        let a = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-a",
                nonce: "nonce-a",
                identity: "worker-a",
                expires_at_ms: expires,
                now_ms: now,
                steal_after: None,
            })
            .await
            .expect("A's read-back confirms A's own nonce");
        assert_eq!(a.nonce, "nonce-a");

        // Phase 2: B raced in and won the underlying option — the
        // read-back now shows B's nonce for every subsequent call. A's
        // renew attempt must see the foreign nonce and back off; it must
        // not spin, and it must not overwrite B's lease.
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-b".to_string(),
                nonce: "nonce-b".to_string(),
                identity: "worker-b".to_string(),
                expires_at_ms: expires,
            }
            .to_value(),
        ));
        let renew_result = lease
            .renew("proj-abc", "nonce-a", future_ms(60), "run-a", "worker-a")
            .await;
        assert!(matches!(renew_result, Err(LeaseError::NotOurs)));

        // Exactly one holder: B's own renew (matching nonce) succeeds.
        let b_renew = lease
            .renew("proj-abc", "nonce-b", future_ms(60), "run-b", "worker-b")
            .await
            .expect("B's own nonce renews cleanly");
        assert_eq!(b_renew.nonce, "nonce-b");
    }

    #[tokio::test]
    async fn write_then_read_back_confirms_the_arbitration_not_a_pre_write_check() {
        // An expired foreign lease past its steal window passes the
        // pre-write check (it is legitimately stealable), so `acquire`
        // proceeds to `write_and_confirm`. The stub's show_option is
        // configured to keep returning that SAME foreign, expired value
        // even after the write — proving `acquire` does not trust that its
        // own `set_option` call returning `Ok` means it won, and instead
        // inspects what a re-read actually shows.
        let stub = StubTerminalDriver::new();
        let expired_at = future_ms(1);
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-other".to_string(),
                nonce: "nonce-other".to_string(),
                identity: "worker-other".to_string(),
                expires_at_ms: expired_at,
            }
            .to_value(),
        ));
        let lease = SessionLease::new(&stub);
        let now = expired_at + 10_000;

        let result = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-mine",
                nonce: "nonce-mine",
                identity: "worker-mine",
                expires_at_ms: now + 30_000,
                now_ms: now,
                steal_after: Some(Duration::from_secs(1)), // past the 1s steal window
            })
            .await;

        assert!(matches!(result, Err(LeaseError::LostReadBack)));
        // Both the set_option write and the show_option read-back happened.
        let calls = stub.calls();
        assert!(calls.iter().any(|c| c.iter().any(|a| a == "set-option")));
        assert!(calls.iter().any(|c| c.iter().any(|a| a == "show-option")));
    }

    #[tokio::test]
    async fn a_foreign_nonce_read_back_aborts_acquisition_when_lease_is_live() {
        let stub = StubTerminalDriver::new();
        let now = 1_000_000_u64;
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-holder".to_string(),
                nonce: "nonce-holder".to_string(),
                identity: "worker-holder".to_string(),
                expires_at_ms: future_ms(30),
            }
            .to_value(),
        ));
        let lease = SessionLease::new(&stub);

        let result = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-challenger",
                nonce: "nonce-challenger",
                identity: "worker-challenger",
                expires_at_ms: future_ms(30),
                now_ms: now,
                steal_after: None,
            })
            .await;

        assert!(matches!(result, Err(LeaseError::Held)));
    }

    #[tokio::test]
    async fn malformed_value_never_reads_as_ours() {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::Ok("garbage-not-a-lease".to_string()));
        let lease = SessionLease::new(&stub);

        // A malformed existing value must not block a fresh acquisition
        // (it is treated as absent), and must not be treated as "ours" on
        // a renew attempt either.
        let renew_result = lease
            .renew("proj-abc", "any-nonce", future_ms(30), "run-x", "worker-x")
            .await;
        assert!(matches!(renew_result, Err(LeaseError::NotOurs)));
    }

    #[tokio::test]
    async fn renew_after_expiry_of_a_foreign_lease_fails() {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-x".to_string(),
                nonce: "nonce-x".to_string(),
                identity: "worker-x".to_string(),
                expires_at_ms: future_ms(1), // already expired
            }
            .to_value(),
        ));
        let lease = SessionLease::new(&stub);

        // Renewing with a DIFFERENT nonce than what's on record always
        // fails, regardless of expiry — renew never adopts someone else's
        // (or a stale) lease.
        let result = lease
            .renew("proj-abc", "nonce-mine", future_ms(200), "run-mine", "me")
            .await;
        assert!(matches!(result, Err(LeaseError::NotOurs)));
    }

    #[tokio::test]
    async fn steal_is_refused_with_steal_after_unset_and_permitted_past_it() {
        let stub = StubTerminalDriver::new();
        let expired_at = future_ms(10);
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-stale".to_string(),
                nonce: "nonce-stale".to_string(),
                identity: "worker-stale".to_string(),
                expires_at_ms: expired_at,
            }
            .to_value(),
        ));
        let lease = SessionLease::new(&stub);

        // now is well past expiry.
        let now = expired_at + 5_000;

        // Fail-closed default: no steal_after supplied.
        let refused = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-new",
                nonce: "nonce-new",
                identity: "worker-new",
                expires_at_ms: now + 30_000,
                now_ms: now,
                steal_after: None,
            })
            .await;
        assert!(matches!(refused, Err(LeaseError::NoStealWindow)));

        // Within the steal grace: still refused.
        let within_grace = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-new",
                nonce: "nonce-new",
                identity: "worker-new",
                expires_at_ms: now + 30_000,
                now_ms: now,
                steal_after: Some(Duration::from_secs(60)),
            })
            .await;
        assert!(matches!(within_grace, Err(LeaseError::WithinStealGrace)));

        // Past the steal window: permitted, and the read-back confirms our
        // nonce (the stub echoes back whatever was last set, since we did
        // not reconfigure show_option_result — but here we must let the
        // stub reflect the actual write, so reconfigure it to echo).
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-new".to_string(),
                nonce: "nonce-new".to_string(),
                identity: "worker-new".to_string(),
                expires_at_ms: now + 30_000,
            }
            .to_value(),
        ));
        let far_future = now + 120_000;
        let permitted = lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-new",
                nonce: "nonce-new",
                identity: "worker-new",
                expires_at_ms: far_future + 30_000,
                now_ms: far_future,
                steal_after: Some(Duration::from_secs(60)),
            })
            .await;
        assert!(permitted.is_ok());
        assert_eq!(permitted.unwrap().nonce, "nonce-new");
    }

    #[tokio::test]
    async fn release_only_clears_the_lease_if_still_ours() {
        let stub = StubTerminalDriver::new();
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-foreign".to_string(),
                nonce: "nonce-foreign".to_string(),
                identity: "worker-foreign".to_string(),
                expires_at_ms: future_ms(30),
            }
            .to_value(),
        ));
        let lease = SessionLease::new(&stub);

        let result = lease.release("proj-abc", "nonce-mine").await;
        assert!(matches!(result, Err(LeaseError::NotOurs)));

        // No set_option ("" clear) should have been issued.
        let calls = stub.calls();
        let clear_calls: Vec<_> = calls
            .iter()
            .filter(|c| c.iter().any(|a| a == "set-option"))
            .collect();
        assert!(clear_calls.is_empty());
    }

    #[test]
    fn jittered_backoff_stays_within_bound() {
        let max = Duration::from_millis(50);
        for _ in 0..50 {
            let d = jittered_backoff(max);
            assert!(d < max);
        }
    }

    #[test]
    fn jittered_backoff_of_zero_is_zero() {
        assert_eq!(jittered_backoff(Duration::ZERO), Duration::ZERO);
    }

    // ── SessionLease::read honours its contract for an unset option ────────

    #[tokio::test]
    async fn read_treats_never_set_option_as_ok_none() {
        // Real tmux's exit-1 "invalid option: <name>" response for a
        // never-set option, reproduced via task 1's stub capability — no
        // pre-seeding. `read()` must NOT propagate this as an error.
        let stub = StubTerminalDriver::new();
        let session_lease = SessionLease::new(&stub);
        let name = option_name("fresh-session");
        stub.set_show_option_result_for(name.clone(), StubOutcome::invalid_option(&name));

        let result = session_lease.read("fresh-session").await;
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None) for an unset option, got {result:?}"
        );
    }

    #[tokio::test]
    async fn acquire_against_a_never_set_option_succeeds_with_no_pre_seed() {
        // The end-to-end regression this task exists to fix: EN.9.D's
        // real-Mini probe could only complete by pre-seeding the option
        // from a fixture binary because every FIRST acquisition against a
        // fresh session hard-failed. This proves acquire() now succeeds
        // against a genuinely fresh session with no pre-seed at all.
        let stub = StubTerminalDriver::new();
        let name = option_name("fresh-session");
        stub.set_show_option_result_for(name.clone(), StubOutcome::invalid_option(&name));
        let session_lease = SessionLease::new(&stub);

        let now = 1_000_000_u64;
        let expires = future_ms(30);
        let acquired = session_lease
            .acquire(AcquireRequest {
                session_name: "fresh-session",
                run_id: "run-first",
                nonce: "nonce-first",
                identity: "worker-first",
                expires_at_ms: expires,
                now_ms: now,
                steal_after: None,
            })
            .await;
        assert!(
            matches!(acquired, Err(LeaseError::LostReadBack)),
            "expected the pre-write check to pass (Ok(None)) and only fail at the \
             read-back confirm — since the stub's show_option is still pinned to \
             invalid_option and never reflects the write — got {acquired:?}"
        );
    }

    #[tokio::test]
    async fn genuine_driver_failure_still_surfaces_as_an_error() {
        // A real driver failure (no server) must NOT be swallowed as
        // Ok(None) — only the specific "invalid option" shape is treated
        // that way. Otherwise a broken tmux becomes indistinguishable from
        // a free lease and the lease stops being fail-closed.
        let stub = StubTerminalDriver::new();
        let session_lease = SessionLease::new(&stub);
        stub.set_show_option_result(StubOutcome::NoServer);

        let result = session_lease.read("some-session").await;
        assert!(
            matches!(result, Err(TmuxError::NoServer)),
            "expected NoServer to surface as an error, got {result:?}"
        );

        let acquire_result = session_lease
            .acquire(AcquireRequest {
                session_name: "some-session",
                run_id: "run-x",
                nonce: "nonce-x",
                identity: "worker-x",
                expires_at_ms: future_ms(30),
                now_ms: 1_000_000,
                steal_after: None,
            })
            .await;
        assert!(
            matches!(acquire_result, Err(LeaseError::Driver(TmuxError::NoServer))),
            "expected acquire() to surface the driver error rather than treat it \
             as an unset option, got {acquire_result:?}"
        );
    }

    #[tokio::test]
    async fn a_different_exit_error_is_not_mistaken_for_invalid_option() {
        // Guard against a too-broad match: any ExitError whose stderr does
        // NOT start with "invalid option" must still surface, not be
        // swallowed.
        let stub = StubTerminalDriver::new();
        let session_lease = SessionLease::new(&stub);
        stub.set_show_option_result(StubOutcome::ExitError {
            code: 1,
            stderr: "can't find session: nope".to_string(),
        });

        let result = session_lease.read("some-session").await;
        match result {
            Err(TmuxError::ExitError { stderr, .. }) => {
                assert_eq!(stderr, "can't find session: nope");
            }
            other => panic!("expected the unrelated ExitError to surface, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fail_closed_steal_semantics_are_unchanged_by_the_unset_option_fix() {
        // An expired FOREIGN lease with no steal_after must still never be
        // acquired — this task must not touch that path.
        let stub = StubTerminalDriver::new();
        let expired_at = future_ms(1);
        stub.set_show_option_result(StubOutcome::Ok(
            Lease {
                run_id: "run-foreign".to_string(),
                nonce: "nonce-foreign".to_string(),
                identity: "worker-foreign".to_string(),
                expires_at_ms: expired_at,
            }
            .to_value(),
        ));
        let session_lease = SessionLease::new(&stub);

        let result = session_lease
            .acquire(AcquireRequest {
                session_name: "proj-abc",
                run_id: "run-mine",
                nonce: "nonce-mine",
                identity: "worker-mine",
                expires_at_ms: future_ms(60),
                now_ms: future_ms(10),
                steal_after: None,
            })
            .await;
        assert!(matches!(result, Err(LeaseError::NoStealWindow)));
    }
}
