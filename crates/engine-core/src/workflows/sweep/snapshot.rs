//! SWEEP snapshot — `EN.15.E` task 2: build a raw snapshot of one roadmap's live state and
//! project it down to the shape a diff should actually depend on.
//!
//! Mirrors `roadmap_sweep.py`'s `build_raw_snapshot` (:233) and `semantic_projection` (:423),
//! including the per-shape projections `_project_block` (:347), `_project_lane_registry_entry`
//! (:358), `_project_lease` (:371), `_project_message_queue` (:385), `_project_lane` (:402) and
//! `_stable_sorted_dicts` (:326). THE PYTHON IS THE ORACLE (Fork 2) — every divergence below is
//! named at the point it happens, never silent.
//!
//! ## The one structural divergence from the Python raw snapshot
//!
//! The Python's raw `discovery.lanes.<repo>.lane_registry`/`leases` entries are FLAT dicts —
//! `agent_name`/`repo`/`lane`/... sit directly beside a pre-computed `heartbeat_age_seconds` and
//! `stale` bool (`roadmap_status_discovery.py`'s own join already buckets those). The engine's
//! `roadmap_status::discover` (`EN.15.H`) instead returns `coord::RegistryEntry { path, claim }`
//! / `coord::LeaseEntry { path, lease }`, where `claim`/`lease` is an `okf_core::Coord<T>` that
//! carries the RAW `heartbeat` string and computes no staleness bucket at all — that computation
//! simply doesn't exist yet on the Rust discovery side. [`semantic_projection`] below computes
//! the same STALE_THRESHOLD_SECONDS-bucketed `stale` bool itself (see [`REGISTRY_STALE_THRESHOLD_SECONDS`])
//! rather than reading a field that isn't there, so the *projected* shape still matches the
//! Python's field-for-field (`agent_name`, `repo`, `lane`, `roadmap`, `current_block`, `stale`,
//! `error` — raw `heartbeat`/`heartbeat_age_seconds`/`acquired_at` dropped, exactly as the
//! Python's own projection drops them).
//!
//! ## Staleness here is NEVER a clock
//!
//! Two independent staleness computations live in this file and must not be conflated:
//! - [`escalation_stale`] — an escalation record's `verified_at_sha` vs the repo's current HEAD
//!   short SHA (`roadmap_sweep.py:634-635`, module doc "THE STALENESS RULE"). A SHA comparison,
//!   never an elapsed-time one.
//! - The registry/lease `stale` bucket computed inside [`semantic_projection`] — a heartbeat-age
//!   threshold (`check_lane_agents.py`'s `STALE_THRESHOLD_SECONDS`, 3h), already pre-computed on
//!   the Python side and merely re-derived here since the Rust discovery join doesn't carry it.
//!
//! These two are deliberately uncoupled from each other and from `DEFAULT_REFIRE_HOURS` (task 4)
//! — three different questions, three different constants, per the Python module doc's own note.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use okf_core::Coord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coord as coordination;
use crate::roadmap_status::{
    self, BlockActivity, LaneResult, OperatorGate, RoadmapStatusError, RoadmapStatusResult,
    RunRecordPair, SdlcState,
};

/// `check_lane_agents.py`'s `STALE_THRESHOLD_SECONDS` (3h) — reused here for both a
/// lane-registry claim's `heartbeat` and a lease's `heartbeat` (falling back to `acquired_at`
/// when no heartbeat has been stamped yet), mirroring the Python discovery join's own bucketing
/// documented in `roadmap_sweep.py`'s `semantic_projection` module comment. Deliberately NOT the
/// same constant as `route`'s (task 4) `DEFAULT_REFIRE_HOURS` — a different question.
pub const REGISTRY_STALE_THRESHOLD_SECONDS: f64 = 10_800.0;

/// A raw, unprocessed snapshot of one roadmap's live state — `roadmap_sweep.py`'s
/// `build_raw_snapshot` return shape. Everything the sweep ever measures lives here, in full
/// fidelity; nothing is dropped. `ts_utc`/`roadmap`/`git_sha`/`discovery`/`escalations` are the
/// exact top-level keys the Python's stored `sweeps/<ts>.json` carries before a `diff`/`routed`
/// section (added by tasks 3/4) is stamped on top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSnapshot {
    pub ts_utc: String,
    pub roadmap: String,
    pub git_sha: Option<String>,
    pub discovery: RoadmapStatusResult,
    pub escalations: Vec<Value>,
}

/// `roadmap_sweep.py`'s `utc_now_ts` (:191) — `%Y-%m-%dT%H:%M:%SZ`, never an invented format.
pub fn utc_now_ts(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// `roadmap_sweep.py`'s `_snapshot_filename` (:207) — colons (legal in Unix filenames but
/// awkward to shell-quote) become `-`, matching every other timestamped artifact in this fleet.
pub fn snapshot_filename(ts_utc: &str) -> String {
    format!("{}.json", ts_utc.replace(':', "-"))
}

/// `roadmap_sweep.py`'s `sweeps_dir` (:214).
pub fn sweeps_dir(root: &Path, roadmap: &str) -> PathBuf {
    root.join("planning")
        .join("roadmaps")
        .join(roadmap)
        .join("sweeps")
}

/// `roadmap_sweep.py`'s `list_snapshot_files` (:218) — sorted `*.json` files, or an empty list
/// for a missing/non-directory path, never an error (a roadmap's first-ever sweep has no
/// `sweeps/` directory at all yet).
pub fn list_snapshot_files(sweeps_directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(sweeps_directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
        })
        .collect();
    files.sort();
    files
}

/// `roadmap_sweep.py`'s `load_snapshot` (:224) — an unreadable or unparsable file is reported
/// via `tracing::warn!` and skipped (`None`), never a panic; mirrors the Python's own tolerant
/// `except Exception` branch. Deserializing as [`RawSnapshot`] ignores the `diff`/`routed`
/// sections tasks 3/4 stamp onto a real on-disk file — `serde`'s default unknown-field behavior,
/// not `deny_unknown_fields` — so a real fixture from `planning/roadmaps/.../sweeps/*.json`
/// loads cleanly even though this task only builds the raw half.
pub fn load_snapshot(path: &Path) -> Option<RawSnapshot> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sweep: skipping unreadable snapshot");
            return None;
        }
    };
    match serde_json::from_str::<RawSnapshot>(&text) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sweep: skipping unparsable snapshot");
            None
        }
    }
}

/// `roadmap_sweep.py`'s `git_head_sha` (:180) — `git rev-parse --short HEAD` in `repo_path`.
/// Never crashes the sweep: a spawn failure, non-zero exit, or empty stdout all yield `None`.
pub fn git_head_sha(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .current_dir(repo_path)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => {
            tracing::warn!(error = %err, "sweep: git rev-parse failed");
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// `roadmap_sweep.py`'s `read_escalations` (:139) — reads `<roadmap_dir>/escalations.jsonl`
/// directly (NOT part of `discover()`'s join, per that module's own doc comment). A missing file
/// yields an empty list; a malformed or non-object line is reported via `tracing::warn!` and
/// skipped, never abandoning the rest of the file.
pub fn read_escalations(path: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for (idx, raw) in text.split('\n').enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value @ Value::Object(_)) => records.push(value),
            Ok(_) => {
                tracing::warn!(
                    path = %path.display(),
                    line = idx + 1,
                    "sweep: skipping non-object escalation line"
                );
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    line = idx + 1,
                    error = %err,
                    "sweep: skipping malformed escalation line"
                );
            }
        }
    }
    records
}

/// The parts of the snapshot that come purely from re-measuring the world — no diff, no routing
/// decisions. `roadmap_sweep.py`'s `build_raw_snapshot` (:233), with `discover()`/`git_head_sha`
/// injectable via [`build_raw_snapshot_with`] so tests never shell out or touch a real clock.
pub fn build_raw_snapshot(
    root: &Path,
    roadmap: &str,
    now: DateTime<Utc>,
) -> Result<RawSnapshot, RoadmapStatusError> {
    build_raw_snapshot_with(root, roadmap, now, git_head_sha)
}

/// Same as [`build_raw_snapshot`], with an injectable git-SHA reader so tests can build a raw
/// snapshot from a fixture corpus without a real `.git` directory or a real HEAD to race.
pub fn build_raw_snapshot_with(
    root: &Path,
    roadmap: &str,
    now: DateTime<Utc>,
    git_runner: impl Fn(&Path) -> Option<String>,
) -> Result<RawSnapshot, RoadmapStatusError> {
    let discovery = roadmap_status::discover_at(root, roadmap, now)?;
    let roadmap_dir = discovery.roadmap_dir.clone();
    let escalations = read_escalations(&roadmap_dir.join("escalations.jsonl"));
    let git_sha = git_runner(root);
    Ok(RawSnapshot {
        ts_utc: utc_now_ts(now),
        roadmap: roadmap.to_string(),
        git_sha,
        discovery,
        escalations,
    })
}

/// Whether one escalation record is stale relative to `current_sha` — `verified_at_sha` vs the
/// repo's current HEAD short SHA (`roadmap_sweep.py:634-635`, module doc "THE STALENESS RULE"),
/// NEVER an elapsed-time comparison. Mirrors the Python's
/// `bool(current_sha and verified_at_sha and current_sha != verified_at_sha)`: false whenever
/// either side is unknown, exactly as the Python's own `and`-chain is falsy in that case.
#[must_use]
pub fn escalation_stale(escalation: &Value, current_sha: Option<&str>) -> bool {
    let verified_at_sha = escalation.get("verified_at_sha").and_then(Value::as_str);
    match (current_sha, verified_at_sha) {
        (Some(current), Some(verified)) => current != verified,
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// Semantic projection — mirrors `roadmap_sweep.py`'s `semantic_projection` (:423) and its
// per-shape helpers. The raw snapshot above stays complete on disk (the evidence base); only the
// projection below is what a diff (task 3) compares.
// ---------------------------------------------------------------------------------------------

/// `roadmap_sweep.py`'s `semantic_projection` (:423) return shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticProjection {
    pub roadmap: String,
    pub repos_in_lane_log: Vec<String>,
    pub repos_with_run_record_only: Vec<String>,
    pub operator_coverage_total: usize,
    pub validate_brain_exit_code: Option<i32>,
    pub lanes: BTreeMap<String, ProjectedLane>,
}

/// `roadmap_sweep.py`'s `_project_lane` (:402).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedLane {
    pub repo: String,
    pub blocks: Vec<ProjectedBlock>,
    pub run_record: Option<RunRecordPair>,
    pub operator_gates: ProjectedOperatorGates,
    pub carryover: Vec<Value>,
    pub lane_registry: Vec<ProjectedRegistryEntry>,
    pub leases: Vec<ProjectedLease>,
    pub message_queue: ProjectedMessageQueue,
}

/// The `operator_gates` section of a [`ProjectedLane`] — `gates` kept verbatim (stably sorted),
/// `coverage_count` passed through untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedOperatorGates {
    pub gates: Vec<OperatorGate>,
    pub coverage_count: usize,
}

/// `roadmap_sweep.py`'s `_project_block` (:347) — `sdlc_state` projected via
/// [`ProjectedSdlcState`], every other field kept verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedBlock {
    pub block: String,
    pub status: String,
    pub note: Option<String>,
    pub ts: Option<String>,
    pub spec_slug: Option<String>,
    pub sdlc_state: Option<ProjectedSdlcState>,
}

/// `roadmap_sweep.py`'s `_project_sdlc_state` (inline in `_project_block`, :350-356) — drops
/// `path`/`updated_at`/`age_hours`; keeps the already-bucketed `liveness`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedSdlcState {
    pub status: String,
    pub status_known: bool,
    pub current_task: Option<Value>,
    pub tasks_run: Vec<Value>,
    pub liveness: String,
}

/// `roadmap_sweep.py`'s `_project_lane_registry_entry` (:358) — `stale` computed here (see the
/// module doc's "one structural divergence" note) rather than read off an already-bucketed
/// field; raw `heartbeat`/`heartbeat_age_seconds`/`acquired_at` never appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedRegistryEntry {
    pub agent_name: Option<String>,
    pub repo: Option<String>,
    pub lane: Option<String>,
    pub roadmap: Option<String>,
    pub current_block: Option<String>,
    pub stale: Option<bool>,
    pub error: Option<String>,
}

/// `roadmap_sweep.py`'s `_project_lease` (:371) — same divergence note as
/// [`ProjectedRegistryEntry`]; `holder` is the Python's name for what `okf_core::LeaseRecord`
/// calls `agent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedLease {
    pub repo: Option<String>,
    pub lane: Option<String>,
    pub holder: Option<String>,
    pub kind: Option<okf_core::LeaseKind>,
    pub scope: Option<okf_core::LeaseScope>,
    pub stale: Option<bool>,
    pub error: Option<String>,
}

/// `roadmap_sweep.py`'s `_project_message_queue` (:385) — reuses
/// [`REGISTRY_STALE_THRESHOLD_SECONDS`] for `oldest_unread_crossed_stale_threshold`, exactly as
/// the Python reuses `check_lane_agents.STALE_THRESHOLD_SECONDS` for the same field (its own
/// module comment: "rather than invent a new queue-specific SLA ... the natural reuse"). Raw
/// `oldest_unread_age_seconds`/`oldest_unread_sent_at` — the values that drift every single pass
/// with nothing else changing — are dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedMessageQueue {
    pub exists: Option<bool>,
    pub inbox_count: Option<usize>,
    pub oldest_unread_crossed_stale_threshold: Option<bool>,
    pub note: Option<String>,
}

/// Sort a list onto a stable, content-derived key so a reordered (but otherwise identical) list
/// of unordered records is never read as a change — `roadmap_sweep.py`'s `_stable_sorted_dicts`
/// (:326), which sorts on `json.dumps(d, sort_keys=True, default=str)`. This uses each item's
/// own serialized form as the sort key instead: not byte-identical to the Python's key string,
/// but equally deterministic and content-derived, which is all [`semantic_projection`]'s
/// idempotence property (two runs over identical input produce byte-identical output) requires.
fn stable_sort_by_json<T: Serialize>(items: &mut [T]) {
    items.sort_by_cached_key(|item| serde_json::to_string(item).unwrap_or_default());
}

fn heartbeat_age_seconds(heartbeat: &str, now: DateTime<Utc>) -> Option<f64> {
    let dt = DateTime::parse_from_rfc3339(heartbeat)
        .ok()?
        .with_timezone(&Utc);
    Some((now - dt).num_milliseconds() as f64 / 1000.0)
}

fn is_heartbeat_stale(heartbeat: &str, now: DateTime<Utc>) -> Option<bool> {
    heartbeat_age_seconds(heartbeat, now).map(|age| age > REGISTRY_STALE_THRESHOLD_SECONDS)
}

fn project_registry_entry(
    entry: &coordination::RegistryEntry,
    now: DateTime<Utc>,
) -> ProjectedRegistryEntry {
    match &entry.claim {
        Coord::Typed(claim) => ProjectedRegistryEntry {
            agent_name: Some(claim.agent_name.clone()),
            repo: Some(claim.repo.clone()),
            lane: Some(claim.lane.clone()),
            roadmap: Some(claim.roadmap.clone()),
            current_block: claim.current_block.clone(),
            stale: is_heartbeat_stale(&claim.heartbeat, now),
            error: None,
        },
        Coord::Legacy(raw) => ProjectedRegistryEntry {
            agent_name: raw
                .get("agent_name")
                .and_then(Value::as_str)
                .map(String::from),
            repo: raw.get("repo").and_then(Value::as_str).map(String::from),
            lane: raw.get("lane").and_then(Value::as_str).map(String::from),
            roadmap: raw.get("roadmap").and_then(Value::as_str).map(String::from),
            current_block: raw
                .get("current_block")
                .and_then(Value::as_str)
                .map(String::from),
            stale: None,
            error: Some("legacy/unrecognized lane-registry claim shape".to_string()),
        },
    }
}

fn project_lease(entry: &coordination::LeaseEntry, now: DateTime<Utc>) -> ProjectedLease {
    match &entry.lease {
        Coord::Typed(record) => {
            // A lease's liveness timestamp is its `heartbeat`, falling back to `acquired_at`
            // when no heartbeat has been stamped yet — mirrors `mev::brain::lease`'s own
            // staleness guard (`crates/engine-core/src/coord/write.rs`'s doc comment on
            // `LEASE_STALE_THRESHOLD_SECONDS`), not an independent re-derivation.
            let liveness_ts = record.heartbeat.as_deref().unwrap_or(&record.acquired_at);
            ProjectedLease {
                repo: Some(record.repo.clone()),
                lane: Some(record.lane.clone()),
                holder: Some(record.agent.clone()),
                kind: Some(record.kind),
                scope: record.scope,
                stale: is_heartbeat_stale(liveness_ts, now),
                error: None,
            }
        }
        Coord::Legacy(raw) => ProjectedLease {
            repo: raw.get("repo").and_then(Value::as_str).map(String::from),
            lane: raw.get("lane").and_then(Value::as_str).map(String::from),
            holder: raw.get("holder").and_then(Value::as_str).map(String::from),
            kind: None,
            scope: None,
            stale: None,
            error: Some("legacy/unrecognized lease shape".to_string()),
        },
    }
}

fn project_message_queue(
    queue: &roadmap_status::MessageQueueState,
    now: DateTime<Utc>,
) -> ProjectedMessageQueue {
    let crossed = queue.oldest_unread_age_seconds.map(|age| {
        // `now` is threaded through purely for API symmetry with the registry/lease
        // projections above; the age itself is already computed by `discover()`, so this is a
        // pure threshold comparison, never a second age computation.
        let _ = now;
        age > REGISTRY_STALE_THRESHOLD_SECONDS
    });
    ProjectedMessageQueue {
        exists: queue.exists,
        inbox_count: queue.inbox_count,
        oldest_unread_crossed_stale_threshold: crossed,
        note: queue.note.clone(),
    }
}

fn project_sdlc_state(state: &SdlcState) -> ProjectedSdlcState {
    ProjectedSdlcState {
        status: state.status.clone(),
        status_known: state.status_known,
        current_task: state.current_task.clone(),
        tasks_run: state.tasks_run.clone(),
        liveness: state.liveness.clone(),
    }
}

fn project_block(block: &BlockActivity) -> ProjectedBlock {
    ProjectedBlock {
        block: block.block.clone(),
        status: block.status.clone(),
        note: block.note.clone(),
        ts: block.ts.clone(),
        spec_slug: block.spec_slug.clone(),
        sdlc_state: block.sdlc_state.as_ref().map(project_sdlc_state),
    }
}

fn project_lane(lane: &LaneResult, now: DateTime<Utc>) -> ProjectedLane {
    let mut blocks: Vec<ProjectedBlock> = lane.blocks.iter().map(project_block).collect();
    stable_sort_by_json(&mut blocks);

    let mut gates: Vec<OperatorGate> = lane.operator_gates.gates.clone();
    stable_sort_by_json(&mut gates);

    let mut carryover: Vec<Value> = lane.carryover.clone();
    stable_sort_by_json(&mut carryover);

    let mut lane_registry: Vec<ProjectedRegistryEntry> = lane
        .lane_registry
        .iter()
        .map(|entry| project_registry_entry(entry, now))
        .collect();
    stable_sort_by_json(&mut lane_registry);

    let mut leases: Vec<ProjectedLease> = lane
        .leases
        .iter()
        .map(|entry| project_lease(entry, now))
        .collect();
    stable_sort_by_json(&mut leases);

    ProjectedLane {
        repo: lane.repo.clone(),
        blocks,
        run_record: lane.run_record.clone(),
        operator_gates: ProjectedOperatorGates {
            gates,
            coverage_count: lane.operator_gates.coverage_count,
        },
        carryover,
        lane_registry,
        leases,
        message_queue: project_message_queue(&lane.message_queue, now),
    }
}

/// Project a raw [`RoadmapStatusResult`] down to the state a wake should actually depend on —
/// `roadmap_sweep.py`'s `semantic_projection` (:423). Order-insensitive (every unordered-set
/// list is stably sorted); deliberately excludes the pass stamp and git SHA, which live at the
/// snapshot's top level, never inside `discovery`. This is what a diff (task 3) compares; the
/// raw snapshot stays complete on disk regardless.
#[must_use]
pub fn semantic_projection(
    discovery: &RoadmapStatusResult,
    now: DateTime<Utc>,
) -> SemanticProjection {
    let mut repos_in_lane_log = discovery.repos_in_lane_log.clone();
    repos_in_lane_log.sort();
    let mut repos_with_run_record_only = discovery.repos_with_run_record_only.clone();
    repos_with_run_record_only.sort();

    let lanes = discovery
        .lanes
        .iter()
        .map(|(repo, lane)| (repo.clone(), project_lane(lane, now)))
        .collect();

    SemanticProjection {
        roadmap: discovery.roadmap.clone(),
        repos_in_lane_log,
        repos_with_run_record_only,
        operator_coverage_total: discovery.operator_coverage_total,
        validate_brain_exit_code: discovery.validate_brain.exit_code,
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap_status::{MessageQueueState, ValidateBrainResult};
    use std::fs;

    fn write_lane_log(roadmap_dir: &Path, lines: &[&str]) {
        fs::write(roadmap_dir.join("lane-log.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn fixture_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // -----------------------------------------------------------------------------------------
    // utc_now_ts / snapshot_filename / sweeps_dir / list_snapshot_files
    // -----------------------------------------------------------------------------------------

    #[test]
    fn utc_now_ts_matches_the_python_format() {
        let now = DateTime::parse_from_rfc3339("2026-09-08T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(utc_now_ts(now), "2026-09-08T12:34:56Z");
    }

    #[test]
    fn snapshot_filename_replaces_colons() {
        assert_eq!(
            snapshot_filename("2026-08-28T01:30:33Z"),
            "2026-08-28T01-30-33Z.json"
        );
    }

    #[test]
    fn sweeps_dir_is_planning_roadmaps_slug_sweeps() {
        let root = Path::new("/brain");
        assert_eq!(
            sweeps_dir(root, "coordination-layer-port"),
            root.join("planning/roadmaps/coordination-layer-port/sweeps")
        );
    }

    #[test]
    fn list_snapshot_files_returns_empty_for_a_missing_directory() {
        let root = fixture_root();
        assert!(list_snapshot_files(&root.path().join("no-such-dir")).is_empty());
    }

    #[test]
    fn list_snapshot_files_is_sorted_and_json_only() {
        let root = fixture_root();
        let dir = root.path().join("sweeps");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("b.json"), "{}").unwrap();
        fs::write(dir.join("a.json"), "{}").unwrap();
        fs::write(dir.join("ignore.txt"), "nope").unwrap();
        let files: Vec<String> = list_snapshot_files(&dir)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(files, vec!["a.json".to_string(), "b.json".to_string()]);
    }

    // -----------------------------------------------------------------------------------------
    // read_escalations
    // -----------------------------------------------------------------------------------------

    #[test]
    fn read_escalations_returns_empty_for_a_missing_file() {
        let root = fixture_root();
        assert!(read_escalations(&root.path().join("escalations.jsonl")).is_empty());
    }

    #[test]
    fn read_escalations_skips_malformed_and_non_object_lines_but_keeps_the_rest() {
        let root = fixture_root();
        let path = root.path().join("escalations.jsonl");
        fs::write(
            &path,
            "{\"gate_id\": \"a\"}\nnot json\n[1,2,3]\n\n{\"gate_id\": \"b\"}\n",
        )
        .unwrap();
        let records = read_escalations(&path);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["gate_id"], "a");
        assert_eq!(records[1]["gate_id"], "b");
    }

    // -----------------------------------------------------------------------------------------
    // escalation_stale — verified_at_sha vs current HEAD, never elapsed time (AC 4)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn escalation_stale_is_true_only_when_shas_disagree() {
        let rec = serde_json::json!({"verified_at_sha": "abc123"});
        assert!(escalation_stale(&rec, Some("def456")));
        assert!(!escalation_stale(&rec, Some("abc123")));
    }

    #[test]
    fn escalation_stale_is_false_when_either_side_is_unknown() {
        let rec = serde_json::json!({"verified_at_sha": "abc123"});
        assert!(!escalation_stale(&rec, None));
        let rec_no_sha = serde_json::json!({});
        assert!(!escalation_stale(&rec_no_sha, Some("abc123")));
    }

    #[test]
    fn escalation_stale_never_reaches_for_a_timestamp_field() {
        // A record carrying only a timestamp (no verified_at_sha) must never be judged stale by
        // elapsed time — the whole point of AC 4.
        let rec = serde_json::json!({"ts_utc": "2020-01-01T00:00:00Z"});
        assert!(!escalation_stale(&rec, Some("abc123")));
    }

    // -----------------------------------------------------------------------------------------
    // build_raw_snapshot(_with) — round-trip shape
    // -----------------------------------------------------------------------------------------

    #[test]
    fn build_raw_snapshot_round_trips_to_json_in_the_pythons_top_level_shape() {
        let root = fixture_root();
        let roadmap_dir = root.path().join("planning/roadmaps/demo-roadmap");
        fs::create_dir_all(&roadmap_dir).unwrap();
        write_lane_log(&roadmap_dir, &[]);

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot =
            build_raw_snapshot_with(root.path(), "demo-roadmap", now, |_| Some("abc1234".into()))
                .expect("build_raw_snapshot_with must succeed against a fixture corpus");

        assert_eq!(snapshot.ts_utc, "2026-09-08T00:00:00Z");
        assert_eq!(snapshot.roadmap, "demo-roadmap");
        assert_eq!(snapshot.git_sha.as_deref(), Some("abc1234"));
        assert!(snapshot.escalations.is_empty());

        let value = serde_json::to_value(&snapshot).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["discovery", "escalations", "git_sha", "roadmap", "ts_utc"]
        );
    }

    #[test]
    fn build_raw_snapshot_reads_escalations_jsonl_directly() {
        let root = fixture_root();
        let roadmap_dir = root.path().join("planning/roadmaps/demo-roadmap");
        fs::create_dir_all(&roadmap_dir).unwrap();
        write_lane_log(&roadmap_dir, &[]);
        fs::write(
            roadmap_dir.join("escalations.jsonl"),
            "{\"gate_id\": \"g1\", \"kind\": \"advisory\"}\n",
        )
        .unwrap();

        let now = Utc::now();
        let snapshot = build_raw_snapshot_with(root.path(), "demo-roadmap", now, |_| None).unwrap();
        assert_eq!(snapshot.escalations.len(), 1);
        assert_eq!(snapshot.escalations[0]["gate_id"], "g1");
    }

    /// A snapshot our own `build_raw_snapshot` produces parses as `Typed` (not `Legacy`) under
    /// `okf_core::coord::sweep_snapshot::SweepSnapshot` (`OK.6.A`, this block's own dependency) —
    /// the compatibility contract that type exists to check, even though `OK.6.A`'s modeled
    /// shape deliberately omits several sections (leases/lane_registry/carryover/operator_gates/
    /// message_queue — see that module's own doc comment) that this task's full-fidelity
    /// [`RawSnapshot`] still carries.
    #[test]
    fn raw_snapshot_json_parses_as_typed_under_the_ok_6_a_sweep_snapshot_type() {
        let root = fixture_root();
        let roadmap_dir = root.path().join("planning/roadmaps/demo-roadmap");
        fs::create_dir_all(&roadmap_dir).unwrap();
        write_lane_log(&roadmap_dir, &[]);

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot =
            build_raw_snapshot_with(root.path(), "demo-roadmap", now, |_| Some("abc1234".into()))
                .unwrap();
        let json = serde_json::to_string(&snapshot).unwrap();

        let parsed: okf_core::SweepSnapshot = serde_json::from_str(&json).unwrap();
        assert!(
            !parsed.is_legacy(),
            "expected Typed, got Legacy: {parsed:?}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // load_snapshot round-trip
    // -----------------------------------------------------------------------------------------

    #[test]
    fn load_snapshot_round_trips_a_written_snapshot() {
        let root = fixture_root();
        let roadmap_dir = root.path().join("planning/roadmaps/demo-roadmap");
        fs::create_dir_all(&roadmap_dir).unwrap();
        write_lane_log(&roadmap_dir, &[]);

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snapshot =
            build_raw_snapshot_with(root.path(), "demo-roadmap", now, |_| Some("abc1234".into()))
                .unwrap();

        let sweeps = root.path().join("sweeps.json");
        fs::write(&sweeps, serde_json::to_string(&snapshot).unwrap()).unwrap();

        let loaded = load_snapshot(&sweeps).expect("must load a snapshot we just wrote");
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn load_snapshot_tolerates_extra_diff_and_routed_sections() {
        // A real on-disk file (after tasks 3/4 run) carries `diff`/`routed` alongside the raw
        // fields this task writes — loading as `RawSnapshot` must ignore them, not error.
        let root = fixture_root();
        let path = root.path().join("with-extra.json");
        fs::write(
            &path,
            serde_json::json!({
                "ts_utc": "2026-08-28T01:30:33Z",
                "roadmap": "demo-roadmap",
                "git_sha": "87268fc5",
                "discovery": {
                    "roadmap": "demo-roadmap",
                    "roadmap_dir": "/tmp/demo-roadmap",
                    "lanes": {},
                    "repos_in_lane_log": [],
                    "repos_with_run_record_only": [],
                    "operator_coverage_total": 0,
                    "coverage_caveat": "",
                    "validate_brain": {"cmd": "x", "exit_code": null, "error": null},
                    "malformed_lines": []
                },
                "escalations": [],
                "diff": {"changed": false},
                "routed": []
            })
            .to_string(),
        )
        .unwrap();

        let loaded = load_snapshot(&path).expect("extra sections must not fail the load");
        assert_eq!(loaded.roadmap, "demo-roadmap");
    }

    #[test]
    fn load_snapshot_returns_none_for_malformed_json() {
        let root = fixture_root();
        let path = root.path().join("bad.json");
        fs::write(&path, "not json").unwrap();
        assert!(load_snapshot(&path).is_none());
    }

    // -----------------------------------------------------------------------------------------
    // semantic_projection — drops volatile fields, keeps bucketed `stale`, stably sorted
    // -----------------------------------------------------------------------------------------

    fn base_discovery() -> RoadmapStatusResult {
        RoadmapStatusResult {
            roadmap: "demo-roadmap".to_string(),
            roadmap_dir: PathBuf::from("/tmp/demo-roadmap"),
            lanes: BTreeMap::new(),
            repos_in_lane_log: vec!["b-repo".to_string(), "a-repo".to_string()],
            repos_with_run_record_only: vec![],
            operator_coverage_total: 2,
            coverage_caveat: "caveat".to_string(),
            validate_brain: ValidateBrainResult {
                cmd: "bastion validate-brain --state /tmp".to_string(),
                exit_code: Some(0),
                error: None,
            },
            malformed_lines: vec![],
        }
    }

    #[test]
    fn semantic_projection_excludes_pass_stamp_and_sha_and_sorts_repo_lists() {
        let discovery = base_discovery();
        let now = Utc::now();
        let projected = semantic_projection(&discovery, now);

        assert_eq!(projected.roadmap, "demo-roadmap");
        assert_eq!(
            projected.repos_in_lane_log,
            vec!["a-repo".to_string(), "b-repo".to_string()]
        );
        assert_eq!(projected.validate_brain_exit_code, Some(0));

        let value = serde_json::to_value(&projected).unwrap();
        assert!(value.get("git_sha").is_none());
        assert!(value.get("ts_utc").is_none());
    }

    #[test]
    fn semantic_projection_is_idempotent_over_identical_input() {
        let discovery = base_discovery();
        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = semantic_projection(&discovery, now);
        let second = semantic_projection(&discovery, now);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
    }

    #[test]
    fn project_message_queue_buckets_age_into_a_stale_crossing_bool_and_drops_the_raw_age() {
        let mut discovery = base_discovery();
        let mut lane = LaneResult {
            repo: "demo".to_string(),
            message_queue: MessageQueueState {
                queue_dir: Some(PathBuf::from("/tmp/queue")),
                exists: Some(true),
                inbox_count: Some(3),
                oldest_unread_sent_at: Some("2026-09-01T00:00:00Z".to_string()),
                oldest_unread_age_seconds: Some(REGISTRY_STALE_THRESHOLD_SECONDS + 1.0),
                note: None,
            },
            ..LaneResult::default()
        };
        discovery.lanes.insert("demo".to_string(), lane.clone());
        let projected = semantic_projection(&discovery, Utc::now());
        let projected_lane = &projected.lanes["demo"];
        assert_eq!(
            projected_lane
                .message_queue
                .oldest_unread_crossed_stale_threshold,
            Some(true)
        );
        assert_eq!(projected_lane.message_queue.inbox_count, Some(3));

        let value = serde_json::to_value(&projected_lane.message_queue).unwrap();
        assert!(value.get("oldest_unread_age_seconds").is_none());
        assert!(value.get("oldest_unread_sent_at").is_none());

        // A fresh queue does not cross the threshold.
        lane.message_queue.oldest_unread_age_seconds = Some(1.0);
        discovery.lanes.insert("demo".to_string(), lane);
        let projected = semantic_projection(&discovery, Utc::now());
        assert_eq!(
            projected.lanes["demo"]
                .message_queue
                .oldest_unread_crossed_stale_threshold,
            Some(false)
        );
    }

    #[test]
    fn stable_sort_makes_a_reordered_carryover_list_read_as_no_change() {
        let mut discovery_a = base_discovery();
        let mut lane_a = LaneResult {
            repo: "demo".to_string(),
            carryover: vec![
                serde_json::json!({"finding_id": "F2"}),
                serde_json::json!({"finding_id": "F1"}),
            ],
            ..LaneResult::default()
        };
        discovery_a.lanes.insert("demo".to_string(), lane_a.clone());

        let mut discovery_b = base_discovery();
        lane_a.carryover.reverse();
        discovery_b.lanes.insert("demo".to_string(), lane_a);

        let now = Utc::now();
        let projected_a = semantic_projection(&discovery_a, now);
        let projected_b = semantic_projection(&discovery_b, now);
        assert_eq!(
            serde_json::to_string(&projected_a.lanes["demo"].carryover).unwrap(),
            serde_json::to_string(&projected_b.lanes["demo"].carryover).unwrap()
        );
    }

    #[test]
    fn project_registry_entry_computes_stale_from_heartbeat_age_and_drops_raw_heartbeat() {
        let mut discovery = base_discovery();
        let claim = okf_core::RegistryClaim {
            agent_name: "engine-rs-1".to_string(),
            repo: "engine-rs".to_string(),
            lane: "engine-rs".to_string(),
            roadmap: "demo-roadmap".to_string(),
            started_at: "2026-09-08T00:00:00Z".to_string(),
            heartbeat: "2026-09-08T00:00:00Z".to_string(),
            current_block: Some("EN.15.E".to_string()),
            block_started_at: None,
            host: None,
        };
        let lane = LaneResult {
            repo: "demo".to_string(),
            lane_registry: vec![coordination::RegistryEntry {
                path: PathBuf::from("/tmp/reg.json"),
                claim: Coord::Typed(claim),
            }],
            ..LaneResult::default()
        };
        discovery.lanes.insert("demo".to_string(), lane);

        // Far in the future: the heartbeat is well past REGISTRY_STALE_THRESHOLD_SECONDS old.
        let now = DateTime::parse_from_rfc3339("2026-09-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let projected = semantic_projection(&discovery, now);
        let entry = &projected.lanes["demo"].lane_registry[0];
        assert_eq!(entry.agent_name.as_deref(), Some("engine-rs-1"));
        assert_eq!(entry.current_block.as_deref(), Some("EN.15.E"));
        assert_eq!(entry.stale, Some(true));
        assert!(entry.error.is_none());

        let value = serde_json::to_value(entry).unwrap();
        assert!(value.get("heartbeat").is_none());
        assert!(value.get("heartbeat_age_seconds").is_none());
    }

    #[test]
    fn project_lease_reads_holder_from_the_agent_field_and_computes_stale() {
        let mut discovery = base_discovery();
        let record = okf_core::LeaseRecord {
            repo: "engine-rs".to_string(),
            lane: "engine-rs".to_string(),
            agent: "engine-rs-1".to_string(),
            acquired_at: "2026-09-08T00:00:00Z".to_string(),
            kind: okf_core::LeaseKind::Exclusive,
            heartbeat: Some("2026-09-08T00:00:00Z".to_string()),
            scope: Some(okf_core::LeaseScope::Repo),
            host: None,
        };
        let lane = LaneResult {
            repo: "demo".to_string(),
            leases: vec![coordination::LeaseEntry {
                path: PathBuf::from("/tmp/lease.json"),
                lease: Coord::Typed(record),
            }],
            ..LaneResult::default()
        };
        discovery.lanes.insert("demo".to_string(), lane);

        let now = DateTime::parse_from_rfc3339("2026-09-08T00:00:01Z")
            .unwrap()
            .with_timezone(&Utc);
        let projected = semantic_projection(&discovery, now);
        let entry = &projected.lanes["demo"].leases[0];
        assert_eq!(entry.holder.as_deref(), Some("engine-rs-1"));
        assert_eq!(entry.stale, Some(false));
    }

    #[test]
    fn project_registry_entry_reports_legacy_shapes_via_the_error_field_never_a_panic() {
        let mut discovery = base_discovery();
        let lane = LaneResult {
            repo: "demo".to_string(),
            lane_registry: vec![coordination::RegistryEntry {
                path: PathBuf::from("/tmp/reg.json"),
                claim: Coord::Legacy(serde_json::json!({"agent_name": "old-shape"})),
            }],
            ..LaneResult::default()
        };
        discovery.lanes.insert("demo".to_string(), lane);
        let projected = semantic_projection(&discovery, Utc::now());
        let entry = &projected.lanes["demo"].lane_registry[0];
        assert_eq!(entry.agent_name.as_deref(), Some("old-shape"));
        assert!(entry.error.is_some());
        assert!(entry.stale.is_none());
    }
}
