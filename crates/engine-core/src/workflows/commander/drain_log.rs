//! Commander drain-log append that never skips, and the heartbeat in the pinned epoch format
//! (`EN.15.F` task 3).
//!
//! Mirrors `scripts/drain_log.py`'s `cmd_record`: read the drain-log's existing lines, build a
//! dedup index from them, then append exactly one `record: "drain"` summary row plus every
//! not-yet-mirrored `record: "receipt"`/`record: "message"` row discovered under the lock
//! dir's queues — a single `read -> index -> append("a")` pass, never a read-modify-write of
//! prior content, so two concurrent invocations interleave lines rather than losing them.
//!
//! ## DEDUP KEYS, pinned from `scripts/drain_log.py:158-179`
//!
//! A **receipt** key is the 6-tuple `(repo, lane, message_id, from, to, ts)`. A **message**
//! key is the 3-tuple `(repo, lane, message_id)`. Neither key includes `roadmap` — the Python
//! never put it there, because the roadmap was implicit in which file the row landed in. A
//! line that fails to parse as JSON is SKIPPED for indexing purposes only: it is reported
//! (via `tracing::warn!`, never a panic) and kept, verbatim, in the file — it never raises and
//! it never causes the log to be rewritten.
//!
//! ## THE DRAIN LOG NEVER SKIPS
//!
//! [`append_drain_log`] always appends exactly one drain-summary row, even when the caller
//! could not resolve which roadmap this pass belongs to — [`DrainSummary::roadmap`] is an
//! `Option<String>` that serializes to a literal JSON `null` rather than being omitted. This
//! is a deliberate extension over `drain_log.py` (whose `--roadmap` is a required CLI
//! argument, because the Python always writes into that roadmap's own directory): the Rust
//! port must be callable from a lane that cannot yet name its roadmap, and "wrote nothing" and
//! "nothing happened" must stay distinguishable afterwards, which requires writing a row.
//!
//! ## The heartbeat, in the pinned bare-epoch-seconds format
//!
//! [`stamp_commander_heartbeat`] writes `<lock_dir>/commander-heartbeats/<name>.heartbeat` as
//! a bare integer via `okf_core::coord::heartbeat::HeartbeatValue::Epoch` and
//! `crate::coord::write::write_heartbeat_file` — VERIFIED against the live fleet file
//! (`.fleet-locks/commander-heartbeats/brain-commander.heartbeat` contains `1788889819`, an
//! integer, no ISO string, no quotes). `base-template/scripts/commander_drain.sh` stays and
//! may stamp the same file from a hand-woken drain; last-writer-wins by design, so this
//! function does nothing to coordinate with a concurrent writer beyond the atomic file write
//! `write_heartbeat_file` already performs.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use okf_core::HeartbeatValue;

use crate::coord::write::{write_heartbeat_file, CoordWriteError};
use crate::workflows::commander::drain::discover_queues;

/// A receipt dedup key — pinned 6-tuple `(repo, lane, message_id, from, to, ts)`.
pub type ReceiptKey = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A message dedup key — pinned 3-tuple `(repo, lane, message_id)`.
pub type MessageKey = (Option<String>, Option<String>, Option<String>);

/// One drain pass's summary counts — the `record: "drain"` row. `roadmap` is `None` when the
/// caller could not resolve a roadmap for the lane this pass drained; per THE DRAIN LOG NEVER
/// SKIPS, that still produces a row (`"roadmap": null`), never a skipped append.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DrainSummary {
    /// The roadmap this pass belongs to, or `None` when it could not be resolved.
    pub roadmap: Option<String>,
    pub drained: u64,
    pub routed: u64,
    pub completed: u64,
    pub manifest_paths: u64,
    pub orphan_inbox: u64,
    pub orphan_processing: u64,
    pub orphan_receipts: u64,
}

/// How many previously-unmirrored receipt/message rows [`append_drain_log`] added, beyond the
/// one drain-summary row it always appends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppendOutcome {
    pub receipts_added: u64,
    pub messages_added: u64,
}

/// Read `path`'s non-blank lines, trailing newline stripped, exactly like
/// `drain_log.py::_load_existing_lines`. A missing file is an empty log, not an error. A line
/// that fails to parse as JSON is reported (`tracing::warn!`) but kept verbatim in the
/// returned list — this function never raises and the caller never rewrites the file from it.
pub fn load_existing_lines(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let raw = raw.trim_end_matches('\r');
        if raw.trim().is_empty() {
            continue;
        }
        if serde_json::from_str::<Value>(raw).is_err() {
            warn!(
                path = %path.display(),
                line = idx + 1,
                "drain_log: skipping malformed line for indexing (kept verbatim, never rewritten)"
            );
        }
        lines.push(raw.to_string());
    }
    lines
}

/// Build the `(receipt_keys, message_keys)` dedup index from a drain log's existing lines,
/// exactly like `drain_log.py::_build_dedup_index`. A line that fails to parse, or parses but
/// isn't a JSON object, or isn't `record: "receipt"`/`record: "message"`, is silently skipped
/// for indexing — it was already reported (if malformed) by [`load_existing_lines`], and this
/// function never raises and never causes the log to be rewritten.
#[must_use]
pub fn build_dedup_index(existing_lines: &[String]) -> (HashSet<ReceiptKey>, HashSet<MessageKey>) {
    let mut receipt_keys = HashSet::new();
    let mut message_keys = HashSet::new();
    for raw in existing_lines {
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let get_str = |k: &str| rec.get(k).and_then(Value::as_str).map(str::to_string);
        match rec.get("record").and_then(Value::as_str) {
            Some("receipt") => {
                receipt_keys.insert((
                    get_str("repo"),
                    get_str("lane"),
                    get_str("message_id"),
                    get_str("from"),
                    get_str("to"),
                    get_str("ts"),
                ));
            }
            Some("message") => {
                message_keys.insert((get_str("repo"), get_str("lane"), get_str("message_id")));
            }
            _ => {}
        }
    }
    (receipt_keys, message_keys)
}

/// Parse `queue_dir/receipts.jsonl` into a list of JSON objects, exactly like
/// `drain_log.py::load_receipts`. A malformed line is reported (`tracing::warn!`) and skipped
/// — never raised, never included in the returned list.
fn load_receipts(queue_dir: &Path) -> Vec<Value> {
    let receipts_path = queue_dir.join("receipts.jsonl");
    let Ok(text) = fs::read_to_string(&receipts_path) else {
        return Vec::new();
    };
    let mut receipts = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(raw) {
            Ok(value) => receipts.push(value),
            Err(e) => warn!(
                path = %receipts_path.display(),
                line = idx + 1,
                error = %e,
                "drain_log: skipping malformed receipt line"
            ),
        }
    }
    receipts
}

/// Return every `.json` file currently sitting in `queue_dir/done/`, sorted by filename —
/// exactly like `drain_log.py::discover_done_messages`.
fn discover_done_messages(queue_dir: &Path) -> Vec<std::path::PathBuf> {
    let done_dir = queue_dir.join("done");
    let Ok(entries) = fs::read_dir(&done_dir) else {
        return Vec::new();
    };
    let mut found: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    found.sort();
    found
}

/// Append one drain pass to the drain-log at `path`: exactly one `record: "drain"` row —
/// ALWAYS, per THE DRAIN LOG NEVER SKIPS, `roadmap: null` when [`DrainSummary::roadmap`] is
/// `None` rather than the row being omitted — followed by one `record: "receipt"` row per
/// `receipts.jsonl` entry and one `record: "message"` row per `done/` message file discovered
/// under every queue `discover_queues(lock_dir)` finds, skipping any already mirrored by dedup
/// key. Creates `path`'s parent directory if needed. A single `read -> index -> append("a")`
/// pass: prior content in `path` is never read back for rewriting, only for the dedup index.
pub fn append_drain_log(
    path: &Path,
    lock_dir: &Path,
    summary: &DrainSummary,
    now_iso: &str,
) -> io::Result<AppendOutcome> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing_lines = load_existing_lines(path);
    let (mut receipt_keys, mut message_keys) = build_dedup_index(&existing_lines);

    let mut new_lines: Vec<String> = Vec::new();

    // 1. Exactly one drain row, ALWAYS — `roadmap: null` when unresolved, never omitted.
    let drain_row = serde_json::json!({
        "record": "drain",
        "ts": now_iso,
        "roadmap": summary.roadmap,
        "drained": summary.drained,
        "routed": summary.routed,
        "completed": summary.completed,
        "manifest_paths": summary.manifest_paths,
        "orphan_inbox": summary.orphan_inbox,
        "orphan_processing": summary.orphan_processing,
        "orphan_receipts": summary.orphan_receipts,
    });
    new_lines.push(drain_row.to_string());

    let mut outcome = AppendOutcome::default();

    for (repo, lane) in discover_queues(lock_dir) {
        let queue_dir = lock_dir.join("queue").join(&repo).join(&lane);

        // 2. Un-mirrored receipts.
        for receipt in load_receipts(&queue_dir) {
            let get_str = |k: &str| receipt.get(k).and_then(Value::as_str).map(str::to_string);
            let key: ReceiptKey = (
                Some(repo.clone()),
                Some(lane.clone()),
                get_str("message_id"),
                get_str("from"),
                get_str("to"),
                get_str("ts"),
            );
            if receipt_keys.contains(&key) {
                continue;
            }
            receipt_keys.insert(key);
            let mut record = serde_json::json!({
                "record": "receipt",
                "repo": repo,
                "lane": lane,
            });
            if let (Value::Object(record_obj), Value::Object(receipt_obj)) = (&mut record, &receipt)
            {
                for (k, v) in receipt_obj {
                    record_obj.insert(k.clone(), v.clone());
                }
            }
            new_lines.push(record.to_string());
            outcome.receipts_added += 1;
        }

        // 3. Un-mirrored done/ messages, full body copied so the record survives deletion.
        for done_path in discover_done_messages(&queue_dir) {
            let body = match fs::read_to_string(&done_path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            {
                Some(body) => body,
                None => {
                    warn!(
                        path = %done_path.display(),
                        "drain_log: skipping unreadable/malformed done/ message"
                    );
                    continue;
                }
            };
            let message_id = body
                .get("message_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let key: MessageKey = (Some(repo.clone()), Some(lane.clone()), message_id.clone());
            if message_keys.contains(&key) {
                continue;
            }
            message_keys.insert(key);
            let filename = done_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let record = serde_json::json!({
                "record": "message",
                "repo": repo,
                "lane": lane,
                "message_id": message_id,
                "filename": filename,
                "message": body,
            });
            new_lines.push(record.to_string());
            outcome.messages_added += 1;
        }
    }

    // Single open(..., append), one write() per line — no read-modify-write of prior content,
    // so two concurrent invocations interleave lines rather than losing them.
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for line in &new_lines {
        writeln!(file, "{line}")?;
    }

    Ok(outcome)
}

/// Stamp `<lock_dir>/commander-heartbeats/<name>.heartbeat` with `now_epoch` as a bare
/// integer — the pinned live-fleet format, never an ISO string, never quoted.
pub fn stamp_commander_heartbeat(
    lock_dir: &Path,
    name: &str,
    now_epoch: i64,
) -> Result<(), CoordWriteError> {
    let path = lock_dir
        .join("commander-heartbeats")
        .join(format!("{name}.heartbeat"));
    write_heartbeat_file(lock_dir, &path, &HeartbeatValue::Epoch(now_epoch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn read_lines(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .expect("drain log must exist")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("every appended line must parse as JSON"))
            .collect()
    }

    // --- dedup keys: pinned tuples ------------------------------------------------------

    #[test]
    fn receipt_dedup_key_is_the_pinned_six_tuple() {
        let lines = vec![serde_json::json!({
            "record": "receipt",
            "repo": "engine-rs",
            "lane": "lane-a",
            "message_id": "m1",
            "from": "inbox",
            "to": "processing",
            "ts": "2026-09-08T00:00:00Z",
        })
        .to_string()];
        let (receipt_keys, message_keys) = build_dedup_index(&lines);
        assert!(message_keys.is_empty());
        assert_eq!(receipt_keys.len(), 1);
        let key = receipt_keys.iter().next().unwrap();
        assert_eq!(
            key,
            &(
                Some("engine-rs".to_string()),
                Some("lane-a".to_string()),
                Some("m1".to_string()),
                Some("inbox".to_string()),
                Some("processing".to_string()),
                Some("2026-09-08T00:00:00Z".to_string()),
            )
        );
    }

    #[test]
    fn message_dedup_key_is_the_pinned_three_tuple() {
        let lines = vec![serde_json::json!({
            "record": "message",
            "repo": "engine-rs",
            "lane": "lane-a",
            "message_id": "m1",
            "filename": "m1.json",
            "message": {"kind": "QUERY"},
        })
        .to_string()];
        let (receipt_keys, message_keys) = build_dedup_index(&lines);
        assert!(receipt_keys.is_empty());
        assert_eq!(
            message_keys,
            HashSet::from([(
                Some("engine-rs".to_string()),
                Some("lane-a".to_string()),
                Some("m1".to_string()),
            )])
        );
    }

    // --- a malformed line is skipped for indexing, never rewritten ----------------------

    #[test]
    fn a_malformed_line_is_skipped_for_indexing_and_kept_verbatim_never_rewritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drain-log.jsonl");
        std::fs::write(
            &path,
            "{ this is not valid json\n{\"record\": \"drain\", \"ts\": \"t0\", \"roadmap\": \"r\", \"drained\": 0, \"routed\": 0, \"completed\": 0, \"manifest_paths\": 0, \"orphan_inbox\": 0, \"orphan_processing\": 0, \"orphan_receipts\": 0}\n",
        )
        .expect("seed malformed log");

        let existing = load_existing_lines(&path);
        // The malformed line is kept, verbatim, as a raw line -- never dropped, never raises.
        assert_eq!(existing.len(), 2);
        assert_eq!(existing[0], "{ this is not valid json");

        let (receipt_keys, message_keys) = build_dedup_index(&existing);
        assert!(receipt_keys.is_empty());
        assert!(message_keys.is_empty());

        let lock_dir = dir.path().join(".fleet-locks");
        let summary = DrainSummary {
            roadmap: Some("r".to_string()),
            ..Default::default()
        };
        append_drain_log(&path, &lock_dir, &summary, "t1").expect("append must not rewrite");

        // The file grew by exactly one line (the new drain row); the malformed line and the
        // prior valid drain row are both still there, untouched.
        let text = fs::read_to_string(&path).expect("read back");
        assert_eq!(text.lines().count(), 3);
        assert_eq!(text.lines().next().unwrap(), "{ this is not valid json");
    }

    // --- THE DRAIN LOG NEVER SKIPS: no roadmap resolved still writes a row --------------

    #[test]
    fn a_pass_with_no_resolved_roadmap_writes_a_roadmap_null_row_never_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("roadmaps")
            .join("_unresolved")
            .join("drain-log.jsonl");
        let lock_dir = dir.path().join(".fleet-locks");

        let summary = DrainSummary {
            roadmap: None,
            drained: 0,
            ..Default::default()
        };
        let outcome =
            append_drain_log(&path, &lock_dir, &summary, "2026-09-08T00:00:00Z").expect("append");
        assert_eq!(outcome, AppendOutcome::default());

        let lines = read_lines(&path);
        assert_eq!(
            lines.len(),
            1,
            "a row must be written even with no roadmap resolved"
        );
        assert_eq!(lines[0]["record"], "drain");
        assert!(
            lines[0]["roadmap"].is_null(),
            "unresolved roadmap must serialize as JSON null, not be omitted: {:?}",
            lines[0]
        );
    }

    #[test]
    fn a_pass_with_a_resolved_roadmap_writes_it_as_a_string() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drain-log.jsonl");
        let lock_dir = dir.path().join(".fleet-locks");

        let summary = DrainSummary {
            roadmap: Some("coordination-layer-port".to_string()),
            drained: 2,
            routed: 2,
            completed: 2,
            ..Default::default()
        };
        append_drain_log(&path, &lock_dir, &summary, "2026-09-08T00:00:00Z").expect("append");

        let lines = read_lines(&path);
        assert_eq!(lines[0]["roadmap"], "coordination-layer-port");
        assert_eq!(lines[0]["drained"], 2);
    }

    // --- receipts + done/ messages get mirrored, and dedup holds across two passes ------

    fn write_receipt(
        lock_dir: &Path,
        repo: &str,
        lane: &str,
        message_id: &str,
        from: &str,
        to: &str,
        ts: &str,
    ) {
        let queue_dir = lock_dir.join("queue").join(repo).join(lane);
        fs::create_dir_all(&queue_dir).expect("mkdir queue dir");
        let line = serde_json::json!({
            "message_id": message_id, "from": from, "to": to, "ts": ts,
        })
        .to_string();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(queue_dir.join("receipts.jsonl"))
            .expect("open receipts.jsonl");
        writeln!(file, "{line}").expect("write receipt");
    }

    fn write_done_message(lock_dir: &Path, repo: &str, lane: &str, message_id: &str) -> PathBuf {
        let done_dir = lock_dir.join("queue").join(repo).join(lane).join("done");
        fs::create_dir_all(&done_dir).expect("mkdir done dir");
        let path = done_dir.join(format!("{message_id}.json"));
        fs::write(
            &path,
            serde_json::json!({"message_id": message_id, "kind": "QUERY"}).to_string(),
        )
        .expect("write done message");
        path
    }

    #[test]
    fn receipts_and_done_messages_are_mirrored_and_deduped_across_two_passes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("drain-log.jsonl");
        let lock_dir = dir.path().join(".fleet-locks");

        write_receipt(
            &lock_dir,
            "engine-rs",
            "lane-a",
            "m1",
            "inbox",
            "processing",
            "t0",
        );
        write_done_message(&lock_dir, "engine-rs", "lane-a", "m1");

        let summary = DrainSummary {
            roadmap: Some("r".to_string()),
            drained: 1,
            ..Default::default()
        };
        let first = append_drain_log(&path, &lock_dir, &summary, "ts-1").expect("first append");
        assert_eq!(first.receipts_added, 1);
        assert_eq!(first.messages_added, 1);

        let lines_after_first = read_lines(&path);
        // drain row + receipt row + message row.
        assert_eq!(lines_after_first.len(), 3);

        // A second pass over the SAME unchanged tree must not re-mirror anything, even though
        // it still writes its own drain row (never skips the summary).
        let second = append_drain_log(&path, &lock_dir, &summary, "ts-2").expect("second append");
        assert_eq!(second.receipts_added, 0);
        assert_eq!(second.messages_added, 0);

        let lines_after_second = read_lines(&path);
        assert_eq!(
            lines_after_second.len(),
            4,
            "second pass adds exactly one more row (its own drain summary), no re-mirrored receipt/message"
        );
    }

    // --- heartbeat: bare epoch seconds, never ISO, never quoted -------------------------

    #[test]
    fn heartbeat_file_contains_bare_epoch_seconds_not_an_iso_timestamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path().join(".fleet-locks");
        fs::create_dir_all(&lock_dir).expect("mkdir lock dir");

        stamp_commander_heartbeat(&lock_dir, "engine-rs-commander", 1_788_889_819)
            .expect("stamp heartbeat");

        let raw = fs::read_to_string(
            lock_dir
                .join("commander-heartbeats")
                .join("engine-rs-commander.heartbeat"),
        )
        .expect("read heartbeat file");
        assert_eq!(raw, "1788889819");
        assert!(raw.parse::<i64>().is_ok(), "must be a bare integer: {raw}");
        assert!(!raw.contains('"'), "must never be quoted: {raw}");
        assert!(!raw.contains('T'), "must never be an ISO timestamp: {raw}");
    }

    #[test]
    fn restamping_the_heartbeat_overwrites_last_writer_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path().join(".fleet-locks");
        fs::create_dir_all(&lock_dir).expect("mkdir lock dir");

        stamp_commander_heartbeat(&lock_dir, "brain-commander", 100).expect("first stamp");
        stamp_commander_heartbeat(&lock_dir, "brain-commander", 200).expect("second stamp");

        let raw = fs::read_to_string(
            lock_dir
                .join("commander-heartbeats")
                .join("brain-commander.heartbeat"),
        )
        .expect("read heartbeat file");
        assert_eq!(raw, "200", "last writer wins by design");
    }
}
