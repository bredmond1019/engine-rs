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

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use okf_core::{LeaseKind, Message, MessageRecord};

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
    /// `.../processing/`), returning one [`DrainedMessage`] per file moved, in filename order.
    ///
    /// `EN.15.D` task 2. This does NOT delegate to [`write::drain`]: that function silently
    /// leaves a malformed file (invalid JSON, or valid JSON missing `message_id`) sitting in
    /// `inbox/` forever — never moved, never receipted — which is exactly the silent-skip this
    /// task's acceptance criteria forbid ("a malformed inbox file must be quarantined into
    /// `processing/` WITH a receipt, never silently skipped"). This method instead quarantines
    /// EVERY file it finds, whether it parses or not: a well-formed envelope is moved with its
    /// own `message_id` and carries its parsed [`MessageRecord`] in [`DrainedMessage::record`];
    /// a malformed one is moved just the same, with a receipt keyed on its filename (there is
    /// no `message_id` to key on) and `record: None`. A caller that only cares about the
    /// well-formed case can simply filter on `record.is_some()`.
    pub fn drain(&self) -> Result<Vec<DrainedMessage>, CoordWriteError> {
        let now = (self.now_iso)();
        let queue_dir = self
            .lock_dir
            .join("queue")
            .join(&self.repo)
            .join(&self.lane);
        let inbox_dir = queue_dir.join("inbox");
        let processing_dir = queue_dir.join("processing");

        let mut files: Vec<PathBuf> = match fs::read_dir(&inbox_dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .collect(),
            // Nothing has ever been sent to this lane yet — an empty drain, not an error,
            // mirroring `write::drain`'s own contract.
            Err(_) => return Ok(Vec::new()),
        };
        files.sort();

        fs::create_dir_all(&processing_dir).map_err(|e| CoordWriteError::Io {
            path: processing_dir.clone(),
            source: e,
        })?;

        let mut drained = Vec::new();
        for path in files {
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                // Another drainer already won the race for this file.
                Err(_) => continue,
            };
            let record = serde_json::from_str::<Message>(&text)
                .ok()
                .and_then(|m| m.typed().cloned());
            let filename = path
                .file_name()
                .expect("path built from a dir entry")
                .to_owned();
            // A well-formed envelope quarantines under its own `message_id`; a malformed file
            // has none, so its filename stands in — still unique, still enough for the receipt
            // ledger to name exactly which file moved.
            let message_id = record
                .as_ref()
                .map(|r| r.message_id.clone())
                .unwrap_or_else(|| filename.to_string_lossy().into_owned());

            let dest = processing_dir.join(&filename);
            if fs::rename(&path, &dest).is_err() {
                // Race lost against another drainer — leave it be, same as above.
                continue;
            }
            append_receipt(&queue_dir, &message_id, "inbox", "processing", &now)?;
            drained.push(DrainedMessage { message_id, record });
        }
        Ok(drained)
    }

    /// Reply to a RENDEZVOUS with a RENDEZVOUS of our own, addressed at the ORIGINAL sender's
    /// own `repo`/`lane` inbox — resolved from `received.sender`, never guessed or hardcoded.
    /// `EN.15.D` task 2.
    pub fn reply_rendezvous(
        &self,
        received: &MessageRecord,
        body: impl Into<String>,
    ) -> Result<PathBuf, CoordWriteError> {
        let now = (self.now_iso)();
        let envelope = serde_json::json!({
            "message_id": uuid_v4_string(),
            "sender": {
                "agent_name": self.agent,
                "repo": self.repo,
                "lane": self.lane,
                "roadmap": received.sender.roadmap,
            },
            "sent_at": now,
            "kind": "RENDEZVOUS",
            "subject": {
                "repo": received.sender.repo,
                "block": received.subject.block,
            },
            "body": body.into(),
            "durable_home": {
                "channel": "lane-log",
                "ref": format!("reply to message {}", received.message_id),
            },
            "verified_by": "engine-rs coord_lane::reply_rendezvous",
        });
        write::send(
            &self.lock_dir,
            &received.sender.repo,
            &received.sender.lane,
            envelope,
            None,
        )
    }
}

/// One inbox file this handle's [`CoordHandle::drain`] moved into `processing/`, whether it
/// parsed as a valid envelope or not — see that method's doc for why a malformed file is
/// quarantined rather than skipped.
#[derive(Debug, Clone)]
pub struct DrainedMessage {
    /// The `message_id` this file's receipt was keyed on — the envelope's own id when
    /// [`record`](DrainedMessage::record) is `Some`, or a filename-derived fallback otherwise.
    pub message_id: String,
    /// The strict typed envelope, when this file parsed as one. `None` for a malformed file —
    /// quarantined exactly the same way, just with nothing left to interpret.
    pub record: Option<MessageRecord>,
}

/// A fresh v4 UUID string for an outgoing reply's `message_id`. Not a wall-clock timestamp, so
/// this needs no clock-seam treatment — every call produces a fresh, unique id regardless of
/// when it runs, and nothing downstream compares it against a staleness window.
fn uuid_v4_string() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Append one transition receipt to `<queue_dir>/receipts.jsonl` — `{message_id, from, to,
/// ts}`, one JSON object per line. Byte-for-byte the same shape `crate::coord::write`'s own
/// (private) `append_receipt` writes to the SAME file, so a receipt this method appends is
/// indistinguishable, to `check_messages.py` or a sibling reader, from one `write::drain`
/// itself would have written. Duplicated rather than called into: `write::write`'s helper is
/// module-private and this task's file scope is `coord_lane.rs`/`integrate.rs` only.
fn append_receipt(
    queue_dir: &Path,
    message_id: &str,
    from: &str,
    to: &str,
    now_iso: &str,
) -> Result<(), CoordWriteError> {
    let receipts_path = queue_dir.join("receipts.jsonl");
    fs::create_dir_all(queue_dir).map_err(|e| CoordWriteError::Io {
        path: receipts_path.clone(),
        source: e,
    })?;
    let receipt = serde_json::json!({
        "message_id": message_id,
        "from": from,
        "to": to,
        "ts": now_iso,
    });
    let line = format!(
        "{}\n",
        serde_json::to_string(&receipt).map_err(|e| CoordWriteError::Invalid {
            path: receipts_path.clone(),
            reason: format!("receipt failed to serialize: {e}"),
        })?
    );
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&receipts_path)
        .map_err(|e| CoordWriteError::Io {
            path: receipts_path.clone(),
            source: e,
        })?;
    file.write_all(line.as_bytes())
        .map_err(|e| CoordWriteError::Io {
            path: receipts_path,
            source: e,
        })
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

    fn inbox_dir_for(h: &CoordHandle) -> PathBuf {
        h.lock_dir
            .join("queue")
            .join(&h.repo)
            .join(&h.lane)
            .join("inbox")
    }

    fn well_formed_envelope(message_id: &str, kind: &str, sent_at: &str) -> serde_json::Value {
        serde_json::json!({
            "message_id": message_id,
            "sender": {
                "agent_name": "base-template-4c",
                "repo": "base-template",
                "lane": "types",
                "roadmap": "coordination-layer-port",
            },
            "sent_at": sent_at,
            "kind": kind,
            "subject": {
                "repo": "engine-rs",
                "block": "EN.15.D",
            },
            "body": "test envelope",
            "durable_home": {
                "channel": "lane-log",
                "ref": "lane-log.jsonl#1",
            },
            "verified_by": "test fixture",
        })
    }

    /// A well-formed envelope drains with its own `message_id` and a parsed [`MessageRecord`],
    /// and lands in `processing/` with exactly one `inbox->processing` receipt.
    #[test]
    fn drain_moves_a_well_formed_message_and_returns_its_parsed_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let inbox = inbox_dir_for(&h);
        std::fs::create_dir_all(&inbox).unwrap();
        let envelope = well_formed_envelope(
            "70ef6ce8-abcd-4e21-9f10-0000000000aa",
            "RENDEZVOUS",
            "2026-09-08T00:00:00Z",
        );
        std::fs::write(
            inbox.join("20260908T000000Z-70ef6ce8-abcd-4e21-9f10-0000000000aa.json"),
            serde_json::to_string(&envelope).unwrap(),
        )
        .unwrap();

        let drained = h.drain().expect("drain");
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].message_id,
            "70ef6ce8-abcd-4e21-9f10-0000000000aa"
        );
        let record = drained[0].record.as_ref().expect("must have parsed");
        assert_eq!(record.kind, okf_core::MessageKind::Rendezvous);

        let processing = h
            .lock_dir
            .join("queue")
            .join(&h.repo)
            .join(&h.lane)
            .join("processing");
        assert_eq!(std::fs::read_dir(&processing).unwrap().count(), 1);
        let receipts = std::fs::read_to_string(
            h.lock_dir
                .join("queue")
                .join(&h.repo)
                .join(&h.lane)
                .join("receipts.jsonl"),
        )
        .unwrap();
        assert_eq!(receipts.lines().count(), 1);
        assert!(receipts.contains("\"from\":\"inbox\""));
        assert!(receipts.contains("\"to\":\"processing\""));
    }

    /// A malformed inbox file (not even valid JSON) is quarantined into `processing/` WITH a
    /// receipt, never silently left in `inbox/` — the acceptance criterion this task adds,
    /// proven as a runtime inversion: this same assertion FAILS against `write::drain` (which
    /// leaves the file in `inbox/` untouched), and PASSES against `CoordHandle::drain`. A
    /// well-formed file sent alongside it still round-trips normally — the positive control.
    #[test]
    fn drain_quarantines_a_malformed_file_with_a_receipt_and_still_drains_a_good_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let inbox = inbox_dir_for(&h);
        std::fs::create_dir_all(&inbox).unwrap();

        // Malformed: not valid JSON at all.
        std::fs::write(inbox.join("20260908T000001Z-bad.json"), b"{ not json").unwrap();

        // Well-formed, sent alongside the malformed file.
        let good = well_formed_envelope(
            "8b1e0e5a-1111-4e21-9f10-0000000000bb",
            "LEASE_RELEASE",
            "2026-09-08T00:00:02Z",
        );
        std::fs::write(
            inbox.join("20260908T000002Z-8b1e0e5a-1111-4e21-9f10-0000000000bb.json"),
            serde_json::to_string(&good).unwrap(),
        )
        .unwrap();

        let drained = h.drain().expect("drain");
        assert_eq!(drained.len(), 2, "both files must be quarantined");

        let processing = h
            .lock_dir
            .join("queue")
            .join(&h.repo)
            .join(&h.lane)
            .join("processing");
        assert!(!inbox.join("20260908T000001Z-bad.json").exists());
        assert!(processing.join("20260908T000001Z-bad.json").exists());
        assert_eq!(
            std::fs::read_dir(&processing).unwrap().count(),
            2,
            "both the malformed and the well-formed file must have moved"
        );

        let receipts = std::fs::read_to_string(
            h.lock_dir
                .join("queue")
                .join(&h.repo)
                .join(&h.lane)
                .join("receipts.jsonl"),
        )
        .unwrap();
        assert_eq!(receipts.lines().count(), 2, "one receipt per file moved");

        let malformed = drained
            .iter()
            .find(|d| d.record.is_none())
            .expect("one entry must be the malformed file");
        assert_eq!(malformed.message_id, "20260908T000001Z-bad.json");

        let well_formed = drained
            .iter()
            .find(|d| d.record.is_some())
            .expect("one entry must be the well-formed file");
        assert_eq!(
            well_formed.message_id,
            "8b1e0e5a-1111-4e21-9f10-0000000000bb"
        );
        assert_eq!(
            well_formed.record.as_ref().unwrap().kind,
            okf_core::MessageKind::LeaseRelease
        );
    }

    /// `reply_rendezvous` addresses its reply at the ORIGINAL sender's own `repo`/`lane`
    /// inbox, resolved from the received envelope's own `sender` field.
    #[test]
    fn reply_rendezvous_lands_in_the_original_senders_inbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let h = handle(dir.path().to_path_buf());
        let received: MessageRecord = serde_json::from_value(well_formed_envelope(
            "9a111111-2222-4e21-9f10-0000000000cc",
            "RENDEZVOUS",
            "2026-09-08T00:00:03Z",
        ))
        .expect("fixture must parse");

        h.reply_rendezvous(&received, "engine-rs-1 answered")
            .expect("reply must send");

        let reply_inbox = dir
            .path()
            .join("queue")
            .join(&received.sender.repo)
            .join(&received.sender.lane)
            .join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&reply_inbox)
            .expect("reply inbox must exist")
            .collect();
        assert_eq!(entries.len(), 1, "exactly one reply envelope written");
    }
}
