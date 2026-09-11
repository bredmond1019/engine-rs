//! CONSOLIDATE Task 2 — D57 §3's two-axis `origin_roadmap` selection rule, plus `--since`
//! scoping mirrored from `lane_log_watermark.py`'s `since_filter`.
//!
//! ## The two-axis rule, collapsed to one comparison
//!
//! D57 §3 states the rule in prose as two axes — "records with `roadmap: X` and
//! `lifecycle != consolidated`, **plus** items in other records with `origin_roadmap: X`" — but
//! read against its own worked example (a `close-the-loop` lane record carrying two blocks whose
//! `origin_roadmap` is `carryover-improvements`) the two axes are one comparison over each row's
//! *effective origin*, not a record-level filter:
//!
//! ```text
//! effective_origin(row) = row.origin_roadmap, or the ledger file's own top-level `roadmap` when absent
//!
//! select row for `roadmap_slug` when effective_origin(row) == roadmap_slug, AND:
//!   - if effective_origin(row) == the record's own roadmap (a NATIVE row): only when the
//!     record's `lifecycle` frontmatter is not `consolidated` (axis "a")
//!   - if effective_origin(row) != the record's own roadmap (an ADOPTED row): always, regardless
//!     of the record's own `lifecycle` (axis "b") — the record's `lifecycle` stamp tracks
//!     re-consolidation of *its own* roadmap's rows, not a foreign roadmap's adopted ones.
//! ```
//!
//! This is what makes all three of this task's acceptance criteria hold simultaneously: axis (a)
//! and (b) are independently testable (a native row is gated on lifecycle, an adopted one is
//! not); a record's own `lifecycle: consolidated` never excludes a *different* roadmap's
//! adopted row from it (that row's effective origin differs from the record's own roadmap, so
//! the lifecycle gate never applies to it); and a row whose `origin_roadmap` names some other,
//! third roadmap is excluded from both — it only ever selects for its own effective origin.
//!
//! ## Where a row's data comes from
//!
//! `RunRecordPair` (Task 1's `discover_participants` output) carries only the notes/review
//! frontmatter (`lifecycle`, `run_started`, `run_ended`) — it does not itself hold ledger rows.
//! The rows this module selects live in `verification-ledger.json` (`EN.15.L`), written as a
//! sibling of `notes.md`/`review.md` in the same `orchestration-run/<roadmap>/` directory (per
//! `docs/sandbox/run-verification-ledger-prompt.md`'s own instructions), so this module locates
//! it from whichever `RunRecord.path` is present and reads it directly. A record with no such
//! file alongside it contributes no rows — `EN.15.L` is this workflow's input, and an empty
//! ledger is a legitimate, un-erroring state (mirroring `read_lane_log`'s own tolerance for a
//! missing file).
//!
//! **Per-row `origin_roadmap` is not yet part of `docs/sandbox/run-verification-ledger-prompt.md`
//! nor `orchestration::ledger::LedgerEntry`** (checked directly — neither carries the field as of
//! this writing). This module reads it permissively as an optional string on each `entries[]`
//! object rather than assuming a schema not yet written, and treats its absence as "this row's
//! origin is the ledger file's own top-level `roadmap`" per D57 §3's own default rule. A future
//! ledger schema revision that adds the field formally needs no change here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::roadmap_status::{read_lane_log, RunRecordPair};

/// One ledger row selected for a roadmap's consolidation, with its resolved origin attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedRow {
    /// The repo that produced this row (the key this record was discovered under).
    pub repo: String,
    /// The ledger file's own top-level `roadmap` — the record's native roadmap.
    pub record_roadmap: String,
    /// This row's effective origin: its own `origin_roadmap` field, or `record_roadmap` when
    /// absent.
    pub origin_roadmap: String,
    /// The raw `entries[]` object, verbatim, for the disposal writer (Task 4) to map from.
    pub row: Value,
}

/// Read `<dir>/verification-ledger.json` if present. Returns `(record_roadmap, entries)`, or
/// `None` when the file is absent or not a JSON object — never an error, mirroring
/// `read_lane_log`'s tolerance for a missing sibling artifact.
fn read_ledger_file(dir: &Path) -> Option<(String, Vec<Value>)> {
    let path = dir.join("verification-ledger.json");
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let obj = value.as_object()?;
    let record_roadmap = obj.get("roadmap")?.as_str()?.to_string();
    let entries = obj
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some((record_roadmap, entries))
}

/// The directory a `RunRecordPair`'s ledger file lives in, taken from whichever of
/// `notes`/`review` is present (they are siblings in the same `orchestration-run/<roadmap>/`
/// directory, so either resolves to the same parent).
fn record_dir(pair: &RunRecordPair) -> Option<PathBuf> {
    pair.notes
        .as_ref()
        .map(|r| r.path.clone())
        .or_else(|| pair.review.as_ref().map(|r| r.path.clone()))
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// This record's own `lifecycle` frontmatter value, preferring `notes.md` (the primary record)
/// and falling back to `review.md`.
fn record_lifecycle(pair: &RunRecordPair) -> Option<String> {
    pair.notes
        .as_ref()
        .and_then(|r| r.lifecycle.clone())
        .or_else(|| pair.review.as_ref().and_then(|r| r.lifecycle.clone()))
}

/// Select every ledger row, across `records`, whose effective origin is `roadmap_slug` — D57
/// §3's two-axis rule, collapsed as described in this module's doc comment.
///
/// `records` need not be scoped to a single `orchestration-run/<roadmap_slug>/` directory: a row
/// adopted from `roadmap_slug` into a different driving lane lives in a record discovered under
/// *that* lane's own roadmap directory, so gathering the full candidate set across every roadmap
/// directory in the corpus is the caller's job (the `graph.rs` assembly, Task 6) — this function
/// is pure selection logic over whatever record set it is handed.
#[must_use]
pub fn select_ledger_rows(
    records: &BTreeMap<String, RunRecordPair>,
    roadmap_slug: &str,
) -> Vec<SelectedRow> {
    let mut selected = Vec::new();
    for (repo, pair) in records {
        let Some(dir) = record_dir(pair) else {
            continue;
        };
        let Some((record_roadmap, entries)) = read_ledger_file(&dir) else {
            continue;
        };
        let lifecycle = record_lifecycle(pair);
        let is_consolidated = lifecycle.as_deref() == Some("consolidated");

        for row in entries {
            let origin_roadmap = row
                .get("origin_roadmap")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| record_roadmap.clone());

            if origin_roadmap != roadmap_slug {
                continue;
            }
            let is_native = origin_roadmap == record_roadmap;
            if is_native && is_consolidated {
                // Axis (a): a native row in a record already consolidated for its own roadmap.
                continue;
            }
            // Axis (a) native-and-not-consolidated, or axis (b) adopted (always included).
            selected.push(SelectedRow {
                repo: repo.clone(),
                record_roadmap: record_roadmap.clone(),
                origin_roadmap: origin_roadmap.clone(),
                row,
            });
        }
    }
    selected
}

/// Split `participants` into `(included, excluded)` against `since`, mirroring
/// `lane_log_watermark.py`'s `since_filter` (lines 279-315) at the same granularity: instead of
/// scoping which *roadmaps* are included from a fleet-wide lane log (the Python's unit), this
/// scopes which *participants* (repos) are included from `<roadmap_dir>/lane-log.jsonl`, keyed by
/// each entry's own `repo` field. A participant is EXCLUDED only when every one of its lines is
/// both parseable and confirmed strictly before `since`; a malformed line, or any line with a
/// `ts` at/after `since`, keeps that participant included — failing toward inclusion exactly as
/// the Python's own comment (lines 283-286) requires, since an unknown timestamp must never be
/// treated as "before the cutoff". `since: None` means no filter: every participant is included,
/// none excluded, without inspecting the log at all.
///
/// `roadmap_dir` is the already-resolved `<root>/planning/roadmaps/<slug>` (or legacy
/// `<root>/planning/<slug>`) directory whose `lane-log.jsonl` is being scoped — callers already
/// have this from `crate::roadmap_status::resolve_roadmap_dir`.
#[must_use]
pub fn since_filter(
    roadmap_dir: &Path,
    participants: &[String],
    since: Option<DateTime<Utc>>,
) -> (Vec<String>, Vec<String>) {
    let Some(since) = since else {
        return (participants.to_vec(), Vec::new());
    };

    let (entries, malformed) = read_lane_log(roadmap_dir);
    let has_malformed = !malformed.is_empty();

    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for participant in participants {
        let mut has_recent = false;
        let mut has_line = false;
        for entry in &entries {
            let Some(repo) = entry.get("repo").and_then(Value::as_str) else {
                continue;
            };
            if repo != participant {
                continue;
            }
            has_line = true;
            if let Some(dt) = entry
                .get("ts")
                .and_then(Value::as_str)
                .and_then(parse_lane_log_ts)
            {
                if dt >= since {
                    has_recent = true;
                    break;
                }
            } else {
                // A parseable JSON line with an unparseable/missing `ts` — unknown timestamp,
                // fail toward inclusion just like a wholly malformed line.
                has_recent = true;
                break;
            }
        }
        // A participant with no lines of its own at all cannot be confirmed strictly before
        // `since`, so it stays included — same "unknown means included" reasoning as a malformed
        // line, and consistent with the Python's own fallback for a roadmap it can't resolve.
        if has_recent || !has_line || has_malformed {
            included.push(participant.clone());
        } else {
            excluded.push(participant.clone());
        }
    }
    (included, excluded)
}

/// Parse a lane-log `ts` value the same way `lane_log_watermark.py`'s `parse_ts` does: accepts a
/// trailing `Z` or a numeric offset, and treats a naive (offset-less) result as UTC rather than
/// rejecting it.
fn parse_lane_log_ts(ts: &str) -> Option<DateTime<Utc>> {
    let s = ts.trim();
    let normalized = s.strip_suffix('Z').map(|rest| format!("{rest}+00:00"));
    let candidate = normalized.as_deref().unwrap_or(s);
    DateTime::parse_from_rfc3339(candidate)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn write_notes(dir: &Path, lifecycle: &str) {
        write(
            &dir.join("notes.md"),
            &format!("---\nlifecycle: {lifecycle}\n---\n"),
        );
    }

    fn write_ledger(dir: &Path, roadmap: &str, entries: Vec<Value>) {
        write(
            &dir.join("verification-ledger.json"),
            &serde_json::json!({
                "roadmap": roadmap,
                "repo": "irrelevant-for-this-test",
                "lane": "irrelevant-for-this-test",
                "created": "2026-09-08T00:00:00Z",
                "status_values": ["untested", "tested", "partial", "failed", "blocked", "not_applicable"],
                "entries": entries,
            })
            .to_string(),
        );
    }

    fn record_pair(dir: &Path) -> RunRecordPair {
        // Mirrors roadmap_status::RunRecord's own frontmatter parsing shape closely enough for
        // this module's own reads (lifecycle + path) without depending on that parser directly.
        use crate::roadmap_status::RunRecord;
        let notes_path = dir.join("notes.md");
        let text = fs::read_to_string(&notes_path).unwrap();
        let lifecycle = text
            .lines()
            .find_map(|l| l.strip_prefix("lifecycle: "))
            .map(str::to_string);
        RunRecordPair {
            notes: Some(RunRecord {
                path: notes_path,
                lifecycle,
                run_started: None,
                run_ended: None,
            }),
            review: None,
        }
    }

    #[test]
    fn axis_a_native_row_excluded_once_record_is_consolidated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("close-the-loop");
        write_notes(&dir, "consolidated");
        write_ledger(
            &dir,
            "close-the-loop",
            vec![serde_json::json!({"id": "ctl-1", "block": "BT.1.A"})],
        );
        let mut records = BTreeMap::new();
        records.insert("base-template".to_string(), record_pair(&dir));

        let selected = select_ledger_rows(&records, "close-the-loop");
        assert!(
            selected.is_empty(),
            "a native row in a lifecycle:consolidated record must be excluded, got {selected:?}"
        );
    }

    #[test]
    fn axis_a_native_row_included_when_not_yet_consolidated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("close-the-loop");
        write_notes(&dir, "lane-complete");
        write_ledger(
            &dir,
            "close-the-loop",
            vec![serde_json::json!({"id": "ctl-1", "block": "BT.1.A"})],
        );
        let mut records = BTreeMap::new();
        records.insert("base-template".to_string(), record_pair(&dir));

        let selected = select_ledger_rows(&records, "close-the-loop");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].origin_roadmap, "close-the-loop");
    }

    #[test]
    fn d57_worked_example_close_the_loop_carries_two_carryover_improvements_blocks() {
        // D57 §3's own worked example: `close-the-loop`'s lane executes two blocks
        // `carryover-improvements` filed, alongside its own native work. `close-the-loop`'s
        // record is `lifecycle: consolidated` (its own roadmap has already been consolidated),
        // which must NOT suppress the two adopted rows when consolidating for
        // `carryover-improvements`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("close-the-loop");
        write_notes(&dir, "consolidated");
        write_ledger(
            &dir,
            "close-the-loop",
            vec![
                serde_json::json!({
                    "id": "bt-native-1",
                    "block": "BT.close-the-loop.1",
                }),
                serde_json::json!({
                    "id": "bt-adopted-1",
                    "block": "BT.ticket.generate-tasks-json-on-ticket",
                    "origin_roadmap": "carryover-improvements",
                }),
                serde_json::json!({
                    "id": "bt-adopted-2",
                    "block": "BT.ticket.harness-schema-realpath",
                    "origin_roadmap": "carryover-improvements",
                }),
            ],
        );
        let mut records = BTreeMap::new();
        records.insert("base-template".to_string(), record_pair(&dir));

        // Consolidating for close-the-loop itself: already consolidated, so nothing selects —
        // not even the native row.
        let for_close_the_loop = select_ledger_rows(&records, "close-the-loop");
        assert!(for_close_the_loop.is_empty());

        // Consolidating for carryover-improvements: the two adopted rows select via axis (b),
        // regardless of close-the-loop's own lifecycle: consolidated stamp.
        let mut for_carryover: Vec<String> = select_ledger_rows(&records, "carryover-improvements")
            .into_iter()
            .map(|r| r.row.get("id").unwrap().as_str().unwrap().to_string())
            .collect();
        for_carryover.sort();
        assert_eq!(for_carryover, vec!["bt-adopted-1", "bt-adopted-2"]);
    }

    #[test]
    fn row_naming_a_third_roadmap_is_excluded_from_both() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("close-the-loop");
        write_notes(&dir, "active");
        write_ledger(
            &dir,
            "close-the-loop",
            vec![serde_json::json!({
                "id": "elsewhere",
                "block": "SOME.1.A",
                "origin_roadmap": "a-third-roadmap",
            })],
        );
        let mut records = BTreeMap::new();
        records.insert("base-template".to_string(), record_pair(&dir));

        assert!(select_ledger_rows(&records, "close-the-loop").is_empty());
        assert!(select_ledger_rows(&records, "carryover-improvements").is_empty());
        assert_eq!(
            select_ledger_rows(&records, "a-third-roadmap").len(),
            1,
            "the row DOES select for the roadmap it actually names"
        );
    }

    #[test]
    fn missing_ledger_file_contributes_no_rows_never_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        write_notes(&dir, "active");
        let mut records = BTreeMap::new();
        records.insert("engine-rs".to_string(), record_pair(&dir));

        assert!(select_ledger_rows(&records, "demo").is_empty());
    }

    #[test]
    fn since_filter_none_includes_everyone_without_reading_the_log() {
        let tmp = tempfile::tempdir().unwrap();
        // Deliberately do not create lane-log.jsonl at all — since_filter(None) must not need it.
        let participants = vec!["engine-rs".to_string(), "mev".to_string()];
        let (included, excluded) = since_filter(tmp.path(), &participants, None);
        assert_eq!(included, participants);
        assert!(excluded.is_empty());
    }

    #[test]
    fn since_filter_earlier_than_watermark_matches_no_filter() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("lane-log.jsonl"),
            &format!(
                "{}\n{}\n",
                serde_json::json!({"repo": "engine-rs", "ts": "2026-09-08T00:00:00Z"}),
                serde_json::json!({"repo": "mev", "ts": "2026-09-05T00:00:00Z"})
            ),
        );
        let participants = vec!["engine-rs".to_string(), "mev".to_string()];

        let no_filter = since_filter(tmp.path(), &participants, None);
        let earlier_since = since_filter(
            tmp.path(),
            &participants,
            Some("2000-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()),
        );
        assert_eq!(no_filter, earlier_since);
        assert_eq!(earlier_since.0, participants);
        assert!(earlier_since.1.is_empty());
    }

    #[test]
    fn since_filter_excludes_a_participant_whose_every_line_is_strictly_before_since() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("lane-log.jsonl"),
            &format!(
                "{}\n{}\n",
                serde_json::json!({"repo": "engine-rs", "ts": "2026-09-08T00:00:00Z"}),
                serde_json::json!({"repo": "mev", "ts": "2026-09-01T00:00:00Z"})
            ),
        );
        let participants = vec!["engine-rs".to_string(), "mev".to_string()];
        let since = "2026-09-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let (included, excluded) = since_filter(tmp.path(), &participants, Some(since));
        assert_eq!(included, vec!["engine-rs".to_string()]);
        assert_eq!(excluded, vec!["mev".to_string()]);
    }

    #[test]
    fn since_filter_fails_toward_inclusion_on_a_malformed_line() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("lane-log.jsonl"),
            "not valid json\n{\"repo\": \"engine-rs\", \"ts\": \"2026-09-01T00:00:00Z\"}\n",
        );
        let participants = vec!["engine-rs".to_string()];
        let since = "2026-09-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        // engine-rs's own line is strictly before `since`, but the file also carries a malformed
        // line elsewhere, so the participant stays included per this task's fail-toward-inclusion
        // rule (mirroring the Python's `has_recent or bad`).
        let (included, excluded) = since_filter(tmp.path(), &participants, Some(since));
        assert_eq!(included, participants);
        assert!(excluded.is_empty());
    }

    #[test]
    fn since_filter_includes_a_participant_with_no_lines_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("lane-log.jsonl"),
            &format!(
                "{}\n",
                serde_json::json!({"repo": "engine-rs", "ts": "2026-09-01T00:00:00Z"})
            ),
        );
        let participants = vec!["never-logged".to_string()];
        let since = "2026-09-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let (included, excluded) = since_filter(tmp.path(), &participants, Some(since));
        assert_eq!(included, participants);
        assert!(excluded.is_empty());
    }
}
