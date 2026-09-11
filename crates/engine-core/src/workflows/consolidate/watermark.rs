//! CONSOLIDATE Task 3 — the shared consolidation watermark.
//!
//! Ports `base-template/scripts/lane_log_watermark.py`'s `cmd_advance` (lines 380-422) into Rust,
//! sharing the SAME on-disk file (`<brain_root>/planning/open-work/orchestration-runs/
//! consolidation-watermark.json`, `WATERMARK_REL` in the Python) so either writer can read the
//! other's advance. This module never shells out to the Python for a read — [`read_watermark`]
//! is a plain JSON read — and [`advance_watermark`] mirrors the Python's drift check byte-for-
//! byte: it re-reads the CURRENT `lane-log.jsonl`, hashes the line at the watermark's stored
//! `line` index, and if that hash disagrees with the stored `last_line_sha256` (or `line` now
//! exceeds the log's length), it REFUSES rather than silently re-basing — exactly as
//! `cmd_advance` does at lane_log_watermark.py:400-403. A backwards advance is refused the same
//! way, mirroring the "refusing to move ... backwards" guard.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::roadmap_status::{resolve_roadmap_dir, RoadmapStatusError};

/// Where the watermark file lives, relative to the brain root — `WATERMARK_REL` in the Python.
const WATERMARK_REL: &str = "planning/open-work/orchestration-runs/consolidation-watermark.json";

/// The watermark schema version — `SCHEMA_VERSION` in the Python.
const SCHEMA_VERSION: u32 = 1;

/// One roadmap's watermark entry — the same five fields (plus `line`) the Python writes, so
/// either writer can read the other's advance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatermarkEntry {
    /// The last `lane-log.jsonl` line (1-indexed, counting only non-blank lines) this
    /// consolidation has consumed.
    pub line: usize,
    /// sha256 of that line's raw text, for the drift check — `None` when `line` is 0.
    pub last_line_sha256: Option<String>,
    /// The `ts` field of that line, carried for human reading only — never used as a cursor.
    pub last_ts: Option<String>,
    /// UTC timestamp of this advance, `%Y-%m-%dT%H:%M:%SZ` — matches the Python's `strftime`.
    pub consolidated_at: String,
    /// The run id that performed this advance, if any.
    pub run_id: Option<String>,
    /// 1-indexed original file line numbers (blank lines counted) that failed to parse as JSON
    /// at the moment of this advance — reported, never silently dropped.
    pub malformed_lines_at_advance: Vec<usize>,
}

/// The whole watermark file: `{"version": <int>, "roadmaps": {<slug>: WatermarkEntry}}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WatermarkFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    roadmaps: BTreeMap<String, WatermarkEntry>,
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

/// Everything that can go wrong advancing a watermark. Every variant is a REFUSAL — this module
/// never silently re-bases a cursor or moves one backwards.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatermarkError {
    /// The stored watermark no longer matches the log on disk: either the stored line's hash
    /// disagrees with what is on disk now, or `line` exceeds the log's current length. Mirrors
    /// `state_for`'s `drift` computation (lane_log_watermark.py:213-222).
    #[error("{0}")]
    Drift(String),
    /// The requested `to_line` is out of range for the log's current length.
    #[error("{0}")]
    OutOfRange(String),
    /// The requested `to_line` would move the watermark backwards from its current position.
    #[error("{0}")]
    Backwards(String),
    /// The roadmap directory could not be resolved (see [`RoadmapStatusError`]).
    #[error("{0}")]
    RoadmapNotFound(String),
    /// The watermark file exists but is not valid JSON, or an I/O error occurred writing it.
    #[error("{0}")]
    Io(String),
}

impl From<RoadmapStatusError> for WatermarkError {
    fn from(e: RoadmapStatusError) -> Self {
        WatermarkError::RoadmapNotFound(e.to_string())
    }
}

/// The result of a successful [`advance_watermark`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceOutcome {
    pub roadmap: String,
    pub advanced_from: usize,
    pub advanced_to: usize,
    pub consumed: usize,
    /// 1-indexed original file line numbers that failed to parse as JSON, at the moment of this
    /// advance — carried into the written entry's `malformed_lines_at_advance`.
    pub malformed_lines: Vec<usize>,
}

fn watermark_path(brain_root: &Path) -> PathBuf {
    brain_root.join(WATERMARK_REL)
}

fn load_watermark_file(brain_root: &Path) -> Result<WatermarkFile, WatermarkError> {
    let path = watermark_path(brain_root);
    if !path.is_file() {
        return Ok(WatermarkFile {
            version: SCHEMA_VERSION,
            roadmaps: BTreeMap::new(),
        });
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| WatermarkError::Io(format!("reading {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| WatermarkError::Io(format!("{} does not parse: {e}", path.display())))
}

fn save_watermark_file(brain_root: &Path, data: &WatermarkFile) -> Result<(), WatermarkError> {
    let path = watermark_path(brain_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| WatermarkError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(data)
        .map_err(|e| WatermarkError::Io(format!("serializing watermark: {e}")))?;
    fs::write(&path, format!("{text}\n"))
        .map_err(|e| WatermarkError::Io(format!("writing {}: {e}", path.display())))
}

/// Read the stored watermark entry for `slug`, or `None` when no watermark file exists yet, the
/// file has no entry for this roadmap, or the file fails to parse. A plain JSON read — this
/// function never shells out to `lane_log_watermark.py`.
pub fn read_watermark(brain_root: &Path, slug: &str) -> Option<WatermarkEntry> {
    let path = watermark_path(brain_root);
    let text = fs::read_to_string(path).ok()?;
    let data: WatermarkFile = serde_json::from_str(&text).ok()?;
    data.roadmaps.get(slug).cloned()
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Read `<roadmap_dir>/lane-log.jsonl` the way the Python's `read_lines` does: split on `\n`,
/// drop blank lines from the returned line list (never counted as malformed), and report the
/// ORIGINAL 1-indexed file line number of every non-blank line that fails to parse as JSON —
/// never dropping it from the count. A missing file yields `(vec![], vec![])`, matching the
/// Python's tolerance for "no log yet".
fn read_raw_lines(log_path: &Path) -> (Vec<String>, Vec<usize>) {
    let Ok(text) = fs::read_to_string(log_path) else {
        return (Vec::new(), Vec::new());
    };
    let mut lines = Vec::new();
    let mut bad = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        let file_line_number = i + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines.push(trimmed.to_string());
        if serde_json::from_str::<Value>(trimmed).is_err() {
            bad.push(file_line_number);
        }
    }
    (lines, bad)
}

fn ts_of(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("ts").and_then(|t| t.as_str()).map(|s| s.to_string()))
}

/// Advance `slug`'s watermark to `to_line`, stamping `run_id`. Mirrors `cmd_advance`
/// (lane_log_watermark.py:380-422) byte-for-byte:
///
/// 1. Re-read the CURRENT `lane-log.jsonl`. If the stored watermark's `line` exceeds the log's
///    current length, or the stored `last_line_sha256` disagrees with the hash of the line
///    actually at that index now, REFUSE with [`WatermarkError::Drift`] — never re-base.
/// 2. If `to_line` exceeds the log's current length, REFUSE with [`WatermarkError::OutOfRange`].
/// 3. If `to_line` is less than the stored watermark's current `line`, REFUSE with
///    [`WatermarkError::Backwards`] — a watermark's `line` never moves backwards.
/// 4. On success, write the SAME five fields the Python writes (`line`, `last_line_sha256`,
///    `last_ts`, `consolidated_at`, `run_id`, `malformed_lines_at_advance`), preserving every
///    other roadmap's untouched entry in the same file.
pub fn advance_watermark(
    brain_root: &Path,
    slug: &str,
    to_line: usize,
    run_id: &str,
) -> Result<AdvanceOutcome, WatermarkError> {
    let roadmap_dir = resolve_roadmap_dir(brain_root, slug)?;
    let log_path = roadmap_dir.join("lane-log.jsonl");
    let (lines, malformed) = read_raw_lines(&log_path);

    let existing = read_watermark(brain_root, slug);
    let current_line = existing.as_ref().map(|e| e.line).unwrap_or(0);
    let stored_hash = existing.as_ref().and_then(|e| e.last_line_sha256.clone());

    if current_line > 0 {
        if current_line > lines.len() {
            return Err(WatermarkError::Drift(format!(
                "watermark at line {current_line} but the log now has {} — the file shrank; \
                 lane-log.jsonl is append-only, so this is an edit or a truncation",
                lines.len()
            )));
        }
        if let Some(expected) = &stored_hash {
            let actual = sha256_hex(&lines[current_line - 1]);
            if &actual != expected {
                return Err(WatermarkError::Drift(format!(
                    "line {current_line} no longer matches the recorded hash — the log was \
                     rewritten, so every line number after it points somewhere else"
                )));
            }
        }
    }

    if to_line > lines.len() {
        return Err(WatermarkError::OutOfRange(format!(
            "--to-line {to_line} out of range (log has {} lines)",
            lines.len()
        )));
    }
    if to_line < current_line {
        return Err(WatermarkError::Backwards(format!(
            "refusing to move {slug}'s watermark backwards ({current_line} -> {to_line}); \
             delete the entry by hand if that is intended"
        )));
    }

    let last_line_sha256 = if to_line > 0 {
        Some(sha256_hex(&lines[to_line - 1]))
    } else {
        None
    };
    // Mirrors the Python's own (imperfect, kept for parity) indexing: `last_ts` is populated
    // only when `to_line - 1` (0-based index into the non-blank `lines` list) is not itself one
    // of the malformed file-line-numbers-minus-one.
    let last_ts = if to_line > 0 && !malformed.iter().any(|b| *b - 1 == to_line - 1) {
        ts_of(&lines[to_line - 1])
    } else {
        None
    };

    let entry = WatermarkEntry {
        line: to_line,
        last_line_sha256,
        last_ts,
        consolidated_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        run_id: Some(run_id.to_string()),
        malformed_lines_at_advance: malformed.clone(),
    };

    let mut data = load_watermark_file(brain_root)?;
    data.roadmaps.insert(slug.to_string(), entry);
    data.version = SCHEMA_VERSION;
    save_watermark_file(brain_root, &data)?;

    Ok(AdvanceOutcome {
        roadmap: slug.to_string(),
        advanced_from: current_line,
        advanced_to: to_line,
        consumed: to_line - current_line,
        malformed_lines: malformed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn lane_log_line(repo: &str, ts: &str) -> String {
        serde_json::json!({
            "repo": repo,
            "lane": "engine-lane",
            "block": "EN.1.A",
            "status": "done",
            "ts": ts
        })
        .to_string()
    }

    #[test]
    fn advancing_forward_from_a_python_written_fixture_entry_succeeds_and_preserves_siblings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        let line2 = lane_log_line("engine-rs", "2026-09-08T00:05:00Z");
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{line1}\n{line2}\n"),
        );

        // A Python-shaped watermark file: `demo` already advanced to line 1 by
        // lane_log_watermark.py, plus an untouched sibling roadmap entry.
        let watermark_json = serde_json::json!({
            "version": 1,
            "roadmaps": {
                "demo": {
                    "line": 1,
                    "last_line_sha256": sha256_hex(&line1),
                    "last_ts": "2026-09-08T00:00:00Z",
                    "consolidated_at": "2026-09-08T01:00:00Z",
                    "run_id": "python-run-1",
                    "malformed_lines_at_advance": []
                },
                "sibling-roadmap": {
                    "line": 7,
                    "last_line_sha256": "deadbeef",
                    "last_ts": "2026-09-01T00:00:00Z",
                    "consolidated_at": "2026-09-01T00:00:00Z",
                    "run_id": "some-other-run",
                    "malformed_lines_at_advance": [3]
                }
            }
        });
        write(
            &root.join("planning/open-work/orchestration-runs/consolidation-watermark.json"),
            &watermark_json.to_string(),
        );

        // read_watermark reads the Python's entry back correctly, without shelling out.
        let read_back = read_watermark(root, "demo").expect("entry present");
        assert_eq!(read_back.line, 1);
        assert_eq!(read_back.last_line_sha256, Some(sha256_hex(&line1)));

        let outcome = advance_watermark(root, "demo", 2, "rust-run-1").expect("advance succeeds");
        assert_eq!(outcome.advanced_from, 1);
        assert_eq!(outcome.advanced_to, 2);
        assert_eq!(outcome.consumed, 1);
        assert!(outcome.malformed_lines.is_empty());

        let new_entry = read_watermark(root, "demo").expect("entry present after advance");
        assert_eq!(new_entry.line, 2);
        assert_eq!(new_entry.last_line_sha256, Some(sha256_hex(&line2)));
        assert_eq!(new_entry.last_ts.as_deref(), Some("2026-09-08T00:05:00Z"));
        assert_eq!(new_entry.run_id.as_deref(), Some("rust-run-1"));

        // The untouched sibling roadmap entry survives byte-for-byte in the same file.
        let sibling = read_watermark(root, "sibling-roadmap").expect("sibling still present");
        assert_eq!(sibling.line, 7);
        assert_eq!(sibling.last_line_sha256.as_deref(), Some("deadbeef"));
        assert_eq!(sibling.run_id.as_deref(), Some("some-other-run"));
        assert_eq!(sibling.malformed_lines_at_advance, vec![3]);
    }

    #[test]
    fn advancing_past_a_drifted_mark_is_refused_never_rebased() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let original_line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{original_line1}\n"),
        );

        // Stored hash does not match what's on disk now — as if the log was rewritten after the
        // watermark was last advanced.
        let watermark_json = serde_json::json!({
            "version": 1,
            "roadmaps": {
                "demo": {
                    "line": 1,
                    "last_line_sha256": "not-the-real-hash",
                    "last_ts": "2026-09-08T00:00:00Z",
                    "consolidated_at": "2026-09-08T01:00:00Z",
                    "run_id": "python-run-1",
                    "malformed_lines_at_advance": []
                }
            }
        });
        write(
            &root.join("planning/open-work/orchestration-runs/consolidation-watermark.json"),
            &watermark_json.to_string(),
        );

        let err = advance_watermark(root, "demo", 1, "rust-run-1").unwrap_err();
        assert!(matches!(err, WatermarkError::Drift(_)), "got {err:?}");

        // The stored entry is untouched — no silent re-base.
        let still_stored = read_watermark(root, "demo").expect("entry present");
        assert_eq!(still_stored.last_line_sha256.as_deref(), Some("not-the-real-hash"));
    }

    #[test]
    fn advancing_past_a_shrunk_log_is_refused_as_drift() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // The log now has fewer lines than the watermark's stored `line`.
        write(&root.join("planning/roadmaps/demo/lane-log.jsonl"), "");

        let watermark_json = serde_json::json!({
            "version": 1,
            "roadmaps": {
                "demo": {
                    "line": 3,
                    "last_line_sha256": "irrelevant",
                    "last_ts": null,
                    "consolidated_at": "2026-09-08T01:00:00Z",
                    "run_id": null,
                    "malformed_lines_at_advance": []
                }
            }
        });
        write(
            &root.join("planning/open-work/orchestration-runs/consolidation-watermark.json"),
            &watermark_json.to_string(),
        );

        let err = advance_watermark(root, "demo", 0, "rust-run-1").unwrap_err();
        assert!(matches!(err, WatermarkError::Drift(_)), "got {err:?}");
    }

    #[test]
    fn a_backwards_advance_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        let line2 = lane_log_line("engine-rs", "2026-09-08T00:05:00Z");
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{line1}\n{line2}\n"),
        );

        let watermark_json = serde_json::json!({
            "version": 1,
            "roadmaps": {
                "demo": {
                    "line": 2,
                    "last_line_sha256": sha256_hex(&line2),
                    "last_ts": "2026-09-08T00:05:00Z",
                    "consolidated_at": "2026-09-08T01:00:00Z",
                    "run_id": "python-run-1",
                    "malformed_lines_at_advance": []
                }
            }
        });
        write(
            &root.join("planning/open-work/orchestration-runs/consolidation-watermark.json"),
            &watermark_json.to_string(),
        );

        let err = advance_watermark(root, "demo", 1, "rust-run-1").unwrap_err();
        assert!(matches!(err, WatermarkError::Backwards(_)), "got {err:?}");

        // Watermark stays exactly as it was — never rebased backwards.
        let still_stored = read_watermark(root, "demo").expect("entry present");
        assert_eq!(still_stored.line, 2);
    }

    #[test]
    fn first_advance_with_no_existing_watermark_file_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{line1}\n"),
        );

        assert!(read_watermark(root, "demo").is_none());

        let outcome = advance_watermark(root, "demo", 1, "rust-run-1").expect("advance succeeds");
        assert_eq!(outcome.advanced_from, 0);
        assert_eq!(outcome.advanced_to, 1);

        let entry = read_watermark(root, "demo").expect("entry present");
        assert_eq!(entry.line, 1);
        assert_eq!(entry.last_line_sha256, Some(sha256_hex(&line1)));
    }

    #[test]
    fn out_of_range_to_line_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{line1}\n"),
        );

        let err = advance_watermark(root, "demo", 5, "rust-run-1").unwrap_err();
        assert!(matches!(err, WatermarkError::OutOfRange(_)), "got {err:?}");
    }

    #[test]
    fn malformed_lines_are_reported_never_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let line1 = lane_log_line("engine-rs", "2026-09-08T00:00:00Z");
        // Line 2 is truncated / not valid JSON.
        write(
            &root.join("planning/roadmaps/demo/lane-log.jsonl"),
            &format!("{line1}\nnot valid json\n"),
        );

        let outcome = advance_watermark(root, "demo", 2, "rust-run-1").expect("advance succeeds");
        assert_eq!(outcome.malformed_lines, vec![2]);

        let entry = read_watermark(root, "demo").expect("entry present");
        assert_eq!(entry.malformed_lines_at_advance, vec![2]);
    }
}
