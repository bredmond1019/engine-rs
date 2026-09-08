//! `CoordHandle` — a Rust chain's grip on the fleet's shared coordination tree.
//!
//! `EN.15.D` task 1. `EN.15.C` built the write seam (`crate::coord::write`) that every route
//! goes through; this module is the first thing in the *orchestration* workflow that actually
//! calls it. A `CoordHandle` is nothing more than the handful of identifiers every one of those
//! calls needs (`lock_dir`, `repo`, `lane`, `agent`) plus an injectable clock, wrapped in
//! methods that delegate straight to `coord::write`'s existing `register`/`heartbeat`/`lease`/
//! `unlease`/`release`/`drain` functions — there is no new coordination *logic* here, only the
//! wiring `integrate_chain_impl` (`EN.15.D` task 1, same task) needs to call it at the right
//! points in its loop.
//!
//! ## The clock seam
//!
//! `now_iso` is a `fn() -> String`, not a call to `chrono::Utc::now()` baked into each method.
//! `mev::brain::lease::check_quiesce` treats any lease (or registry claim) whose `heartbeat` is
//! older than 10800s as stale — a hardcoded timestamp literal in a test therefore passes for
//! three hours and then fails forever after, which is exactly the defect this crate hit and
//! fixed in `da4c847` (see `crate::coord::write`'s own test module for the `now_iso()` helper
//! this one mirrors). Every method on [`CoordHandle`] reads the current instant through this
//! seam, never through a literal, so a test can inject a fixed-but-freshly-formatted timestamp
//! instead of a frozen one.
//!
//! ## What this module does NOT do
//!
//! It does not resolve a lock dir (`super::super::super::coord::resolve_lock_dir` — actually
//! `crate::coord::resolve_lock_dir` — is the caller's job, same as every other `coord::write`
//! caller). It does not decide *when* to register/heartbeat/lease/unlease — that decision is
//! `integrate_chain_impl`'s, made at the block boundaries the task record names. It writes no
//! slot record (`RegisterRequest::category` is always `None` here): a Rust orchestration chain
//! is not one of the heavy-lane categories `fleet_concurrency_check.py` gates, so there is
//! nothing for this handle to enforce a capacity cap against.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use okf_core::LeaseKind;

use crate::coord::write::{
    self, CoordWriteError, HeartbeatRequest, LeaseRequest, RegisterOutcome, RegisterRequest,
};

/// Everything a Rust-driven chain needs to appear on the fleet's shared coordination tree:
/// register a lane-agent claim, heartbeat it, take and release the block-scoped repo lease, and
/// drain its own inbox — all against one `lock_dir`, as one `agent` on one `lane`.
#[derive(Clone)]
pub struct CoordHandle {
    /// The resolved `.fleet-locks/` directory every `coord::write` call writes under.
    pub lock_dir: PathBuf,
    /// The repo slug this chain is driving (matches `brain.toml`'s `[[repos]] slug`), and the
    /// key every lease this handle takes is filed under.
    pub repo: String,
    /// The lane name stamped onto the registry claim and the lease record.
    pub lane: String,
    /// The `ListAgents` nickname this chain registers and leases as — the same identity concept
    /// `mev::brain::lease::check_quiesce`'s self-exemption and `EN.15.B`'s `sdlc_event.agent`
    /// already use; this handle introduces no second one.
    pub agent: String,
    /// The clock seam every method reads "now" through. See the module doc's "clock seam"
    /// section for why this can never be a frozen literal.
    pub now_iso: fn() -> String,
}

impl CoordHandle {
    /// Build a handle. `now_iso` is required at construction (not defaulted to
    /// `chrono::Utc::now`) so a caller — production or test — always makes the clock choice
    /// explicit.
    pub fn new(
        lock_dir: PathBuf,
        repo: impl Into<String>,
        lane: impl Into<String>,
        agent: impl Into<String>,
        now_iso: fn() -> String,
    ) -> Self {
        Self {
            lock_dir,
            repo: repo.into(),
            lane: lane.into(),
            agent: agent.into(),
            now_iso,
        }
    }

    /// Register this handle's `agent`/`repo`/`lane` as a lane-agent claim for `roadmap`, at
    /// `<lock_dir>/lane-agents/agent-<agent>.json`. Writes no slot record (`category: None`) —
    /// see the module doc. A repeat call is a heartbeat-via-re-register, exactly like
    /// `coord::write::register`'s own documented semantics: `started_at` is kept from the first
    /// call, `heartbeat` is re-stamped every time.
    pub fn register(&self, roadmap: &str) -> Result<RegisterOutcome, CoordWriteError> {
        let now = (self.now_iso)();
        let req = RegisterRequest {
            agent_name: &self.agent,
            repo: &self.repo,
            lane: &self.lane,
            roadmap,
            host: None,
            category: None,
            now_iso: &now,
            now_epoch: epoch_seconds(),
            pid: std::process::id() as i64,
        };
        write::register(&self.lock_dir, &req)
    }

    /// Re-stamp this handle's registry claim's `heartbeat` field (and, when given,
    /// `current_block`) — `started_at` is left untouched. Errors if this handle was never
    /// registered (nothing to heartbeat).
    pub fn heartbeat(&self, current_block: Option<&str>) -> Result<(), CoordWriteError> {
        let now = (self.now_iso)();
        let req = HeartbeatRequest {
            agent_name: &self.agent,
            host: None,
            now_iso: &now,
            current_block,
            block_started_at: None,
        };
        write::heartbeat(&self.lock_dir, &req)
    }

    /// Take (or renew) an exclusive lease on this handle's `repo`, at
    /// `<lock_dir>/leases/lease-<repo>.json`. `window` names the block(s) this lease claims
    /// exclusivity for; every block it names must already be present in `lane_blocks`, or the
    /// call is refused before anything is written — see `coord::write::lease`'s own doc.
    pub fn lease(
        &self,
        kind: LeaseKind,
        window: Option<&[String]>,
        lane_blocks: &[String],
    ) -> Result<(), CoordWriteError> {
        let now = (self.now_iso)();
        let req = LeaseRequest {
            repo: &self.repo,
            lane: &self.lane,
            agent: &self.agent,
            kind,
            scope: None,
            host: None,
            now_iso: &now,
            window,
            lane_blocks,
        };
        write::lease(&self.lock_dir, &req)
    }

    /// Release the lease on this handle's `repo`, if any. Idempotent — an already-absent lease
    /// returns `Ok(false)` rather than an error.
    pub fn unlease(&self) -> Result<bool, CoordWriteError> {
        write::unlease(&self.lock_dir, &self.repo)
    }

    /// Remove this handle's registry claim. Idempotent — an already-absent claim returns
    /// `Ok(false)` rather than an error.
    pub fn release(&self) -> Result<bool, CoordWriteError> {
        write::release(&self.lock_dir, &self.agent)
    }

    /// Drain this handle's own inbox (`<lock_dir>/queue/<repo>/<lane>/inbox/` ->
    /// `.../processing/`), returning the `message_id` of every file moved.
    pub fn drain(&self) -> Result<Vec<String>, CoordWriteError> {
        let now = (self.now_iso)();
        write::drain(&self.lock_dir, &self.repo, &self.lane, &now)
    }
}

/// Epoch seconds for [`RegisterRequest::now_epoch`] — a real clock read, never a frozen
/// literal, but distinct from the `now_iso` seam: this value only ever reaches a slot record
/// (`category: Some(..)`), which [`CoordHandle::register`] never writes, so nothing compares it
/// against `check_quiesce`'s ISO-timestamp staleness window.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_iso() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn handle(lock_dir: PathBuf) -> CoordHandle {
        CoordHandle::new(lock_dir, "engine-rs", "engine-rs", "engine-rs-1", now_iso)
    }

    #[test]
    fn register_writes_a_registry_claim_at_the_expected_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let outcome = h.register("coordination-layer-port").expect("register");
        assert!(outcome.allowed);
        let claim_path = dir
            .path()
            .join("lane-agents")
            .join("agent-engine-rs-1.json");
        assert!(claim_path.exists(), "expected {claim_path:?} to exist");
    }

    #[test]
    fn heartbeat_after_register_updates_the_same_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        h.register("coordination-layer-port").expect("register");
        h.heartbeat(Some("EN.15.D")).expect("heartbeat");
        let claim_path = dir
            .path()
            .join("lane-agents")
            .join("agent-engine-rs-1.json");
        let text = std::fs::read_to_string(&claim_path).expect("read claim");
        assert!(text.contains("EN.15.D"));
    }

    #[test]
    fn heartbeat_without_a_prior_register_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        assert!(h.heartbeat(None).is_err());
    }

    #[test]
    fn lease_writes_a_lease_record_at_the_expected_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let lane_blocks = vec!["EN.15.D".to_string()];
        let window = vec!["EN.15.D".to_string()];
        h.lease(LeaseKind::Exclusive, Some(&window), &lane_blocks)
            .expect("lease");
        let lease_path = dir.path().join("leases").join("lease-engine-rs.json");
        assert!(lease_path.exists(), "expected {lease_path:?} to exist");
    }

    #[test]
    fn unlease_then_lease_file_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let lane_blocks = vec!["EN.15.D".to_string()];
        h.lease(LeaseKind::Exclusive, None, &lane_blocks)
            .expect("lease");
        let removed = h.unlease().expect("unlease");
        assert!(removed);
        let lease_path = dir.path().join("leases").join("lease-engine-rs.json");
        assert!(!lease_path.exists());
    }

    #[test]
    fn release_removes_the_registry_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        h.register("coordination-layer-port").expect("register");
        let removed = h.release().expect("release");
        assert!(removed);
        let claim_path = dir
            .path()
            .join("lane-agents")
            .join("agent-engine-rs-1.json");
        assert!(!claim_path.exists());
    }

    #[test]
    fn drain_with_no_messages_is_an_empty_vec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let moved = h.drain().expect("drain");
        assert!(moved.is_empty());
    }
}
