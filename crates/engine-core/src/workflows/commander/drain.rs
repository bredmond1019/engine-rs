//! Commander inbox drain: discover every queue, route by `kind`, complete with receipts
//! (`EN.15.F` task 1).
//!
//! Mirrors `scripts/drain_log.py`'s `discover_queues` (which reuses
//! `base-template/scripts/check_messages.py`'s own function of the same name, :356): every
//! `<lock_dir>/queue/<repo>/<lane>/` directory that exists on disk is a queue to drain, full
//! stop — **not only the running lane's own inbox**. THIS IS THE BLOCK'S CENTRAL SCAR: the
//! Python commander drained only its own inbox and reported "drained 0" for THIRTEEN
//! consecutive drains while three messages, one a P0, sat unread among them.
//!
//! Draining itself reuses [`crate::coord::write::drain`] (inbox -> processing, one receipt per
//! file moved) and [`crate::coord::write::complete`] (processing -> done, one more receipt) —
//! this module never reimplements the move-and-receipt dance, it only adds the routing step
//! between them: read each moved message's `kind`, look up its priority, and complete every
//! message regardless of what that priority is (task 1 files nothing per-kind beyond the
//! priority annotation — the one judgement step this workflow gains, orphan triage, is a later
//! task's `GatedAction::RunDrain`-gated `ClaudeCodeStep`).
//!
//! ## The priority lookup
//!
//! Brain D43 is the fleet's sole priority authority; a message envelope itself is forbidden
//! from carrying a sender-declared `priority` or `urgency` field (`message.schema.json`,
//! enforced by `check_messages.py`'s `FORBIDDEN_KEYS`) precisely so a second, sender-controlled
//! rubric cannot fork alongside it. [`message_kind_priority`] is that lookup applied to the one
//! signal a message DOES carry — its `kind` — using the same lower-is-hotter P0..P3 buckets
//! D43 defines, and reproducing the interrupt discipline the `ping-agent` skill's Rule 3 already
//! settled: `RENDEZVOUS` and `LEASE_RELEASE` are objectively time-critical (both concern the
//! tree), everything else is routine. [`drain_lane`] completes higher-priority messages first
//! within a single drain pass — a stable sort, so same-priority messages keep the order they
//! were moved into `processing/` in (filename order, i.e. arrival order).
//!
//! ## RE-DERIVES, NEVER DETECTS
//!
//! Every function in this file re-reads the tree from scratch on each call. No cursor, no
//! cache, no "last seen" marker is introduced anywhere here — that is exactly the property
//! that makes running the same drain twice over an unchanged tree a safe no-op rather than a
//! silent race the second time two drains overlap.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use okf_core::MessageKind;

use crate::coord::write::{self, CoordWriteError};

/// One message this drain routed and completed, annotated with its looked-up priority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedMessage {
    /// The uuid half of the message's filename stem.
    pub message_id: String,
    /// The raw `kind` string read back from the moved envelope, when it could be read and
    /// parsed at all. `None` covers every failure mode uniformly (file vanished out from under
    /// a concurrent drainer, JSON did not parse, `kind` field absent or not a string) — a
    /// message this degraded still gets [`DEFAULT_PRIORITY`] rather than aborting the drain
    /// over one bad envelope.
    pub kind: Option<String>,
    /// The looked-up priority, `0` (hottest) to `3` (coldest) — see [`message_kind_priority`].
    pub priority: u8,
    /// Whether `complete` actually moved this message to `done/` and wrote its receipt.
    /// `false` only when another drainer already won the race for this exact message between
    /// this drain's `drain()` call and its own `complete()` call for it — never an error.
    pub completed: bool,
}

/// One queue's drain-and-route result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneDrainResult {
    /// Repo slug this queue belongs to.
    pub repo: String,
    /// Lane slug (the queue's owning lane) this queue belongs to.
    pub lane: String,
    /// Every message drained from this lane's inbox, in the order they were completed
    /// (priority order, ties broken by arrival order).
    pub routed: Vec<RoutedMessage>,
}

impl LaneDrainResult {
    /// How many messages this lane's queue actually had drained this pass.
    pub fn drained_count(&self) -> usize {
        self.routed.len()
    }
}

/// The priority [`message_kind_priority`] returns for a `kind` this drain could not read or
/// parse at all — the coldest bucket (P3), never a panic and never a dropped message. An
/// unreadable `kind` is a degraded read, not grounds to refuse completing the message: the
/// message itself already made it to `processing/` via [`crate::coord::write::drain`], and
/// leaving it stranded there would reintroduce exactly the "sat unread" failure this block
/// exists to close.
pub const DEFAULT_PRIORITY: u8 = 3;

/// Look up a [`MessageKind`]'s priority under D43's lower-is-hotter P0..P3 buckets, reproducing
/// the `ping-agent` skill's Rule 3 interrupt discipline: `Rendezvous` and `LeaseRelease` both
/// concern the tree and are objectively time-critical (P0); every other kind is routine and is
/// triaged in `kind` order rather than urgently (`EdgeReleased` P1, `Finding` P2, `Query` P3).
/// Exhaustive over [`MessageKind`] — a sixth kind added there without a line added here is a
/// compile error, not a silent default.
pub fn message_kind_priority(kind: MessageKind) -> u8 {
    match kind {
        MessageKind::Rendezvous => 0,
        MessageKind::LeaseRelease => 0,
        MessageKind::EdgeReleased => 1,
        MessageKind::Finding => 2,
        MessageKind::Query => 3,
    }
}

/// Parse a `kind` string as read raw off disk into a priority, via [`MessageKind`]'s own
/// `SCREAMING_SNAKE_CASE` wire form. An out-of-enum or malformed string never panics and never
/// refuses the message — it falls back to [`DEFAULT_PRIORITY`], exactly like a `kind` this
/// drain could not read at all.
fn priority_for_raw_kind(kind: Option<&str>) -> u8 {
    kind.and_then(|k| {
        let quoted = format!("\"{k}\"");
        serde_json::from_str::<MessageKind>(&quoted).ok()
    })
    .map(message_kind_priority)
    .unwrap_or(DEFAULT_PRIORITY)
}

/// Every `<lock_dir>/queue/<repo>/<lane>/` directory that exists on disk, as `(repo, lane)`
/// pairs — mirroring `scripts/drain_log.py`'s `discover_queues`, which walks the WHOLE `queue/`
/// tree rather than one lane's own subtree. A missing `queue/` directory (nothing has ever been
/// sent fleet-wide) yields an empty list, not an error. Sorted for determinism: repo, then lane.
pub fn discover_queues(lock_dir: &Path) -> Vec<(String, String)> {
    let queue_root = lock_dir.join("queue");
    let mut found = Vec::new();

    let Ok(repo_entries) = fs::read_dir(&queue_root) else {
        return found;
    };
    let mut repo_dirs: Vec<PathBuf> = repo_entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    repo_dirs.sort();

    for repo_dir in repo_dirs {
        let Some(repo) = repo_dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(lane_entries) = fs::read_dir(&repo_dir) else {
            continue;
        };
        let mut lane_dirs: Vec<PathBuf> = lane_entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        lane_dirs.sort();

        for lane_dir in lane_dirs {
            let Some(lane) = lane_dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            found.push((repo.to_string(), lane.to_string()));
        }
    }
    found
}

/// Read the `kind` field back off a message file sitting in `processing_dir` whose filename's
/// `<uuid>` half (`<ts>-<uuid>.json`, split on the FIRST `-`) matches `message_id`. Mirrors the
/// same by-suffix match [`crate::coord::write::complete`] uses to find a message by id. Returns
/// `None` for every failure mode uniformly (no matching file, unreadable, unparseable JSON, or
/// no string `kind` field) — never panics, never propagates an error, since a degraded read
/// here must not block completing the message.
fn read_kind(processing_dir: &Path, message_id: &str) -> Option<String> {
    let entries = fs::read_dir(processing_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(dash) = stem.find('-') else {
            continue;
        };
        if &stem[dash + 1..] != message_id {
            continue;
        }
        let text = fs::read_to_string(&path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        return value
            .get("kind")
            .and_then(|k| k.as_str())
            .map(|s| s.to_string());
    }
    None
}

/// Drain one lane's queue: move every message currently in its `inbox/` to `processing/`
/// (via [`crate::coord::write::drain`]), route each by its `kind`'s D43 priority, then complete
/// every one of them (via [`crate::coord::write::complete`]) in priority order — a stable sort,
/// so messages of equal priority keep the order they arrived in.
///
/// A queue with nothing in `inbox/` yet drains to an empty `routed` list, not an error — the
/// same "empty drain, not a fault" contract [`crate::coord::write::drain`] itself has. This
/// function is safe to call twice in a row over an unchanged tree: the second call finds an
/// empty `inbox/` and returns an empty result, with no cursor consulted or written anywhere to
/// make that true.
pub fn drain_lane(
    lock_dir: &Path,
    repo: &str,
    lane: &str,
    now_iso: &str,
) -> Result<LaneDrainResult, CoordWriteError> {
    let moved = write::drain(lock_dir, repo, lane, now_iso)?;
    if moved.is_empty() {
        return Ok(LaneDrainResult {
            repo: repo.to_string(),
            lane: lane.to_string(),
            routed: Vec::new(),
        });
    }

    let processing_dir = lock_dir
        .join("queue")
        .join(repo)
        .join(lane)
        .join("processing");

    // Route: read each moved message's kind and look up its priority, preserving arrival
    // (filename) order as the tie-break for a stable priority sort next.
    let mut routed: Vec<(String, Option<String>, u8)> = moved
        .into_iter()
        .map(|message_id| {
            let kind = read_kind(&processing_dir, &message_id);
            let priority = priority_for_raw_kind(kind.as_deref());
            (message_id, kind, priority)
        })
        .collect();
    routed.sort_by_key(|(_, _, priority)| *priority);

    let mut results = Vec::with_capacity(routed.len());
    for (message_id, kind, priority) in routed {
        let completed = match write::complete(lock_dir, repo, lane, &message_id, now_iso) {
            Ok(completed) => completed,
            Err(err) => {
                warn!(
                    repo,
                    lane,
                    message_id = message_id.as_str(),
                    error = %err,
                    "commander drain: failed to complete a routed message"
                );
                false
            }
        };
        results.push(RoutedMessage {
            message_id,
            kind,
            priority,
            completed,
        });
    }

    Ok(LaneDrainResult {
        repo: repo.to_string(),
        lane: lane.to_string(),
        routed: results,
    })
}

/// Drain EVERY queue discovered under the lock dir — [`discover_queues`] first, then
/// [`drain_lane`] for each — never only the running lane's own inbox. This is the top-level
/// entry point later tasks in this block wire into the registered `COMMANDER` workflow.
///
/// A per-lane `drain_lane` I/O failure (e.g. a permissions error moving a file) is logged and
/// that lane's result is omitted from the returned list rather than aborting the whole pass —
/// one lane's filesystem trouble must not stop every other lane's mail from being read, which
/// would reproduce this block's central scar by a different mechanism.
pub fn drain_all_queues(lock_dir: &Path, now_iso: &str) -> Vec<LaneDrainResult> {
    let queues = discover_queues(lock_dir);
    info!(
        queue_count = queues.len(),
        "commander drain: discovered queues"
    );

    let mut results = Vec::with_capacity(queues.len());
    for (repo, lane) in queues {
        match drain_lane(lock_dir, &repo, &lane, now_iso) {
            Ok(result) => {
                info!(
                    repo = result.repo.as_str(),
                    lane = result.lane.as_str(),
                    drained = result.drained_count(),
                    "commander drain: lane drained"
                );
                results.push(result);
            }
            Err(err) => {
                warn!(repo, lane, error = %err, "commander drain: failed to drain a lane");
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::write::send;

    fn message_json(message_id: &str, sent_at: &str, kind: &str) -> serde_json::Value {
        serde_json::json!({
            "message_id": message_id,
            "sender": {
                "agent_name": "engine-rs-1",
                "repo": "engine-rs",
                "lane": "engine-rs",
                "roadmap": "coordination-layer-port",
            },
            "sent_at": sent_at,
            "kind": kind,
            "subject": { "repo": "bastion", "block": "BA.21.A" },
            "body": "test body",
            "durable_home": {
                "channel": "state-edge",
                "ref": "bastion/planning/state.json#BA.21.A",
            },
            "verified_by": "UNVERIFIED: engine-rs-1",
        })
    }

    #[test]
    fn discover_queues_finds_every_repo_lane_pair_not_just_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        send(
            dir.path(),
            "bastion",
            "lane-a",
            message_json("id-a", "2026-09-08T10:00:00Z", "QUERY"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "engine-rs",
            "lane-b",
            message_json("id-b", "2026-09-08T10:00:01Z", "FINDING"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "engine-rs",
            "lane-c",
            message_json("id-c", "2026-09-08T10:00:02Z", "RENDEZVOUS"),
            None,
        )
        .expect("send must succeed");

        let mut found = discover_queues(dir.path());
        found.sort();
        assert_eq!(
            found,
            vec![
                ("bastion".to_string(), "lane-a".to_string()),
                ("engine-rs".to_string(), "lane-b".to_string()),
                ("engine-rs".to_string(), "lane-c".to_string()),
            ]
        );
    }

    #[test]
    fn discover_queues_on_an_empty_tree_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(discover_queues(dir.path()).is_empty());
    }

    #[test]
    fn a_tree_with_messages_in_three_lanes_reports_three_drained_not_zero() {
        // THE BLOCK'S CENTRAL SCAR: the Python commander drained only its own inbox and
        // reported "drained 0" for thirteen consecutive drains. This is the regression test.
        let dir = tempfile::tempdir().expect("tempdir");
        send(
            dir.path(),
            "bastion",
            "lane-a",
            message_json("id-a", "2026-09-08T10:00:00Z", "QUERY"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "engine-rs",
            "lane-b",
            message_json("id-b", "2026-09-08T10:00:01Z", "FINDING"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "mev",
            "lane-c",
            message_json("id-c", "2026-09-08T10:00:02Z", "RENDEZVOUS"),
            None,
        )
        .expect("send must succeed");

        let results = drain_all_queues(dir.path(), "2026-09-08T10:05:00Z");
        let total_drained: usize = results.iter().map(LaneDrainResult::drained_count).sum();
        assert_eq!(
            total_drained, 3,
            "a drain against ANY one lane must still see every other lane's mail"
        );
        assert_eq!(results.len(), 3, "three distinct queues must be reported");
    }

    #[test]
    fn each_completed_message_leaves_a_receipt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let message_id = "70ef6ce8-abcd-4e21-9f10-0000000000aa";
        send(
            dir.path(),
            "bastion",
            "bastion-lane",
            message_json(message_id, "2026-09-08T10:00:00Z", "QUERY"),
            None,
        )
        .expect("send must succeed");

        let result = drain_lane(
            dir.path(),
            "bastion",
            "bastion-lane",
            "2026-09-08T10:05:00Z",
        )
        .expect("drain_lane must succeed");
        assert_eq!(result.routed.len(), 1);
        assert!(result.routed[0].completed);

        let receipts_path = dir
            .path()
            .join("queue")
            .join("bastion")
            .join("bastion-lane")
            .join("receipts.jsonl");
        let text = fs::read_to_string(&receipts_path).expect("receipts.jsonl must exist");
        let lines: Vec<&str> = text.lines().collect();
        // One inbox->processing receipt (from `drain`) plus one processing->done receipt
        // (from `complete`) -- exactly two receipts for the one message's two transitions.
        assert_eq!(lines.len(), 2, "each transition must leave its own receipt");
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(last["from"], "processing");
        assert_eq!(last["to"], "done");
        assert_eq!(last["message_id"], message_id);

        let done_dir = dir
            .path()
            .join("queue")
            .join("bastion")
            .join("bastion-lane")
            .join("done");
        let done_files: Vec<_> = fs::read_dir(&done_dir).unwrap().flatten().collect();
        assert_eq!(done_files.len(), 1, "the message must now sit in done/");
    }

    #[test]
    fn running_the_same_drain_twice_over_an_unchanged_tree_is_a_no_op_the_second_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        send(
            dir.path(),
            "bastion",
            "bastion-lane",
            message_json("id-1", "2026-09-08T10:00:00Z", "QUERY"),
            None,
        )
        .expect("send must succeed");

        let first = drain_all_queues(dir.path(), "2026-09-08T10:05:00Z");
        let first_total: usize = first.iter().map(LaneDrainResult::drained_count).sum();
        assert_eq!(first_total, 1);

        let second = drain_all_queues(dir.path(), "2026-09-08T10:06:00Z");
        let second_total: usize = second.iter().map(LaneDrainResult::drained_count).sum();
        assert_eq!(
            second_total, 0,
            "re-deriving from an unchanged tree must be a safe no-op, not a re-drain"
        );
    }

    #[test]
    fn message_kind_priority_matches_the_rule_3_interrupt_discipline() {
        assert_eq!(message_kind_priority(MessageKind::Rendezvous), 0);
        assert_eq!(message_kind_priority(MessageKind::LeaseRelease), 0);
        assert_eq!(message_kind_priority(MessageKind::EdgeReleased), 1);
        assert_eq!(message_kind_priority(MessageKind::Finding), 2);
        assert_eq!(message_kind_priority(MessageKind::Query), 3);
    }

    #[test]
    fn routing_completes_higher_priority_kinds_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Sent in arrival order QUERY, FINDING, RENDEZVOUS -- routing must still complete
        // RENDEZVOUS (P0) before FINDING (P2) before QUERY (P3).
        send(
            dir.path(),
            "bastion",
            "lane-a",
            message_json("id-query", "2026-09-08T10:00:00Z", "QUERY"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "bastion",
            "lane-a",
            message_json("id-finding", "2026-09-08T10:00:01Z", "FINDING"),
            None,
        )
        .expect("send must succeed");
        send(
            dir.path(),
            "bastion",
            "lane-a",
            message_json("id-rendezvous", "2026-09-08T10:00:02Z", "RENDEZVOUS"),
            None,
        )
        .expect("send must succeed");

        let result = drain_lane(dir.path(), "bastion", "lane-a", "2026-09-08T10:05:00Z")
            .expect("drain_lane must succeed");
        let ids: Vec<&str> = result
            .routed
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["id-rendezvous", "id-finding", "id-query"]);
        assert!(result.routed.iter().all(|m| m.completed));
    }

    #[test]
    fn an_unreadable_kind_falls_back_to_the_default_priority_and_still_completes() {
        // A message whose kind is missing/unparseable must not stall the drain -- it still
        // gets completed, just at the coldest priority bucket.
        assert_eq!(priority_for_raw_kind(None), DEFAULT_PRIORITY);
        assert_eq!(
            priority_for_raw_kind(Some("NOT_A_REAL_KIND")),
            DEFAULT_PRIORITY
        );
    }

    #[test]
    fn drain_lane_on_a_lane_with_no_inbox_yet_is_an_empty_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = drain_lane(
            dir.path(),
            "bastion",
            "never-sent-to",
            "2026-09-08T10:05:00Z",
        )
        .expect("draining an unpopulated lane must not error");
        assert!(result.routed.is_empty());
    }
}
