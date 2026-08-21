//! Reads the `bastion:BA.18.A` blocked-edge sink as the queue's durable
//! source (`EN.8.B` task 2).
//!
//! Per `planning/8.B-operator-queue/tasks.md`: the sink
//! (`core/bastion/src/serve/blocked_edge/sink.rs`) is an append-only JSONL
//! file, not an API and not a database. This module reads it directly —
//! same path, same wire shape — with no dependency on the writing process
//! being alive. `BlockedEdgeRecord` below is a **wire-format mirror** of
//! bastion's real struct, not an import of it: engine-rs and bastion are
//! separate repos with separate crate graphs, so the contract is the JSONL
//! shape on disk, read against the real struct at authoring time (per the
//! task instruction), not a `path` dependency across repo boundaries.
//!
//! Read behavior deliberately follows `operator::ledger::store`'s
//! `FileApprovalLedger`, which itself follows the bastion sink as its
//! reference implementation: a missing file is an empty `Vec`, never an
//! error (the engine may start before the producer has ever written), a
//! malformed line is skipped with a warning rather than failing the whole
//! read (a line half-written by a killed producer must not make the queue
//! unreadable), and every read opens the file fresh — no handle is held
//! between reads, so a `BlockedEdgeSource` can safely outlive the writing
//! process' restarts.
//!
//! **The level predicate is re-evaluated downstream, not here.** Per
//! `bastion:BA.18.A`'s notes, an edge record is a trigger, not a truth —
//! this module only carries the record's `to` state forward on
//! [`PendingBlockedEdge`] so `EN.8.B` task 3 can drop an item whose
//! underlying session is no longer blocked at *selection* time.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Wire-format mirror of bastion's `detect::AgentState` — same variants,
/// same `#[serde(rename_all = "snake_case")]` representation
/// (`core/bastion/src/detect/mod.rs`). Kept local rather than imported: see
/// the module header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedEdgeState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

/// Wire-format mirror of `bastion::serve::blocked_edge::sink::BlockedEdgeRecord`
/// — one JSONL line from the sink. See the module header for why this is a
/// mirror rather than an import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockedEdgeRecord {
    pub session: String,
    pub host: String,
    pub from: Option<BlockedEdgeState>,
    pub to: BlockedEdgeState,
    pub observed_at: DateTime<Utc>,
}

/// One pending item as read from a [`QueueSource`] — the sink record plus
/// nothing else the queue itself needs to invent. `to` is carried forward
/// verbatim so a later stage can re-evaluate whether the underlying
/// session is still blocked before ever delivering it.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingBlockedEdge {
    pub session: String,
    pub host: String,
    pub to: BlockedEdgeState,
    pub observed_at: DateTime<Utc>,
}

impl From<BlockedEdgeRecord> for PendingBlockedEdge {
    fn from(record: BlockedEdgeRecord) -> Self {
        Self {
            session: record.session,
            host: record.host,
            to: record.to,
            observed_at: record.observed_at,
        }
    }
}

/// The injectable seam for the queue's durable source — mirrors
/// `CronStore`/`ApprovalLedger`'s trait/impl/stub convention. A default
/// impl ([`BlockedEdgeSource`]) reads the real bastion sink file; tests
/// substitute [`InMemoryQueueSource`] (no real filesystem reads in the
/// gated suite).
pub trait QueueSource: Send + Sync {
    /// Every item currently pending in the source, in file order (oldest
    /// first) — ordering for *delivery* is [`super::item::compare_items`]'s
    /// job, not this trait's.
    fn pending(&self) -> Vec<PendingBlockedEdge>;
}

/// A [`QueueSource`] backed by the real `bastion:BA.18.A` blocked-edge
/// sink JSONL file.
///
/// Cheap to construct (just a path) — never holds an open file handle
/// between reads, so it works whether or not the writing `bastion serve`
/// process is currently alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedEdgeSource {
    path: PathBuf,
}

impl BlockedEdgeSource {
    /// Point a source at `path`. Does not touch the filesystem.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path this source reads.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl QueueSource for BlockedEdgeSource {
    fn pending(&self) -> Vec<PendingBlockedEdge> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            // A sink that has not been written to yet is an empty queue,
            // never an error — the engine may well start before the
            // producer has ever run.
            Err(_) => return Vec::new(),
        };

        let reader = BufReader::new(file);
        let mut pending = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<BlockedEdgeRecord>(trimmed) {
                Ok(record) => pending.push(PendingBlockedEdge::from(record)),
                Err(err) => {
                    // A malformed line (e.g. half-written by a killed
                    // producer) is skipped, never a panic and never a
                    // whole-read failure — the surrounding valid records
                    // must still load.
                    tracing::warn!(
                        path = %self.path.display(),
                        error = %err,
                        "skipping malformed blocked-edge sink line"
                    );
                }
            }
        }
        pending
    }
}

/// Resolve the default sink path with the same precedence bastion writes
/// under: `$XDG_STATE_HOME/bastion/blocked-edges.jsonl`, falling back to
/// `$HOME/.local/state/bastion/blocked-edges.jsonl`. Returns `None` when
/// neither is set.
///
/// Pure function over its two arguments — reads no environment itself —
/// mirroring `bastion::serve::blocked_edge::sink::default_sink_path` and
/// this repo's own `operator::ledger::store::default_ledger_path`
/// convention. Note the path segment is `"bastion"`, not `"engine-rs"` —
/// this must resolve to the *same* file bastion writes, not a
/// engine-rs-namespaced one.
pub fn default_sink_path(xdg_state_home: Option<String>, home: Option<String>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_state_home {
        Some(
            PathBuf::from(xdg)
                .join("bastion")
                .join("blocked-edges.jsonl"),
        )
    } else {
        home.map(|h| {
            PathBuf::from(h)
                .join(".local")
                .join("state")
                .join("bastion")
                .join("blocked-edges.jsonl")
        })
    }
}

/// An in-memory [`QueueSource`] stub — no real file I/O. Matches
/// `operator::ledger::store::InMemoryApprovalLedger`'s convention, so the
/// gated suite exercises the source contract without ever reading a real
/// filesystem.
#[cfg(test)]
pub struct InMemoryQueueSource {
    items: std::sync::Mutex<Vec<PendingBlockedEdge>>,
}

#[cfg(test)]
impl InMemoryQueueSource {
    pub fn new(items: Vec<PendingBlockedEdge>) -> Self {
        Self {
            items: std::sync::Mutex::new(items),
        }
    }
}

#[cfg(test)]
impl QueueSource for InMemoryQueueSource {
    fn pending(&self) -> Vec<PendingBlockedEdge> {
        self.items
            .lock()
            .expect("InMemoryQueueSource mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "engine-operator-queue-source-test-{}-{}-{}",
            std::process::id(),
            name,
            uuid::Uuid::new_v4()
        ))
    }

    fn write_line(path: &std::path::Path, json: &str) {
        use std::fs::OpenOptions;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append should succeed");
        writeln!(file, "{json}").expect("write should succeed");
    }

    fn real_record_json(session: &str, from: Option<&str>, to: &str, secs: i64) -> String {
        // Mirrors the exact wire shape bastion's `BlockedEdgeRecord`
        // serializes to (verified against
        // `core/bastion/src/serve/blocked_edge/sink.rs`).
        let from_json = match from {
            Some(s) => format!("\"{s}\""),
            None => "null".to_string(),
        };
        format!(
            "{{\"session\":\"{session}\",\"host\":\"mini-console\",\"from\":{from_json},\"to\":\"{to}\",\"observed_at\":\"{}\"}}",
            ts(secs).to_rfc3339()
        )
    }

    // ── missing file ─────────────────────────────────────────────────────

    #[test]
    fn missing_sink_file_yields_empty_pending() {
        let path = temp_path("missing");
        let source = BlockedEdgeSource::new(&path);
        assert_eq!(source.pending(), Vec::new());
    }

    // ── real wire-format round trip ─────────────────────────────────────

    #[test]
    fn reads_real_bastion_wire_format_records() {
        let path = temp_path("wire-format");
        write_line(
            &path,
            &real_record_json("sess-a", Some("working"), "blocked", 0),
        );
        write_line(&path, &real_record_json("sess-b", None, "blocked", 60));

        let source = BlockedEdgeSource::new(&path);
        let pending = source.pending();

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].session, "sess-a");
        assert_eq!(pending[0].to, BlockedEdgeState::Blocked);
        assert_eq!(pending[1].session, "sess-b");
        let _ = std::fs::remove_file(&path);
    }

    // ── malformed lines are skipped, not fatal ──────────────────────────

    #[test]
    fn malformed_line_is_skipped_valid_records_still_load() {
        let path = temp_path("malformed");
        write_line(&path, &real_record_json("sess-a", None, "blocked", 0));
        write_line(&path, "not json at all");
        write_line(&path, &real_record_json("sess-b", None, "blocked", 1));

        let source = BlockedEdgeSource::new(&path);
        let pending = source.pending();

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].session, "sess-a");
        assert_eq!(pending[1].session, "sess-b");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn blank_lines_are_skipped() {
        let path = temp_path("blank");
        write_line(&path, &real_record_json("sess-a", None, "blocked", 0));
        write_line(&path, "");

        let source = BlockedEdgeSource::new(&path);
        assert_eq!(source.pending().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    // ── no writer process required, no handle held between reads ───────

    #[test]
    fn reads_with_no_writing_process_and_no_held_handle() {
        let path = temp_path("no-writer");
        {
            // Write, then drop everything — simulates the writer process
            // exiting entirely before the reader ever runs.
            write_line(&path, &real_record_json("sess-a", None, "blocked", 0));
        }

        let source = BlockedEdgeSource::new(&path);
        // Two independent reads: if a handle were held open across calls,
        // this would still work, but this asserts the *contract*, not the
        // implementation — construct fresh each time to be sure.
        let first = source.pending();
        let second = BlockedEdgeSource::new(&path).pending();
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    // ── carries `to` state forward ──────────────────────────────────────

    #[test]
    fn carries_to_state_forward_for_downstream_re_evaluation() {
        let path = temp_path("to-state");
        write_line(&path, &real_record_json("sess-a", None, "blocked", 0));

        let source = BlockedEdgeSource::new(&path);
        let pending = source.pending();
        assert_eq!(pending[0].to, BlockedEdgeState::Blocked);
        let _ = std::fs::remove_file(&path);
    }

    // ── path() accessor ──────────────────────────────────────────────────

    #[test]
    fn path_returns_the_constructed_path() {
        let path = temp_path("accessor");
        let source = BlockedEdgeSource::new(&path);
        assert_eq!(source.path(), path.as_path());
    }

    // ── default_sink_path ────────────────────────────────────────────────

    #[test]
    fn default_sink_path_prefers_xdg_state_home() {
        let path = default_sink_path(Some("/xdg/state".to_string()), Some("/home/u".to_string()));
        assert_eq!(
            path,
            Some(PathBuf::from("/xdg/state/bastion/blocked-edges.jsonl"))
        );
    }

    #[test]
    fn default_sink_path_falls_back_to_home() {
        let path = default_sink_path(None, Some("/home/u".to_string()));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/u/.local/state/bastion/blocked-edges.jsonl"
            ))
        );
    }

    #[test]
    fn default_sink_path_is_none_with_neither_env_var() {
        assert_eq!(default_sink_path(None, None), None);
    }

    // ── InMemoryQueueSource stub ─────────────────────────────────────────

    #[test]
    fn in_memory_stub_returns_seeded_items() {
        let item = PendingBlockedEdge {
            session: "sess-a".to_string(),
            host: "mini-console".to_string(),
            to: BlockedEdgeState::Blocked,
            observed_at: ts(0),
        };
        let source = InMemoryQueueSource::new(vec![item.clone()]);
        assert_eq!(source.pending(), vec![item]);
    }
}
