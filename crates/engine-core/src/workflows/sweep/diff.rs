//! SWEEP diff — `EN.15.E` task 3: compare a freshly-built [`RawSnapshot`](super::RawSnapshot)
//! against the immediately preceding one on disk.
//!
//! Mirrors `roadmap_sweep.py`'s `diff_snapshots` (:262) and `_summarize_discovery_diff` (:453),
//! plus `build_dedup_history` (:496), `_escalation_key` (:171) and `_parse_iso` (:518). THE
//! PYTHON IS THE ORACLE (Fork 2) — every divergence below is named at the point it happens,
//! never silent.
//!
//! ## `prev = None` is a legitimate input, not an error
//!
//! The very first sweep for a roadmap has no preceding snapshot. `diff_snapshots` treats that
//! case specially (`roadmap_sweep.py`'s own "CONVERGENCE FIX" note): `non_escalation_diff` is
//! always `false` on a first sweep — there is no established baseline for state/lease/queue/
//! validate-brain drift to have moved AGAINST, so comparing against nothing would fire a bogus
//! "state drift" wake on every roadmap's very first pass. A genuinely present escalation on a
//! first sweep is NOT swallowed by this rule: `new_escalations` is still computed against an
//! empty prior list, so a real escalation always fires even with no prior baseline.
//!
//! ## The dedup key
//!
//! `_escalation_key` (:171) is the full tuple `(ts_utc, repo, lane, kind, gate_id, summary)`,
//! never `gate_id` alone — a lane can legitimately re-append a new escalation reusing an old
//! `gate_id` after a prior one cleared, so identity is the full tuple of fields that make two
//! lines "the same event", not just the routing address. This must match the Python
//! field-for-field or the golden replay (task 6) diverges on the second snapshot onward.
//!
//! ## Which `now` a projection uses
//!
//! The Python's `discovery` dict already carries pre-bucketed `stale`/`liveness` fields computed
//! at the moment THAT snapshot was originally measured — `diff_snapshots` just compares the two
//! already-bucketed projections. The Rust [`RawSnapshot`] instead carries a raw, unbucketed
//! `Coord<T>` for lane-registry/lease claims (`snapshot`'s own "one structural divergence" note),
//! so [`super::semantic_projection`] must be handed a `now` to bucket against — and it must be
//! EACH SIDE'S OWN measurement instant (parsed from that side's `ts_utc`), never the diff's own
//! wall-clock time, or a lease measured stale at prev-time could read as fresh purely because the
//! diff ran later. `parse_snapshot_now` below is that per-side instant.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::snapshot::{semantic_projection, RawSnapshot, SemanticProjection};

/// `roadmap_sweep.py`'s `diff_snapshots` (:262) return shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryDiff {
    pub changed: bool,
    pub new_escalations: Vec<Value>,
    pub escalation_count_prev: usize,
    pub escalation_count_curr: usize,
    pub non_escalation_diff: bool,
    pub non_escalation_summary: Option<BTreeMap<String, Vec<String>>>,
    pub first_sweep: bool,
}

/// A stable identity for a single escalation record — `roadmap_sweep.py`'s `_escalation_key`
/// (:171): the full `(ts_utc, repo, lane, kind, gate_id, summary)` tuple, never `gate_id` alone.
pub type EscalationKey = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// `roadmap_sweep.py`'s `_escalation_key` (:171).
#[must_use]
pub fn escalation_key(rec: &Value) -> EscalationKey {
    let field =
        |name: &str| -> Option<String> { rec.get(name).and_then(Value::as_str).map(String::from) };
    (
        field("ts_utc"),
        field("repo"),
        field("lane"),
        field("kind"),
        field("gate_id"),
        field("summary"),
    )
}

/// Parse a snapshot's own `ts_utc` (`utc_now_ts`'s `%Y-%m-%dT%H:%M:%SZ` format) back into the
/// instant that snapshot's own [`semantic_projection`] bucketing must be computed against — see
/// this module's doc comment, "Which `now` a projection uses". Falls back to the current instant
/// on an unparsable timestamp (should never happen for a snapshot this codebase wrote) rather
/// than panicking; a malformed `ts_utc` is a data problem to surface elsewhere, not a reason to
/// crash a diff.
fn parse_snapshot_now(ts_utc: &str) -> DateTime<Utc> {
    match DateTime::parse_from_rfc3339(ts_utc) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(err) => {
            tracing::warn!(ts_utc, error = %err, "sweep: unparsable snapshot ts_utc, using now");
            Utc::now()
        }
    }
}

/// `roadmap_sweep.py`'s `_summarize_discovery_diff` (:453) — a shallow, per-repo summary of
/// which projected lane sections differ; never a claim about WHY (the sweep may relay a diff,
/// never originate a finding). `validate_brain` is corpus-wide, not per-lane, so it is reported
/// once under the pseudo-key `"<corpus>"` rather than folded into the per-repo loop.
#[must_use]
pub fn summarize_discovery_diff(
    prev: &SemanticProjection,
    curr: &SemanticProjection,
) -> BTreeMap<String, Vec<String>> {
    let mut summary: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let repos: std::collections::BTreeSet<&String> =
        prev.lanes.keys().chain(curr.lanes.keys()).collect();
    for repo in repos {
        let prev_lane = prev.lanes.get(repo);
        let curr_lane = curr.lanes.get(repo);
        if prev_lane == curr_lane {
            continue;
        }
        let changed_sections = match (prev_lane, curr_lane) {
            (Some(p), Some(c)) => {
                let mut sections = Vec::new();
                if p.blocks != c.blocks {
                    sections.push("blocks".to_string());
                }
                if p.run_record != c.run_record {
                    sections.push("run_record".to_string());
                }
                if p.operator_gates != c.operator_gates {
                    sections.push("operator_gates".to_string());
                }
                if p.carryover != c.carryover {
                    sections.push("carryover".to_string());
                }
                if p.lane_registry != c.lane_registry {
                    sections.push("lane_registry".to_string());
                }
                if p.leases != c.leases {
                    sections.push("leases".to_string());
                }
                if p.message_queue != c.message_queue {
                    sections.push("message_queue".to_string());
                }
                sections
            }
            _ => vec!["<lane appeared or disappeared>".to_string()],
        };
        summary.insert(repo.clone(), changed_sections);
    }

    if prev.validate_brain_exit_code != curr.validate_brain_exit_code {
        summary.insert("<corpus>".to_string(), vec!["validate_brain".to_string()]);
    }

    summary
}

/// Compute what changed between the immediately preceding stored snapshot (`prev`, or `None` on
/// the very first sweep) and the freshly-measured `curr` — `roadmap_sweep.py`'s `diff_snapshots`
/// (:262). See this module's doc comment for the `prev = None` and `now` rules.
#[must_use]
pub fn diff_snapshots(prev: Option<&RawSnapshot>, curr: &RawSnapshot) -> DiscoveryDiff {
    let curr_escalations = &curr.escalations;

    let empty_escalations: &[Value] = &[];
    let (prev_escalations, non_escalation_diff, non_escalation_summary) = match prev {
        None => (empty_escalations, false, None),
        Some(prev) => {
            let prev_now = parse_snapshot_now(&prev.ts_utc);
            let curr_now = parse_snapshot_now(&curr.ts_utc);
            let prev_projected = semantic_projection(&prev.discovery, prev_now);
            let curr_projected = semantic_projection(&curr.discovery, curr_now);
            let non_escalation_diff = prev_projected != curr_projected;
            let non_escalation_summary = non_escalation_diff
                .then(|| summarize_discovery_diff(&prev_projected, &curr_projected));
            (
                prev.escalations.as_slice(),
                non_escalation_diff,
                non_escalation_summary,
            )
        }
    };

    let prev_keys: HashSet<EscalationKey> = prev_escalations.iter().map(escalation_key).collect();
    let new_escalations: Vec<Value> = curr_escalations
        .iter()
        .filter(|rec| !prev_keys.contains(&escalation_key(rec)))
        .cloned()
        .collect();

    DiscoveryDiff {
        changed: !new_escalations.is_empty() || non_escalation_diff,
        new_escalations,
        escalation_count_prev: prev_escalations.len(),
        escalation_count_curr: curr_escalations.len(),
        non_escalation_diff,
        non_escalation_summary,
        first_sweep: prev.is_none(),
    }
}

// -------------------------------------------------------------------------------------------
// Dedup history — reconstructed from every previously stored snapshot's `routed` section, never
// from a second state file (the hard constraint is ONE write, this directory only).
// -------------------------------------------------------------------------------------------

/// One entry of [`build_dedup_history`]'s returned map — `roadmap_sweep.py`'s
/// `{"ts": ..., "severity": ...}` (:496).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupEntry {
    pub ts: Option<String>,
    pub severity: Option<String>,
}

/// Read the `routed` array a real on-disk snapshot file carries (stamped there by task 4's
/// routing pass), tolerantly — an unreadable or unparsable file yields an empty list, mirroring
/// [`super::snapshot::load_snapshot`]'s own tolerant `except Exception` behavior. A file with no
/// `routed` key (e.g. one this crate's own [`super::build_raw_snapshot`] wrote, before routing
/// ran) also yields an empty list, never an error — that is `RawSnapshot`'s own shape and is
/// exactly what a first sweep looks like.
fn load_routed_entries(path: &Path) -> Vec<Value> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sweep: skipping unreadable snapshot for dedup history");
            return Vec::new();
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sweep: skipping unparsable snapshot for dedup history");
            return Vec::new();
        }
    };
    value
        .get("routed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Return `{gate_id: {ts, severity}}` for the most recent successful route of each `gate_id`,
/// scanning every snapshot file already on disk (oldest to newest, later overwrites earlier) —
/// `roadmap_sweep.py`'s `build_dedup_history` (:496). `skip` excludes a path (used defensively;
/// normally irrelevant since this is always called before this pass's own file is written).
#[must_use]
pub fn build_dedup_history(
    sweeps_directory: &Path,
    skip: Option<&Path>,
) -> BTreeMap<String, DedupEntry> {
    let mut history: BTreeMap<String, DedupEntry> = BTreeMap::new();
    for path in super::snapshot::list_snapshot_files(sweeps_directory) {
        if skip.is_some_and(|skip| skip == path) {
            continue;
        }
        for entry in load_routed_entries(&path) {
            let routed = entry
                .get("routed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !routed {
                continue;
            }
            let Some(gate_id) = entry.get("gate_id").and_then(Value::as_str) else {
                continue;
            };
            let ts = entry
                .get("ts_routed")
                .and_then(Value::as_str)
                .map(String::from);
            let severity = entry
                .get("severity")
                .and_then(Value::as_str)
                .map(String::from);
            history.insert(gate_id.to_string(), DedupEntry { ts, severity });
        }
    }
    history
}

/// `roadmap_sweep.py`'s `_parse_iso` (:518) — tolerant ISO-8601 parsing for a dedup history
/// entry's `ts`: a `Z` suffix is accepted (the Python replaces `Z` with `+00:00` before
/// `datetime.fromisoformat`), and a timestamp with no timezone offset at all is treated as UTC
/// (the Python's `if dt.tzinfo is None: dt = dt.replace(tzinfo=timezone.utc)`) rather than
/// rejected — match that tolerance, never assume strict RFC3339. An unparsable or absent
/// timestamp returns `None`, mirroring the Python's own `except Exception: return None`.
#[must_use]
pub fn parse_iso(ts: Option<&str>) -> Option<DateTime<Utc>> {
    let ts = ts?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(ts, format) {
            return Some(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap_status::{RoadmapStatusResult, ValidateBrainResult};
    use std::collections::BTreeMap as StdBTreeMap;
    use std::path::PathBuf;

    fn base_discovery() -> RoadmapStatusResult {
        RoadmapStatusResult {
            roadmap: "demo-roadmap".to_string(),
            roadmap_dir: PathBuf::from("/tmp/demo-roadmap"),
            lanes: StdBTreeMap::new(),
            repos_in_lane_log: vec![],
            repos_with_run_record_only: vec![],
            operator_coverage_total: 0,
            coverage_caveat: String::new(),
            validate_brain: ValidateBrainResult {
                cmd: "bastion validate-brain --state /tmp".to_string(),
                exit_code: Some(0),
                error: None,
            },
            malformed_lines: vec![],
        }
    }

    fn raw_snapshot(ts_utc: &str, escalations: Vec<Value>) -> RawSnapshot {
        RawSnapshot {
            ts_utc: ts_utc.to_string(),
            roadmap: "demo-roadmap".to_string(),
            git_sha: Some("abc1234".to_string()),
            discovery: base_discovery(),
            escalations,
        }
    }

    // -----------------------------------------------------------------------------------------
    // escalation_key — full tuple, not gate_id alone
    // -----------------------------------------------------------------------------------------

    #[test]
    fn escalation_key_is_the_full_tuple_not_gate_id_alone() {
        let a = serde_json::json!({
            "ts_utc": "2026-09-01T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "first",
        });
        let b = serde_json::json!({
            "ts_utc": "2026-09-02T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "second",
        });
        assert_ne!(escalation_key(&a), escalation_key(&b));
    }

    #[test]
    fn escalation_key_matches_for_identical_records() {
        let a = serde_json::json!({
            "ts_utc": "2026-09-01T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "first",
        });
        let b = a.clone();
        assert_eq!(escalation_key(&a), escalation_key(&b));
    }

    // -----------------------------------------------------------------------------------------
    // diff_snapshots — prev = None (first sweep)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn diff_snapshots_with_prev_none_returns_the_first_sweep_shape() {
        let curr = raw_snapshot("2026-09-08T00:00:00Z", vec![]);
        let diff = diff_snapshots(None, &curr);
        assert!(diff.first_sweep);
        assert!(!diff.non_escalation_diff);
        assert!(diff.non_escalation_summary.is_none());
        assert!(!diff.changed);
        assert_eq!(diff.escalation_count_prev, 0);
        assert_eq!(diff.escalation_count_curr, 0);
    }

    #[test]
    fn diff_snapshots_with_prev_none_still_fires_on_a_real_escalation() {
        let escalation = serde_json::json!({
            "ts_utc": "2026-09-08T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "brand new",
        });
        let curr = raw_snapshot("2026-09-08T00:00:00Z", vec![escalation.clone()]);
        let diff = diff_snapshots(None, &curr);
        assert!(diff.first_sweep);
        assert!(diff.changed);
        assert_eq!(diff.new_escalations, vec![escalation]);
    }

    // -----------------------------------------------------------------------------------------
    // diff_snapshots — dedup across prev/curr
    // -----------------------------------------------------------------------------------------

    #[test]
    fn diff_snapshots_deduplicates_an_escalation_present_in_both_prev_and_curr() {
        let escalation = serde_json::json!({
            "ts_utc": "2026-09-01T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "still open",
        });
        let prev = raw_snapshot("2026-09-01T00:00:00Z", vec![escalation.clone()]);
        let curr = raw_snapshot("2026-09-02T00:00:00Z", vec![escalation]);
        let diff = diff_snapshots(Some(&prev), &curr);
        assert!(diff.new_escalations.is_empty());
        assert!(!diff.changed);
    }

    #[test]
    fn diff_snapshots_reports_a_genuinely_new_escalation_alongside_an_old_one() {
        let old = serde_json::json!({
            "ts_utc": "2026-09-01T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G1", "summary": "still open",
        });
        let new = serde_json::json!({
            "ts_utc": "2026-09-02T00:00:00Z", "repo": "engine-rs", "lane": "engine-rs",
            "kind": "advisory", "gate_id": "G2", "summary": "brand new",
        });
        let prev = raw_snapshot("2026-09-01T00:00:00Z", vec![old.clone()]);
        let curr = raw_snapshot("2026-09-02T00:00:00Z", vec![old, new.clone()]);
        let diff = diff_snapshots(Some(&prev), &curr);
        assert_eq!(diff.new_escalations, vec![new]);
        assert!(diff.changed);
    }

    // -----------------------------------------------------------------------------------------
    // diff_snapshots — non-escalation diff on identical vs differing discovery
    // -----------------------------------------------------------------------------------------

    #[test]
    fn diff_over_two_identical_snapshots_reports_no_changes() {
        let prev = raw_snapshot("2026-09-01T00:00:00Z", vec![]);
        let curr = raw_snapshot("2026-09-02T00:00:00Z", vec![]);
        let diff = diff_snapshots(Some(&prev), &curr);
        assert!(!diff.changed);
        assert!(!diff.non_escalation_diff);
        assert!(diff.non_escalation_summary.is_none());
        assert!(!diff.first_sweep);
    }

    #[test]
    fn diff_reports_non_escalation_diff_and_a_summary_when_discovery_moves() {
        let mut prev_discovery = base_discovery();
        let mut curr_discovery = base_discovery();
        curr_discovery.validate_brain.exit_code = Some(1);
        prev_discovery.repos_in_lane_log = vec!["engine-rs".to_string()];
        curr_discovery.repos_in_lane_log = vec!["engine-rs".to_string()];

        let prev = RawSnapshot {
            discovery: prev_discovery,
            ..raw_snapshot("2026-09-01T00:00:00Z", vec![])
        };
        let curr = RawSnapshot {
            discovery: curr_discovery,
            ..raw_snapshot("2026-09-02T00:00:00Z", vec![])
        };
        let diff = diff_snapshots(Some(&prev), &curr);
        assert!(diff.changed);
        assert!(diff.non_escalation_diff);
        let summary = diff
            .non_escalation_summary
            .expect("summary must be present");
        assert_eq!(
            summary.get("<corpus>"),
            Some(&vec!["validate_brain".to_string()])
        );
    }

    // -----------------------------------------------------------------------------------------
    // summarize_discovery_diff
    // -----------------------------------------------------------------------------------------

    #[test]
    fn summarize_discovery_diff_names_lane_appeared_or_disappeared() {
        let now = Utc::now();
        let mut curr_discovery = base_discovery();
        let lane = crate::roadmap_status::LaneResult {
            repo: "engine-rs".to_string(),
            ..crate::roadmap_status::LaneResult::default()
        };
        curr_discovery.lanes.insert("engine-rs".to_string(), lane);

        let prev_projected = semantic_projection(&base_discovery(), now);
        let curr_projected = semantic_projection(&curr_discovery, now);
        let summary = summarize_discovery_diff(&prev_projected, &curr_projected);
        assert_eq!(
            summary.get("engine-rs"),
            Some(&vec!["<lane appeared or disappeared>".to_string()])
        );
    }

    // -----------------------------------------------------------------------------------------
    // build_dedup_history
    // -----------------------------------------------------------------------------------------

    fn write_snapshot_with_routed(dir: &Path, filename: &str, routed: Value) {
        fs::create_dir_all(dir).unwrap();
        let body = serde_json::json!({
            "ts_utc": "2026-09-01T00:00:00Z",
            "roadmap": "demo-roadmap",
            "git_sha": "abc1234",
            "discovery": {
                "roadmap": "demo-roadmap", "roadmap_dir": "/tmp/demo-roadmap", "lanes": {},
                "repos_in_lane_log": [], "repos_with_run_record_only": [],
                "operator_coverage_total": 0, "coverage_caveat": "",
                "validate_brain": {"cmd": "x", "exit_code": null, "error": null},
                "malformed_lines": []
            },
            "escalations": [],
            "routed": routed,
        });
        fs::write(dir.join(filename), body.to_string()).unwrap();
    }

    #[test]
    fn build_dedup_history_returns_the_most_recent_route_per_gate_id() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sweeps");
        write_snapshot_with_routed(
            &dir,
            "2026-09-01T00-00-00Z.json",
            serde_json::json!([
                {"gate_id": "G1", "routed": true, "ts_routed": "2026-09-01T00:00:00Z", "severity": "blocking"}
            ]),
        );
        write_snapshot_with_routed(
            &dir,
            "2026-09-02T00-00-00Z.json",
            serde_json::json!([
                {"gate_id": "G1", "routed": true, "ts_routed": "2026-09-02T00:00:00Z", "severity": "advisory"}
            ]),
        );
        let history = build_dedup_history(&dir, None);
        assert_eq!(
            history.get("G1"),
            Some(&DedupEntry {
                ts: Some("2026-09-02T00:00:00Z".to_string()),
                severity: Some("advisory".to_string()),
            })
        );
    }

    #[test]
    fn build_dedup_history_ignores_unrouted_entries() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sweeps");
        write_snapshot_with_routed(
            &dir,
            "2026-09-01T00-00-00Z.json",
            serde_json::json!([
                {"gate_id": "G1", "routed": false, "reason": "skip-dedup"}
            ]),
        );
        let history = build_dedup_history(&dir, None);
        assert!(history.get("G1").is_none());
    }

    #[test]
    fn build_dedup_history_over_a_missing_directory_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let history = build_dedup_history(&root.path().join("no-such-dir"), None);
        assert!(history.is_empty());
    }

    #[test]
    fn build_dedup_history_tolerates_a_file_with_no_routed_section() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sweeps");
        fs::create_dir_all(&dir).unwrap();
        let snap = raw_snapshot("2026-09-01T00:00:00Z", vec![]);
        fs::write(
            dir.join("2026-09-01T00-00-00Z.json"),
            serde_json::to_string(&snap).unwrap(),
        )
        .unwrap();
        let history = build_dedup_history(&dir, None);
        assert!(history.is_empty());
    }

    #[test]
    fn build_dedup_history_respects_skip() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sweeps");
        write_snapshot_with_routed(
            &dir,
            "2026-09-01T00-00-00Z.json",
            serde_json::json!([
                {"gate_id": "G1", "routed": true, "ts_routed": "2026-09-01T00:00:00Z", "severity": "blocking"}
            ]),
        );
        let skip_path = dir.join("2026-09-01T00-00-00Z.json");
        let history = build_dedup_history(&dir, Some(&skip_path));
        assert!(history.is_empty());
    }

    // -----------------------------------------------------------------------------------------
    // parse_iso
    // -----------------------------------------------------------------------------------------

    #[test]
    fn parse_iso_accepts_a_z_suffixed_timestamp() {
        let parsed = parse_iso(Some("2026-09-01T00:00:00Z")).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-09-01T00:00:00+00:00");
    }

    #[test]
    fn parse_iso_treats_a_naive_timestamp_as_utc() {
        let parsed = parse_iso(Some("2026-09-01T00:00:00")).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-09-01T00:00:00+00:00");
    }

    #[test]
    fn parse_iso_returns_none_for_absent_or_malformed_input() {
        assert!(parse_iso(None).is_none());
        assert!(parse_iso(Some("not-a-timestamp")).is_none());
    }
}
