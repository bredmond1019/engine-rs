//! Injectable `ApprovalLedger` seam + file-backed default impl — `EN.8.C`
//! task 2.
//!
//! Mirrors this repo's established injectable-seam convention (see
//! `crates/engine-core/src/cron/store.rs`'s `CronStore`/`FileCronStore`
//! pair): a trait here ([`ApprovalLedger`]), a real impl backed by an
//! append-only JSON-Lines file ([`FileApprovalLedger`]), and an in-memory
//! `#[cfg(test)]` stub ([`InMemoryApprovalLedger`]) so the gated suite never
//! performs real filesystem writes.
//!
//! [`FileApprovalLedger`] follows `bastion`'s `src/serve/blocked_edge/sink.rs`
//! shape — that file is the reference implementation for this exact
//! problem (append-only JSONL, cheap to construct, opened fresh in append
//! mode on every write so there is no long-lived handle to worry about).
//! It deliberately diverges from that reference on read behavior: per this
//! block's own acceptance criteria, a malformed line is skipped with a
//! warning rather than surfaced as an error, because a partially-written
//! line from a killed process must not make the rest of the ledger
//! unreadable.
//!
//! Every trait method is infallible by design (`append`/`read_all`/
//! `rows_for` return no `Result`) — writes are best-effort exactly like
//! `FileCronStore::persist`: a failed disk write must not panic a live
//! approval-gate path over a transient I/O issue. The in-memory state (and
//! the caller's control flow) stays correct even when the durable copy
//! momentarily doesn't.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;

use super::record::ApprovalLedgerRow;

/// The injectable seam for approval-ledger persistence — mirrors
/// `CronStore`'s trait/impl/stub convention. A default impl
/// ([`FileApprovalLedger`]) persists to an append-only JSONL file; tests
/// substitute [`InMemoryApprovalLedger`] (no real filesystem writes in the
/// gated suite).
pub trait ApprovalLedger: Send + Sync {
    /// Append one row. Never overwrites or coalesces — two calls for the
    /// same `item_id` produce two distinct, independently readable rows.
    fn append(&self, row: ApprovalLedgerRow);
    /// Every row currently in the ledger, oldest first.
    fn read_all(&self) -> Vec<ApprovalLedgerRow>;
    /// Every row for one `item_id`, oldest first.
    fn rows_for(&self, item_id: &str) -> Vec<ApprovalLedgerRow>;
}

/// An [`ApprovalLedger`] backed by a single append-only JSON-Lines file:
/// one row per line, opened fresh in append mode for every write.
///
/// Cheap to construct (just a path) and cheap to clone. The file (and its
/// parent directory) is created lazily on the first [`Self::append`] —
/// constructing a `FileApprovalLedger` never touches the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileApprovalLedger {
    path: PathBuf,
}

impl FileApprovalLedger {
    /// Point a ledger at `path`. Does not touch the filesystem.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path this ledger reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ApprovalLedger for FileApprovalLedger {
    fn append(&self, row: ApprovalLedgerRow) {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                // Best-effort: a failed directory create surfaces as a
                // failed open just below, which is itself swallowed per
                // this trait's infallible contract.
                let _ = std::fs::create_dir_all(parent);
            }
        }

        let Ok(line) = serde_json::to_string(&row) else {
            // A row that fails to serialize is a programmer error in the
            // row type, not a runtime condition the ledger can recover
            // from mid-write; drop it rather than poison the file with a
            // partial line.
            return;
        };

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn read_all(&self) -> Vec<ApprovalLedgerRow> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            // A missing file (nothing written yet) is not an error — an
            // empty ledger reads as an empty Vec, same as an empty file.
            Err(_) => return Vec::new(),
        };

        let reader = BufReader::new(file);
        let mut rows = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<ApprovalLedgerRow>(trimmed) {
                Ok(row) => rows.push(row),
                Err(err) => {
                    // A malformed line (e.g. a partially-written line from
                    // a killed process) is skipped, never a panic and
                    // never a whole-read failure — the surrounding valid
                    // rows must still come back.
                    tracing::warn!(
                        path = %self.path.display(),
                        error = %err,
                        "skipping malformed approval ledger line"
                    );
                }
            }
        }
        rows
    }

    fn rows_for(&self, item_id: &str) -> Vec<ApprovalLedgerRow> {
        self.read_all()
            .into_iter()
            .filter(|row| row.item_id == item_id)
            .collect()
    }
}

/// Resolve the default ledger path:
/// `$XDG_STATE_HOME/engine-rs/approval-ledger.jsonl`, falling back to
/// `$HOME/.local/state/engine-rs/approval-ledger.jsonl`. Returns `None`
/// when neither is set.
///
/// Pure function over its two arguments — reads no environment itself — so
/// it is testable without touching the process environment, mirroring
/// `bastion`'s `default_sink_path` and this repo's own `CronStore`
/// XDG-resolution convention.
pub fn default_ledger_path(
    xdg_state_home: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_state_home {
        Some(
            PathBuf::from(xdg)
                .join("engine-rs")
                .join("approval-ledger.jsonl"),
        )
    } else {
        home.map(|h| {
            PathBuf::from(h)
                .join(".local")
                .join("state")
                .join("engine-rs")
                .join("approval-ledger.jsonl")
        })
    }
}

/// An in-memory [`ApprovalLedger`] stub — no real file I/O. Matches
/// `crates/engine-core/src/cron/store.rs`'s `InMemoryCronStore`
/// convention, so the gated suite exercises the ledger contract without
/// ever writing to a real filesystem.
#[cfg(test)]
pub struct InMemoryApprovalLedger {
    rows: Mutex<Vec<ApprovalLedgerRow>>,
}

#[cfg(test)]
impl InMemoryApprovalLedger {
    pub fn new() -> Self {
        Self {
            rows: Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl Default for InMemoryApprovalLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl ApprovalLedger for InMemoryApprovalLedger {
    fn append(&self, row: ApprovalLedgerRow) {
        self.rows
            .lock()
            .expect("ApprovalLedger mutex poisoned")
            .push(row);
    }

    fn read_all(&self) -> Vec<ApprovalLedgerRow> {
        self.rows
            .lock()
            .expect("ApprovalLedger mutex poisoned")
            .clone()
    }

    fn rows_for(&self, item_id: &str) -> Vec<ApprovalLedgerRow> {
        self.rows
            .lock()
            .expect("ApprovalLedger mutex poisoned")
            .iter()
            .filter(|row| row.item_id == item_id)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::ledger::record::LedgerDecision;
    use chrono::{DateTime, TimeZone, Utc};

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn row(item_id: &str, decided_at_secs: i64) -> ApprovalLedgerRow {
        ApprovalLedgerRow {
            item_id: item_id.to_string(),
            digest: "digest-a".to_string(),
            decision: LedgerDecision::Approved,
            who: "operator-a".to_string(),
            delivered_at: ts(0),
            decided_at: ts(decided_at_secs),
            rendered_diff: "rendered summary".to_string(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "engine-rs-approval-ledger-test-{}-{}-{}",
            std::process::id(),
            name,
            uuid::Uuid::new_v4()
        ))
    }

    // ── InMemoryApprovalLedger ───────────────────────────────────────────

    #[test]
    fn in_memory_ledger_append_then_read_all_round_trips() {
        let ledger = InMemoryApprovalLedger::new();
        ledger.append(row("item-a", 10));
        assert_eq!(ledger.read_all(), vec![row("item-a", 10)]);
    }

    #[test]
    fn in_memory_ledger_two_appends_same_item_produce_two_rows() {
        let ledger = InMemoryApprovalLedger::new();
        ledger.append(row("item-a", 10));
        ledger.append(row("item-a", 20));
        assert_eq!(ledger.rows_for("item-a").len(), 2);
    }

    #[test]
    fn in_memory_ledger_rows_for_filters_by_item_id() {
        let ledger = InMemoryApprovalLedger::new();
        ledger.append(row("item-a", 10));
        ledger.append(row("item-b", 20));
        assert_eq!(ledger.rows_for("item-a").len(), 1);
        assert_eq!(ledger.rows_for("item-b").len(), 1);
    }

    // ── FileApprovalLedger ────────────────────────────────────────────────

    #[test]
    fn file_ledger_append_then_read_all_round_trips_one_row() {
        let path = temp_path("roundtrip");
        let ledger = FileApprovalLedger::new(&path);
        ledger.append(row("item-a", 10));

        let read = ledger.read_all();
        assert_eq!(read, vec![row("item-a", 10)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_ledger_read_all_on_missing_file_returns_empty_not_panic() {
        let path = temp_path("missing");
        let ledger = FileApprovalLedger::new(&path);
        assert!(ledger.read_all().is_empty());
    }

    #[test]
    fn file_ledger_append_creates_missing_parent_directory() {
        let path = temp_path("nested")
            .join("sub")
            .join("dir")
            .join("ledger.jsonl");
        let ledger = FileApprovalLedger::new(&path);
        ledger.append(row("item-a", 10));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap().parent().unwrap());
    }

    #[test]
    fn file_ledger_two_appends_same_item_produce_two_lines_not_one() {
        let path = temp_path("two-lines");
        let ledger = FileApprovalLedger::new(&path);
        ledger.append(row("item-a", 10));
        ledger.append(row("item-a", 20));

        let contents = std::fs::read_to_string(&path).expect("read_to_string should succeed");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "two appends must never overwrite or coalesce"
        );

        let read = ledger.rows_for("item-a");
        assert_eq!(read.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_ledger_survives_drop_and_reconstruction_at_the_same_path() {
        let path = temp_path("restart");
        {
            let writer = FileApprovalLedger::new(&path);
            writer.append(row("item-a", 10));
            // `writer` dropped here — simulating a process restart.
        }

        let reopened = FileApprovalLedger::new(&path);
        let read = reopened.read_all();
        assert_eq!(read.len(), 1, "row should survive restart");
        assert_eq!(read[0].item_id, "item-a");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_ledger_is_readable_from_an_independently_constructed_instance() {
        // Nothing here is shared between the writer and reader except the
        // path — the same relationship a second OS process would have.
        let path = temp_path("cross-process");
        let writer = FileApprovalLedger::new(&path);
        writer.append(row("item-a", 10));

        let reader = FileApprovalLedger::new(path.clone());
        assert_eq!(reader.read_all().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_ledger_read_all_skips_malformed_line_without_panicking() {
        let path = temp_path("malformed");
        let ledger = FileApprovalLedger::new(&path);
        ledger.append(row("item-a", 10));

        // Inject a malformed line between two valid rows.
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open for append should succeed");
            writeln!(file, "not json").expect("write malformed line should succeed");
        }
        ledger.append(row("item-b", 20));

        let read = ledger.read_all();
        assert_eq!(
            read.len(),
            2,
            "the malformed line must be skipped, not surfaced as an error or panic"
        );
        assert_eq!(read[0].item_id, "item-a");
        assert_eq!(read[1].item_id, "item-b");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_ledger_read_all_skips_blank_lines() {
        let path = temp_path("blank-lines");
        let ledger = FileApprovalLedger::new(&path);
        ledger.append(row("item-a", 10));

        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open for append should succeed");
        writeln!(file).expect("write blank line should succeed");
        drop(file);

        let read = ledger.read_all();
        assert_eq!(read.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    // ── default_ledger_path ──────────────────────────────────────────────

    #[test]
    fn default_ledger_path_prefers_xdg_state_home() {
        let path = default_ledger_path(Some("/xdg/state".to_string()), Some("/home/u".to_string()));
        assert_eq!(
            path,
            Some(PathBuf::from("/xdg/state/engine-rs/approval-ledger.jsonl"))
        );
    }

    #[test]
    fn default_ledger_path_falls_back_to_home() {
        let path = default_ledger_path(None, Some("/home/u".to_string()));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/u/.local/state/engine-rs/approval-ledger.jsonl"
            ))
        );
    }

    #[test]
    fn default_ledger_path_is_none_with_neither_env_var() {
        assert_eq!(default_ledger_path(None, None), None);
    }
}
